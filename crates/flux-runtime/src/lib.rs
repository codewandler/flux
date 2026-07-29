//! `flux-runtime` — the mandatory safety envelope around tool execution.
//!
//! Every tool call goes through [`Executor::dispatch`]: permission-rule check → (if unmatched)
//! approval prompt → execute through the guarded [`System`](flux_system::System). There is no
//! path to IO that skips this. Tools declare their permission *subjects* and pre-execution
//! *intents*; the dispatcher gates on them and redacts secrets from any error surfaced.

mod perm;
pub use perm::{Pattern, PermDecision, PermissionManager};

mod approval;
pub use approval::{RiskApprover, DEFAULT_CONSENT_MARKER};

mod fn_tool;
pub use fn_tool::{tool_fn, FnTool};

pub mod context;
pub mod metadata;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use flux_core::{Error, OperationTiming, Result, Usage};
use flux_evidence::{
    DestructiveEscalation, EvidenceLog, Observation, Phase, Reaction, KIND_DESTRUCTIVE,
};
use flux_policy::{
    default_local_grants, evaluate, local_identity, Action, AuthorizationPolicy, Caller,
    CallerKind as PolicyCallerKind, Decision, Request as PolicyRequest, ResourceKind, ResourceRef,
    Trust,
};
use flux_secret::Redactor;
use flux_spec::{AccessKind, Effect, Idempotency, IntentSet, Risk, StagingDisposition, ToolSpec};
use flux_system::{PathAccess, System};

/// The result of executing a tool.
///
/// A result has **two faces**. `content` is the *canonical* value: it is what gets bound to a session
/// symbol, spliced into `{{symbol}}` interpolations, and used for `when`/`return` truthiness — i.e.
/// what deterministic execution works with. `view` is an optional *LLM-facing* rendering shown to the
/// model (and the user) — e.g. a line-numbered file, or a status line with a unified diff appended.
/// When `view` is `None` the model sees `content`. Keeping them separate lets a `read` return raw
/// bytes (clean to interpolate) while showing the model a numbered view, and lets `edit`/`write`
/// attach a diff without polluting the canonical value.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub view: Option<String>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            view: None,
            is_error: false,
        }
    }

    /// An OK result whose model-facing `view` differs from the canonical `content`.
    pub fn ok_view(content: impl Into<String>, view: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            view: Some(view.into()),
            is_error: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            view: None,
            is_error: true,
        }
    }

    /// Attach (or replace) the model-facing view.
    pub fn with_view(mut self, view: impl Into<String>) -> Self {
        self.view = Some(view.into());
        self
    }

    /// The model-facing rendering: the explicit `view` if set, else the canonical `content`.
    pub fn view(&self) -> &str {
        self.view.as_deref().unwrap_or(&self.content)
    }
}

/// What a sub-agent run produced: its final text plus enough to roll its spend into the parent turn
/// (C-06). `model` is the role's resolved model (whatever `AgentSpec::into_engine` ran it as —
/// the role's own override, or the spawner's default); `usage` is the child's accumulated per-turn
/// tally from [`crate::LoopHost`]'s equivalent on the engine side, `None` when the child billed
/// nothing (e.g. a `mock` sub-agent, or a role whose provider reported no usage). `session_id` is
/// the child's own session in whatever store the spawner ran it against — under a shared audit
/// store (A-08) that's the durable, correlated child stream; `tool_calls` is a cheap trace count
/// for the parent's `subagent.trace` observation.
#[derive(Debug, Clone, Default)]
pub struct SpawnOutcome {
    pub text: String,
    pub model: String,
    pub usage: Option<flux_core::Usage>,
    pub session_id: String,
    pub tool_calls: usize,
}

/// One live, correlated event from a spawned sub-agent. Every event carries the child identity so
/// a host can safely keep same-named operations from concurrent/nested children separate. The
/// event stream deliberately has no child text or thinking variants: final prose stays in
/// [`SpawnOutcome::text`] and private reasoning stays private.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnActivity {
    /// Process-local unique id for this spawn. Unlike an ephemeral event store's session id
    /// (`s_1` in every fresh store), this remains distinct across concurrent storeless children.
    pub spawn_id: u64,
    pub role: String,
    pub child_session_id: String,
    pub parent_session: Option<String>,
    pub depth: usize,
    pub event: SpawnActivityEvent,
}

/// The activity a spawned child may report to its parent. Tool result *content* is intentionally
/// absent; a customer-facing surface must still default-deny the tool input and observation data
/// carried by this trusted host-side contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpawnActivityEvent {
    Planning {
        active: bool,
    },
    ToolCall {
        call_id: u64,
        name: String,
        input: Value,
    },
    ToolTiming {
        call_id: u64,
        name: String,
        timing: OperationTiming,
    },
    ToolResult {
        call_id: u64,
        name: String,
        is_error: bool,
    },
    Observation {
        observation: Observation,
    },
    Finished {
        usage: Option<flux_core::Usage>,
        /// Whether the child failed, timed out, or was cancelled. No error text crosses this
        /// boundary; the parent receives only the terminal outcome bit.
        is_error: bool,
    },
}

/// Observation kind used to bridge typed child activity through the existing L3 `AgentSink`
/// extension point. The observation is live-only; hosts must treat its data as internal and
/// project it default-deny before exposing it to a customer.
pub const KIND_SUBAGENT_ACTIVITY: &str = "subagent.activity";

impl SpawnActivity {
    /// Encode this typed event as the observation shape existing `AgentSink` implementations can
    /// consume without adding an unscoped child callback to every surface.
    pub fn to_observation(&self) -> Observation {
        Observation::new(
            KIND_SUBAGENT_ACTIVITY,
            Phase::ToolFollowup,
            serde_json::to_value(self).unwrap_or(Value::Null),
        )
    }

    /// Decode a live child-activity observation; unrelated/malformed observations return `None`.
    pub fn from_observation(observation: &Observation) -> Option<Self> {
        (observation.kind == KIND_SUBAGENT_ACTIVITY)
            .then(|| serde_json::from_value(observation.data.clone()).ok())
            .flatten()
    }
}

/// Synchronous, send-only reporter for [`SpawnActivity`]. Defined in L2 so [`Spawner`] can accept
/// it without depending on the L3 agent-loop [`AgentSink`](https://docs.rs/codewandler-flux-flow).
/// Implementations must not hold a lock across an await; the engine adapter only enqueues events.
pub trait SpawnActivitySink: Send + Sync {
    fn emit(&self, activity: SpawnActivity);
}

/// One line of output from a tool that is **still running** (C-158).
///
/// Deliberately a separate channel from [`SpawnActivity`] rather than a content field added to
/// [`SpawnActivityEvent`]. That type carries a spawned *child agent's* activity across a trust
/// boundary and documents that result content is intentionally absent; widening it would loosen a
/// boundary for every sub-agent consumer in order to serve a local `bash` card. This channel makes
/// the narrower claim: content from a tool **this** agent invoked directly, already redacted by
/// [`ToolContext::progress_reporter`], for display only. Nothing here is fed back to the model —
/// the model still sees exactly one thing, the final [`ToolResult`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolProgress {
    /// The op name, matching the `tool_call` that opened the card.
    pub tool: String,
    /// One complete, already-redacted output line.
    pub line: String,
}

/// Observation kind carrying [`ToolProgress`] through the existing `AgentSink::observation`
/// extension point, so no surface has to grow a new callback to ignore.
pub const KIND_TOOL_PROGRESS: &str = "tool.progress";

impl ToolProgress {
    pub fn to_observation(&self) -> Observation {
        Observation::new(
            KIND_TOOL_PROGRESS,
            Phase::ToolFollowup,
            serde_json::to_value(self).unwrap_or(Value::Null),
        )
    }

    /// Decode a live tool-progress observation; unrelated/malformed observations return `None`.
    pub fn from_observation(observation: &Observation) -> Option<Self> {
        (observation.kind == KIND_TOOL_PROGRESS)
            .then(|| serde_json::from_value(observation.data.clone()).ok())
            .flatten()
    }
}

/// Synchronous, send-only reporter for [`ToolProgress`], mirroring [`SpawnActivitySink`].
/// Implementations must not hold a lock across an await, and must not block: this is called from
/// the pipe-drain task of a running child, so a slow sink stalls the child it is reporting on.
pub trait ToolProgressSink: Send + Sync {
    fn emit(&self, progress: ToolProgress);
}

/// An owned, `'static` handle a tool uses to report its own in-flight output.
///
/// Tools never touch a [`ToolProgressSink`] directly, and this is the only way to reach one: every
/// line goes through the same [`Redactor`] the final result does, so there is no path by which a
/// tool can put unredacted content on a surface. Obtained from
/// [`ToolContext::progress_reporter`], which returns `None` when no host installed a sink — a tool
/// then simply runs without reporting.
#[derive(Clone)]
pub struct ToolProgressReporter {
    tool: String,
    redactor: Redactor,
    sink: Arc<dyn ToolProgressSink>,
}

impl ToolProgressReporter {
    /// Redact `line` and hand it to the installed sink.
    pub fn report(&self, line: &str) {
        self.sink.emit(ToolProgress {
            tool: self.tool.clone(),
            line: self.redactor.redact(line),
        });
    }
}

impl std::fmt::Debug for ToolProgressReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolProgressReporter")
            .field("tool", &self.tool)
            .finish_non_exhaustive()
    }
}

/// Where a pane asks to sit (C-220). The model **proposes** a role; the surface resolves, demotes
/// or suppresses it. Deliberately not geometry: a slot names *what the region is for*, never how
/// wide it is or where it starts, so the surface stays free to ignore it entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneSlot {
    Left,
    Right,
    Bottom,
    Overlay,
}

/// The renderer a pane asks for, from a **closed** set. The model picks a shape for its content;
/// it cannot supply a renderer, a widget or a style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneKind {
    Rows,
    Kv,
    Log,
    Progress,
    Tree,
    Markdown,
}

/// How long a pane survives. Mirrors `op.register`'s scope ladder so the model learns one lifetime
/// vocabulary rather than two.
///
/// [`PaneLifetime::Project`] parses but is **rejected by [`SurfaceReporter::send`]**: cross-session
/// panes imply an on-disk pane store that no story has built yet. The value exists so the wire
/// vocabulary is stable and a future story adds behaviour rather than a variant; until then a
/// caller gets a clear error instead of a pane that silently fails to come back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneLifetime {
    Turn,
    Session,
    Project,
}

/// One node of a [`PaneData::Tree`] payload. Content only — indentation, guides and collapse state
/// belong to the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneNode {
    /// The node's text, redacted with the rest of the payload before it reaches a sink.
    pub label: String,
    /// Nested nodes, empty for a leaf. Depth is the model's; the renderer owns how it is drawn.
    #[serde(default)]
    pub children: Vec<PaneNode>,
}

/// The typed payload behind a [`PaneKind`], carrying **content and nothing else**.
///
/// Every variant is text and structure: there is no colour, width, rect or z-order field anywhere
/// in this type, and adding one would hand the model the ability to paint a region that imitates
/// the approval sheet. That is the trust property C-222 rests on, and it is pinned by test rather
/// than by convention. Column widths, wrapping, glyphs, tint and placement are surface-owned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneData {
    Rows {
        #[serde(default)]
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Kv {
        pairs: Vec<(String, String)>,
    },
    Log {
        lines: Vec<String>,
    },
    Progress {
        label: String,
        done: u64,
        total: u64,
    },
    Tree {
        roots: Vec<PaneNode>,
    },
    Markdown {
        text: String,
    },
}

impl PaneData {
    /// The [`PaneKind`] this payload can be rendered as. Keeps [`PaneSpec::new`] from ever
    /// producing a spec whose declared kind disagrees with its data.
    pub fn kind(&self) -> PaneKind {
        match self {
            PaneData::Rows { .. } => PaneKind::Rows,
            PaneData::Kv { .. } => PaneKind::Kv,
            PaneData::Log { .. } => PaneKind::Log,
            PaneData::Progress { .. } => PaneKind::Progress,
            PaneData::Tree { .. } => PaneKind::Tree,
            PaneData::Markdown { .. } => PaneKind::Markdown,
        }
    }

    /// Every string in this payload, run through `redactor`. Applied by [`SurfaceReporter`], which
    /// is the only thing that can reach a [`SurfaceSink`].
    fn redacted(self, redactor: &Redactor) -> Self {
        fn scrub(redactor: &Redactor, nodes: Vec<PaneNode>) -> Vec<PaneNode> {
            nodes
                .into_iter()
                .map(|node| PaneNode {
                    label: redactor.redact(&node.label),
                    children: scrub(redactor, node.children),
                })
                .collect()
        }
        let line = |s: String| redactor.redact(&s);
        match self {
            PaneData::Rows { header, rows } => PaneData::Rows {
                header: header.into_iter().map(line).collect(),
                rows: rows
                    .into_iter()
                    .map(|row| row.into_iter().map(line).collect())
                    .collect(),
            },
            PaneData::Kv { pairs } => PaneData::Kv {
                pairs: pairs.into_iter().map(|(k, v)| (line(k), line(v))).collect(),
            },
            PaneData::Log { lines } => PaneData::Log {
                lines: lines.into_iter().map(line).collect(),
            },
            PaneData::Progress { label, done, total } => PaneData::Progress {
                label: line(label),
                done,
                total,
            },
            PaneData::Tree { roots } => PaneData::Tree {
                roots: scrub(redactor, roots),
            },
            PaneData::Markdown { text } => PaneData::Markdown { text: line(text) },
        }
    }
}

/// A pane the model asks the surface to open.
///
/// The field list is the whole vocabulary: `id`, `title`, `slot`, `kind`, `lifetime`, `data`.
/// **Nothing here reaches a `Style`** — no colour, no width, no rect, no z-order — because a model
/// that can style a region inside a trusted terminal is a model that can imitate the approval
/// sheet. Trust chrome (border, mark, placement) is surface-owned and therefore unforgeable: the
/// model has no field to write it into. See `docs/designs/agent-authored-surface.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneSpec {
    /// Model-chosen handle, used to address later `update`/`close` commands at this pane.
    pub id: String,
    /// The pane's heading. Content only — the surface owns how (and whether) it is emphasized.
    pub title: String,
    /// Where the pane asks to sit. A proposal: the surface may resolve, demote or suppress it.
    pub slot: PaneSlot,
    /// Which renderer to use. Must agree with `data`; [`SurfaceReporter::send`] rejects a mismatch.
    pub kind: PaneKind,
    /// How long the pane survives. `project` parses but is rejected at the reporter.
    pub lifetime: PaneLifetime,
    /// The pane's content, typed per [`PaneKind`].
    pub data: PaneData,
}

impl PaneSpec {
    /// A spec whose `kind` is taken from `data`, so the two cannot disagree. Deserialized specs
    /// carry both independently — [`SurfaceReporter::send`] rejects a mismatch there.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        slot: PaneSlot,
        lifetime: PaneLifetime,
        data: PaneData,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            slot,
            kind: data.kind(),
            lifetime,
            data,
        }
    }
}

/// One instruction from a tool to the human surface. `list` is deliberately absent: this channel is
/// send-only, so reading back what is open is a surface-side query, not a sink command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum PaneCommand {
    Open(PaneSpec),
    Update { id: String, data: PaneData },
    Close { id: String },
}

impl PaneCommand {
    /// This command with every model-supplied string redacted. `id` is scrubbed too — redaction is
    /// deterministic, so an `open`/`update`/`close` triple still addresses the same pane.
    fn redacted(self, redactor: &Redactor) -> Self {
        match self {
            PaneCommand::Open(spec) => PaneCommand::Open(PaneSpec {
                id: redactor.redact(&spec.id),
                title: redactor.redact(&spec.title),
                slot: spec.slot,
                kind: spec.kind,
                lifetime: spec.lifetime,
                data: spec.data.redacted(redactor),
            }),
            PaneCommand::Update { id, data } => PaneCommand::Update {
                id: redactor.redact(&id),
                data: data.redacted(redactor),
            },
            PaneCommand::Close { id } => PaneCommand::Close {
                id: redactor.redact(&id),
            },
        }
    }
}

/// Synchronous, send-only channel from a tool to the human surface, mirroring [`ToolProgressSink`]
/// and [`SpawnActivitySink`]. Defined at L2 so a tool can address the surface without any crate
/// below L6 knowing a surface exists.
///
/// Implementations must not hold a lock across an await, and must not block: this is called from
/// inside a running tool, so a slow sink stalls the work it is describing. Enqueue and return.
pub trait SurfaceSink: Send + Sync {
    fn emit(&self, command: PaneCommand);
}

/// An owned, `'static` handle a tool uses to address the human surface.
///
/// Tools never touch a [`SurfaceSink`] directly, and this is the only way to reach one: every
/// command goes through the same [`Redactor`] the final result does, so there is no path by which
/// a tool can put unredacted content on a surface. Obtained from [`ToolContext::surface`], which
/// returns `None` when no host installed a sink — a headless run then gets a clear failure rather
/// than a silent no-op the model would read as success.
#[derive(Clone)]
pub struct SurfaceReporter {
    redactor: Redactor,
    sink: Arc<dyn SurfaceSink>,
}

impl SurfaceReporter {
    /// Validate `command`, redact it, and hand it to the installed sink.
    ///
    /// Two rejections, both cheap and both about keeping the contract honest rather than about
    /// policy: [`PaneLifetime::Project`] has no implementation yet, and a deserialized spec whose
    /// `kind` disagrees with its `data` would leave the surface choosing which one to believe.
    pub fn send(&self, command: PaneCommand) -> Result<()> {
        if let PaneCommand::Open(spec) = &command {
            if spec.lifetime == PaneLifetime::Project {
                return Err(Error::Other(format!(
                    "pane '{}': lifetime 'project' is not supported yet — panes persist for 'turn' \
                     or 'session' only",
                    spec.id
                )));
            }
            if spec.kind != spec.data.kind() {
                return Err(Error::Other(format!(
                    "pane '{}': declared kind {:?} does not match its data ({:?})",
                    spec.id,
                    spec.kind,
                    spec.data.kind()
                )));
            }
        }
        self.sink.emit(command.redacted(&self.redactor));
        Ok(())
    }
}

impl std::fmt::Debug for SurfaceReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceReporter").finish_non_exhaustive()
    }
}

/// Grace allowed for turn-owned sub-agent tasks to observe parent cancellation and durably
/// finalize before the runtime aborts them. The child engine's cancellation path normally closes
/// immediately; this bound exists for buggy or cancellation-insensitive [`Spawner`] implementations.
pub const SPAWN_CLEANUP_GRACE: Duration = Duration::from_secs(10);

/// Owns Tokio tasks started by `task` operations for one lexical turn.
///
/// Dropping the operation future only drops its [`tokio::task::JoinHandle`], which would otherwise
/// detach the sub-agent. The supervisor keeps an abort handle until the task actually exits, letting
/// the turn finalizer first await cooperative cancellation and only abort after
/// [`SPAWN_CLEANUP_GRACE`]. A fresh instance is installed by each engine turn; nested child engines
/// therefore supervise their own descendants transitively.
pub struct SpawnTaskSupervisor {
    next_id: AtomicU64,
    tasks: Mutex<HashMap<u64, Option<tokio::task::AbortHandle>>>,
    idle: tokio::sync::Notify,
    cancel: tokio_util::sync::CancellationToken,
}

impl Default for SpawnTaskSupervisor {
    fn default() -> Self {
        Self::with_cancel(tokio_util::sync::CancellationToken::new())
    }
}

impl SpawnTaskSupervisor {
    /// Create a turn owner whose descendants receive `cancel` as their cooperative stop signal.
    pub fn with_cancel(cancel: tokio_util::sync::CancellationToken) -> Self {
        Self {
            next_id: AtomicU64::new(0),
            tasks: Mutex::new(HashMap::new()),
            idle: tokio::sync::Notify::new(),
            cancel,
        }
    }

    /// A child token for one supervised spawn. Cancelling the turn owner stops every token derived
    /// here while keeping sibling cancellation scopes independent.
    pub fn child_token(&self) -> tokio_util::sync::CancellationToken {
        self.cancel.child_token()
    }

    /// Spawn and register one turn-owned task. The returned handle is awaited by the operation on
    /// the normal path; the supervisor remains its owner if that operation future is cancelled.
    pub fn spawn<F, T>(self: &Arc<Self>, future: F) -> tokio::task::JoinHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.tasks.lock().unwrap().insert(id, None);
        let supervisor = self.clone();
        let handle = tokio::spawn(async move {
            let _task = SpawnTaskGuard { id, supervisor };
            future.await
        });
        let abort = handle.abort_handle();
        if let Some(slot) = self.tasks.lock().unwrap().get_mut(&id) {
            *slot = Some(abort);
        }
        handle
    }

    async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            if self.tasks.lock().unwrap().is_empty() {
                return;
            }
            notified.await;
        }
    }

    /// Wait for all children to finish cooperatively. If `grace` elapses, abort every remaining
    /// task and wait for the abort drops to reap their registrations. Returns `true` when cleanup
    /// was cooperative and `false` when the abort backstop was needed.
    pub async fn shutdown(&self, grace: Duration) -> bool {
        self.cancel.cancel();
        if tokio::time::timeout(grace, self.wait_idle()).await.is_ok() {
            return true;
        }
        let aborts = self
            .tasks
            .lock()
            .unwrap()
            .values()
            .filter_map(Clone::clone)
            .collect::<Vec<_>>();
        for abort in aborts {
            abort.abort();
        }
        // Tokio runs a task's destructors when an abort is observed. Keep the parent turn alive
        // until those drops have removed every registration; a second bound prevents a broken
        // executor from turning the abort backstop into an unbounded wait.
        let _ = tokio::time::timeout(grace, self.wait_idle()).await;
        false
    }

    /// Whether no turn-owned sub-agent task remains. Primarily useful for diagnostics/tests; turn
    /// finalization should call [`Self::shutdown`] so the empty state is actively enforced.
    pub fn is_idle(&self) -> bool {
        self.tasks.lock().unwrap().is_empty()
    }
}

struct SpawnTaskGuard {
    id: u64,
    supervisor: Arc<SpawnTaskSupervisor>,
}

impl Drop for SpawnTaskGuard {
    fn drop(&mut self) {
        let became_idle = {
            let mut tasks = self.supervisor.tasks.lock().unwrap();
            tasks.remove(&self.id);
            tasks.is_empty()
        };
        if became_idle {
            self.supervisor.idle.notify_waiters();
        }
    }
}

/// Lexically scoped capabilities belonging to one live runtime turn. A conversational driver
/// installs this around the future that actually drives the turn; guarded adapter tools and nested
/// runtimes can then inherit cancellation, parent-session lineage, child-activity reporting, and
/// turn-owned sub-agent supervision without reading process-global or long-lived mutable slots.
///
/// The scope is future-local: concurrent Tokio tasks do not exchange it, a nested scope restores
/// its parent on exit (including cancellation/drop), and a context retained after the scope ends
/// cannot observe obsolete turn state. A spawned Tokio task does not inherit task-locals; callers
/// that deliberately cross that boundary (such as `FlowClient::execute_streamed`) must snapshot and
/// pin this value onto their fresh [`ToolContext`] before spawning.
#[derive(Clone, Default)]
pub struct RuntimeTurnContext {
    cancel: Option<tokio_util::sync::CancellationToken>,
    session: Option<String>,
    spawn_activity: Option<Arc<dyn SpawnActivitySink>>,
    spawn_supervisor: Option<Arc<SpawnTaskSupervisor>>,
    identity: Option<TurnIdentity>,
    tool_progress: Option<Arc<dyn ToolProgressSink>>,
    surface: Option<Arc<dyn SurfaceSink>>,
}

/// The caller and trust assertion frozen for one runtime turn.
///
/// Long-lived executors retain an immutable assembly-time fallback identity. Multi-principal
/// surfaces install this value lexically through an engine's per-turn entry point after the engine
/// acquires its single-active-turn gate. Policy checks, approval receipts, guarded tool calls, and
/// spawned children in that turn therefore observe one immutable snapshot.
#[derive(Clone, Debug)]
pub struct TurnIdentity {
    caller: Caller,
    trust: Trust,
}

impl TurnIdentity {
    pub fn new(caller: Caller, trust: Trust) -> Self {
        Self { caller, trust }
    }

    pub fn caller(&self) -> &Caller {
        &self.caller
    }

    pub fn trust(&self) -> &Trust {
        &self.trust
    }

    pub fn into_parts(self) -> (Caller, Trust) {
        (self.caller, self.trust)
    }
}

impl RuntimeTurnContext {
    /// An explicitly empty turn context. When scoped, its absent fields are authoritative: stale
    /// fallback slots on a long-lived [`ToolContext`] stay hidden for the scope's lifetime.
    pub fn new() -> Self {
        Self::default()
    }

    /// Carry the cancellation token of the live parent turn.
    pub fn with_cancel(mut self, cancel: tokio_util::sync::CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Carry the live parent turn's session id for child-stream correlation.
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    /// Carry the live parent turn's child-activity reporter.
    pub fn with_spawn_activity_sink(mut self, sink: Arc<dyn SpawnActivitySink>) -> Self {
        self.spawn_activity = Some(sink);
        self
    }

    /// Carry the surface channel a running tool reports its own in-flight output on (C-158).
    pub fn with_tool_progress_sink(mut self, sink: Arc<dyn ToolProgressSink>) -> Self {
        self.tool_progress = Some(sink);
        self
    }

    /// Carry the pane channel a tool addresses the human surface on (C-220).
    pub fn with_surface_sink(mut self, sink: Arc<dyn SurfaceSink>) -> Self {
        self.surface = Some(sink);
        self
    }

    /// Carry the owner for sub-agent tasks started during this turn.
    pub fn with_spawn_supervisor(mut self, supervisor: Arc<SpawnTaskSupervisor>) -> Self {
        self.spawn_supervisor = Some(supervisor);
        self
    }

    /// Freeze the authorization identity for this lexical turn.
    pub fn with_identity(mut self, identity: TurnIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// The turn's cancellation token, when driven by a cancellable surface.
    pub fn cancel_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        self.cancel.clone()
    }

    /// The turn's session id, when this runtime is part of a parent conversation.
    pub fn session_id(&self) -> Option<String> {
        self.session.clone()
    }

    /// The turn's live child-activity reporter, when one is attached.
    pub fn spawn_activity_sink(&self) -> Option<Arc<dyn SpawnActivitySink>> {
        self.spawn_activity.clone()
    }

    /// The turn-owned sub-agent task supervisor, when an engine installed one.
    pub fn spawn_supervisor(&self) -> Option<Arc<SpawnTaskSupervisor>> {
        self.spawn_supervisor.clone()
    }

    /// The turn-owned live tool-output channel, when a surface installed one.
    pub fn tool_progress_sink(&self) -> Option<Arc<dyn ToolProgressSink>> {
        self.tool_progress.clone()
    }

    /// The turn-owned pane channel, when a surface installed one.
    ///
    /// `pub(crate)` on purpose, unlike the sibling [`RuntimeTurnContext::tool_progress_sink`]:
    /// [`ToolContext::surface`] claims to be the ONLY way to reach a [`SurfaceSink`], and a public
    /// getter here would make that false — a tool could pair it with
    /// [`ToolContext::runtime_turn_context`] and emit unredacted bytes straight to the screen.
    /// Installing a sink stays public ([`RuntimeTurnContext::with_surface_sink`]) because L6
    /// surfaces must still be able to; only reading one back is crate-private. C-222's
    /// trusted-chrome invariant leans on this being enforced by visibility, not by convention.
    pub(crate) fn surface_sink(&self) -> Option<Arc<dyn SurfaceSink>> {
        self.surface.clone()
    }

    /// The immutable authorization identity carried by this turn, when explicitly installed.
    pub fn identity(&self) -> Option<TurnIdentity> {
        self.identity.clone()
    }

    /// Whether this snapshot carries no live turn capabilities.
    pub fn is_empty(&self) -> bool {
        self.cancel.is_none()
            && self.session.is_none()
            && self.spawn_activity.is_none()
            && self.spawn_supervisor.is_none()
            && self.identity.is_none()
            && self.tool_progress.is_none()
            && self.surface.is_none()
    }
}

impl std::fmt::Debug for RuntimeTurnContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeTurnContext")
            .field("cancel", &self.cancel.is_some())
            .field("session", &self.session)
            .field("spawn_activity", &self.spawn_activity.is_some())
            .field("spawn_supervisor", &self.spawn_supervisor.is_some())
            .field(
                "identity",
                &self
                    .identity
                    .as_ref()
                    .map(|identity| identity.caller().principal.id.as_str()),
            )
            .finish()
    }
}

tokio::task_local! {
    static ACTIVE_RUNTIME_TURN: RuntimeTurnContext;
}

/// Snapshot the currently active lexical turn, if a driver installed one. `Some(empty)` is
/// distinct from `None`: an explicitly empty scope suppresses legacy stored fallback values.
pub fn active_runtime_turn_context() -> Option<RuntimeTurnContext> {
    ACTIVE_RUNTIME_TURN.try_with(Clone::clone).ok()
}

/// Drive `future` inside one lexical runtime-turn scope. Nested scopes restore the previous value
/// automatically, and concurrent tasks remain isolated by Tokio's task-local semantics.
pub async fn scope_runtime_turn<F>(turn: RuntimeTurnContext, future: F) -> F::Output
where
    F: Future,
{
    ACTIVE_RUNTIME_TURN.scope(turn, future).await
}

/// One sub-agent spawn, fully described. `cap_scope` is the caller's active `with_tools`
/// allowlist, if any — the spawner intersects it into the role's own `tools`, so a `task` invoked
/// from inside a capability scope can never hand the child a broader tool set than the block that
/// spawned it (capabilities only narrow on descent). `parent_session`, when known, is recorded as
/// the child session's `correlation_id` so a shared audit store correlates child streams to the
/// turn that spawned them (A-08).
#[derive(Clone, Default)]
pub struct SpawnRequest {
    pub role: String,
    pub task: String,
    pub cap_scope: Option<Vec<String>>,
    pub parent_session: Option<String>,
    /// Live reporter snapshotted from the parent turn. `None` preserves the storeless/one-shot
    /// behavior: the child still returns its final [`SpawnOutcome`], but emits no live activity.
    pub activity: Option<Arc<dyn SpawnActivitySink>>,
    /// Snapshot of the parent context's ACTIVE guarded system at delegation time (C-100). The
    /// spawner seeds the child's own independent [`WorkspaceContext`] from this snapshot, so a
    /// child spawned inside a worktree session (C-97) operates from the transitioned root — but a
    /// child's own enter/leave never affects the parent (and vice versa). `None` falls back to the
    /// spawner's assembly-time system.
    pub system: Option<Arc<System>>,
}

impl std::fmt::Debug for SpawnRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnRequest")
            .field("role", &self.role)
            .field("task", &self.task)
            .field("cap_scope", &self.cap_scope)
            .field("parent_session", &self.parent_session)
            .field("activity", &self.activity.is_some())
            .field(
                "system",
                &self.system.as_ref().map(|s| s.workspace().root()),
            )
            .finish()
    }
}

impl SpawnRequest {
    /// A bare request: no capability scope, no parent correlation.
    pub fn new(role: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            task: task.into(),
            cap_scope: None,
            parent_session: None,
            activity: None,
            system: None,
        }
    }
}

/// Runs a sub-agent (by role name) and returns its outcome. Implemented by `flux-orchestrate`
/// and injected into [`ToolContext`] so a `task` tool can delegate without `flux-runtime`
/// depending on the agent loop. The `cancel` token aborts the sub-agent turn (so autopilot loops
/// and plan-and-dispatch stay interruptible).
#[async_trait]
pub trait Spawner: Send + Sync {
    async fn spawn(
        &self,
        request: SpawnRequest,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> flux_core::Result<SpawnOutcome>;
}

/// Where a **dispatched** run is recorded so it can be re-derived after a restart (A-130).
///
/// `fleet.dispatch` hands work to a remote worker and gets back a worker-minted task id. That id
/// plus the worker's address are the only handles a later reconciliation sweep has, so they must
/// reach durable storage before the dispatch reports success: a task whose id was lost is strictly
/// worse than one never dispatched, because nothing will ever sweep it and it silently consumes a
/// worker.
///
/// The port lives here (L2) for exactly the reason [`Spawner`] does. The recorder is a work board
/// in `flux-capabilities` (L5) and the caller is `flux-orchestrate` (L3); neither may depend on the
/// other, and both already depend on this crate.
#[async_trait]
pub trait DispatchLedger: Send + Sync {
    /// The permission subject one item's record occupies — e.g. `board/item/PROJ-42`.
    ///
    /// Synchronous and infallible because it runs on the **gating** path, before execution: an op
    /// declaring a write must be able to name what it writes. Implementations must return a
    /// concrete subject, never `*` and never empty, so a grant scoped to one item cannot widen.
    fn subject(&self, item: &str) -> String;

    /// Bind `item` to the worker running it. Must be durable by the time this returns — a caller
    /// treats `Ok(())` as "a restarted process will find this run".
    async fn record_dispatch(
        &self,
        ctx: &ToolContext,
        item: &str,
        runner: &str,
        task_id: &str,
    ) -> flux_core::Result<()>;
}

/// Host capabilities used by model-backed stages inside an authored Flux-Lang outer loop. Defined
/// here (L2) so guarded tools can delegate without depending on the L3 engine. Models return typed
/// stage values and provider-native calls; only caller-authored Flux reaches deterministic execution.
#[async_trait]
pub trait LoopHost: Send + Sync {
    /// Record one independent model call made by a guarded operation inside the current turn.
    ///
    /// The callback is synchronous because cancellation may drop the operation future after usage
    /// arrived; a drop guard must be able to publish the already-observed accounting before the
    /// turn finalizes. Hosts that do not own turn accounting (direct one-shot runtimes) keep the
    /// default no-op and can read the operation's evidence observation instead.
    fn record_model_usage(&self, _provider: &str, _model: &str, _usage: Usage) {}

    /// Reserve one call against a guarded operation's own per-turn call budget (A-96's `consult`
    /// op is the first user): returns the ordinal of THIS call within the turn (`0` for the first,
    /// `1` for the second, …), reset every turn like the rest of turn accounting. The caller
    /// compares the ordinal against its own configured cap and refuses once it is reached — this
    /// method only counts, it never itself blocks. Hosts that do not own turn accounting (direct
    /// one-shot runtimes) keep the default `0`, so a single dispatched call can never exceed a cap
    /// of `1` or more on its own.
    fn reserve_consult_call(&self) -> usize {
        0
    }

    /// Detect one turn's intent and resolve the initial capability signals into a durable stage
    /// artifact. Adaptive hosts override this; tool-only runtimes fail clearly.
    async fn detect_intent(&self) -> flux_core::Result<serde_json::Value> {
        Err(flux_core::Error::Other(
            "detect_intent: this host does not provide an adaptive loop".into(),
        ))
    }

    /// Continue native-schema exploration from a typed state artifact. The input may also carry a
    /// resumed user decision or an execution report from the previous action batch.
    async fn explore(&self, _input: serde_json::Value) -> flux_core::Result<serde_json::Value> {
        Err(flux_core::Error::Other(
            "explore: this host does not provide an adaptive loop".into(),
        ))
    }

    /// Ask for aggregate approval and mint an opaque one-shot receipt for one exact action batch.
    async fn approve_batch(
        &self,
        _input: serde_json::Value,
    ) -> flux_core::Result<serde_json::Value> {
        Err(flux_core::Error::Other(
            "approve_batch: this host does not provide an adaptive loop".into(),
        ))
    }

    /// Consume a matching approval receipt and execute the batch through the safety envelope.
    async fn execute_batch(
        &self,
        _input: serde_json::Value,
    ) -> flux_core::Result<serde_json::Value> {
        Err(flux_core::Error::Other(
            "execute_batch: this host does not provide an adaptive loop".into(),
        ))
    }

    /// Turn a terminal adaptive artifact into the channel-neutral answer text.
    async fn present_results(
        &self,
        _input: serde_json::Value,
    ) -> flux_core::Result<serde_json::Value> {
        Err(flux_core::Error::Other(
            "present_results: this host does not provide an adaptive loop".into(),
        ))
    }

    /// Run a named, host-configured model stage through its exact typed operation contract.
    async fn model_stage(
        &self,
        name: &str,
        _input: serde_json::Value,
    ) -> flux_core::Result<serde_json::Value> {
        Err(flux_core::Error::Other(format!(
            "model stage `{name}` is not configured on this host"
        )))
    }

    /// Execute a caller-authored Flux AST in the current session. This is deterministic language
    /// execution, not model planning; hosts revalidate it against the live operation catalog.
    async fn run_authored_flow(
        &self,
        _ast: serde_json::Value,
    ) -> flux_core::Result<serde_json::Value> {
        Err(flux_core::Error::Other(
            "run_authored_flow: this host does not provide Flux execution".into(),
        ))
    }

    /// Hand a bounded run of native-schema model stages to the loop under an exact capability scope,
    /// then return control to the caller. Proposed effects use the same batch approval seam as the
    /// default adaptive loop.
    async fn ai_segment(&self, _input: serde_json::Value) -> flux_core::Result<serde_json::Value> {
        Err(flux_core::Error::Other(
            "ai_segment: this host does not provide adaptive model stages".into(),
        ))
    }
}

/// A request to register a Flux-Lang composite op into a host-managed catalog.
///
/// Defined at the runtime layer so the root `op.register` tool can delegate without depending on
/// `flux-flow`. The engine owns parsing, validation, storage, and catalog mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeRegisterRequest {
    pub source: String,
    pub scope: String,
    #[serde(default)]
    pub replace: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expose: Option<bool>,
}

/// Host capability for registering composite ops. Implemented by the flow engine and injected into
/// [`ToolContext`] for `op.register`; ordinary tool-only dispatch contexts leave it absent.
#[async_trait]
pub trait CompositeRegistrar: Send + Sync {
    async fn register_composite(
        &self,
        request: CompositeRegisterRequest,
    ) -> flux_core::Result<serde_json::Value>;
}

/// The result of loading one skill's body on demand (D-188: opt-in model-invoked progressive skill
/// disclosure). `name` is the catalog entry's canonical name (the caller's request may have been
/// resolved case-sensitively against it); `body` is the full skill markdown body — nothing more is
/// injected than what this call returns.
#[derive(Debug, Clone)]
pub struct SkillLoadOutcome {
    pub name: String,
    pub body: String,
}

/// Host capability for on-demand skill body loading (D-188). Implemented by the flow engine and
/// injected into [`ToolContext`] when a session's skill catalog is non-empty (the opt-in
/// model-invoked mode); `skill.load` fails clearly when absent. A successful load is expected to
/// make the skill behave like an explicitly `--skill`-activated one for the rest of the session —
/// the host, not this trait, owns that persistence.
#[async_trait]
pub trait SkillLoader: Send + Sync {
    async fn load_skill(&self, session_id: &str, name: &str)
        -> flux_core::Result<SkillLoadOutcome>;
}

/// The lifecycle phase of an active worktree session (C-97). `Merged` exists so a `leave` retry
/// after a partial cleanup never re-merges: the merge commit landed, only worktree/branch removal
/// remains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreePhase {
    /// The context is working inside the worktree; nothing has been merged.
    Active,
    /// The merge into the original branch succeeded but worktree/branch cleanup did not complete.
    Merged,
}

/// The state of one context-local worktree transition (C-97): everything `git_worktree_leave`
/// needs to integrate, restore, and clean up — the original guarded system, the commit `main`
/// pointed at on enter, the generated branch, and the allocated paths.
#[derive(Clone)]
pub struct WorktreeSession {
    /// The guarded system the context had before entering; restored on leave.
    pub original: Arc<System>,
    /// The commit `main` pointed at when the worktree was created.
    pub base_commit: String,
    /// The generated `flux/worktree/...` branch the worktree checkout is on.
    pub branch: String,
    /// The worktree checkout directory (inside `parent_dir`).
    pub checkout: std::path::PathBuf,
    /// The allocated private `/tmp/flux-worktree-*` parent directory.
    pub parent_dir: std::path::PathBuf,
    /// Where the session is in its lifecycle.
    pub phase: WorktreePhase,
}

/// The context-local, swappable workspace handle (C-97). Each agent context owns one; it holds the
/// currently active guarded [`System`] plus the optional worktree session state. Cloned contexts
/// share it (so every op in one agent context sees a transition), while a spawned child receives an
/// **independent** `WorkspaceContext` seeded from the parent's active snapshot. Never touches
/// process-global state — there is no `set_current_dir` anywhere on this path.
#[derive(Clone)]
pub struct WorkspaceContext {
    state: Arc<Mutex<WorkspaceState>>,
}

struct WorkspaceState {
    active: Arc<System>,
    session: Option<WorktreeSession>,
}

impl WorkspaceContext {
    pub fn new(system: Arc<System>) -> Self {
        Self {
            state: Arc::new(Mutex::new(WorkspaceState {
                active: system,
                session: None,
            })),
        }
    }

    /// Snapshot the currently active guarded system. Callers hold the snapshot for the duration of
    /// one operation; a transition made meanwhile is observed by the *next* call, not mid-flight.
    pub fn active(&self) -> Arc<System> {
        self.state.lock().unwrap().active.clone()
    }

    /// Snapshot the current worktree session state, if a transition is active.
    pub fn worktree_session(&self) -> Option<WorktreeSession> {
        self.state.lock().unwrap().session.clone()
    }

    /// Transition this context into a worktree: record the session and swap the active system.
    /// Recoverable error if a session is already active — nesting is rejected in v1.
    pub fn enter_worktree(
        &self,
        session: WorktreeSession,
        active: Arc<System>,
    ) -> flux_core::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.session.is_some() {
            return Err(flux_core::Error::Config(
                "a worktree session is already active in this context; run git_worktree_leave first"
                    .into(),
            ));
        }
        state.session = Some(session);
        state.active = active;
        Ok(())
    }

    /// Record that the session's merge landed (cleanup may still be outstanding), so a retried
    /// leave never re-merges.
    pub fn mark_merged(&self) {
        if let Some(session) = self.state.lock().unwrap().session.as_mut() {
            session.phase = WorktreePhase::Merged;
        }
    }

    /// Complete a leave: restore the original system and clear the session state.
    pub fn leave_worktree(&self) -> flux_core::Result<()> {
        let mut state = self.state.lock().unwrap();
        let Some(session) = state.session.take() else {
            return Err(flux_core::Error::Config(
                "no worktree session is active in this context".into(),
            ));
        };
        state.active = session.original;
        Ok(())
    }
}

/// What a tool is given at execution time: the guarded IO surface, the secret redactor, an optional
/// sub-agent spawner, and the per-session read-set (file → mtime at last read) used by the
/// read-before-write guard. The read-set is shared (an `Arc<Mutex<…>>`) so every op in a session sees
/// the same map: a `read` in one node records an mtime an `edit` in a later node checks against.
///
/// The guarded system is reached through [`ToolContext::system`], which snapshots the context-local
/// [`WorkspaceContext`]'s active system — a worktree transition (C-97) swaps what subsequent calls
/// observe without any process-global state change.
#[derive(Clone)]
pub struct ToolContext {
    workspace: WorkspaceContext,
    pub redactor: Redactor,
    pub spawner: Option<Arc<dyn Spawner>>,
    /// D-188: on-demand skill-body loader, installed by the flow engine when the opt-in
    /// model-invoked skill catalog is non-empty for this session. `None` means the mode is off (or
    /// this dispatch context has no engine behind it) — `skill.load` then fails clearly.
    pub skill_loader: Option<Arc<dyn SkillLoader>>,
    /// The authored outer-loop capability, installed per turn by the engine. `None` outside a
    /// model-in-the-loop run — adaptive stage ops then return a clear error rather than silently
    /// doing nothing.
    pub loop_host: Option<Arc<dyn LoopHost>>,
    /// Root op registration capability (`op.register`), installed by a model-in-the-loop engine.
    /// Kept separate from [`LoopHost`] so other hosts can opt into composite registration without
    /// exposing planner/interpreter reentry.
    pub composite_registrar: Option<Arc<dyn CompositeRegistrar>>,
    pub read_times: Arc<Mutex<HashMap<String, std::time::SystemTime>>>,
    /// The append-only evidence log, shared (an `Arc<Mutex<…>>`) so the dispatcher's `tool_call`
    /// markers, externally-recorded observations ([`Executor::observe`]), flow-emitted `observe(…)`
    /// ops, and any sibling run that re-enters this same context all write to **one** audit trail.
    /// Lives here (not Executor-private) so the `observe`/`evidence` ops can read and append to it.
    pub evidence: Arc<Mutex<EvidenceLog>>,
    /// Stored cancellation fallback for legacy/direct drivers and for a fresh nested runtime that
    /// deliberately pins a lexical [`RuntimeTurnContext`] before crossing `tokio::spawn`. Active
    /// conversational turns use the task-local scope, not this long-lived mutable slot.
    cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// Stored session-lineage fallback; see `cancel`. The lexical turn is authoritative even when
    /// its session is `None`, so an empty direct one-shot scope cannot revive stale lineage here.
    session: Arc<Mutex<Option<String>>>,
    /// Stored child-activity fallback; see `cancel`. Ordinary engine turns manufacture a fresh
    /// reporter and carry it only in their lexical [`RuntimeTurnContext`].
    spawn_activity: Arc<Mutex<Option<Arc<dyn SpawnActivitySink>>>>,
    /// Stored live tool-output fallback; see `cancel`. Ordinary engine turns carry the surface's
    /// sink lexically with the rest of [`RuntimeTurnContext`].
    tool_progress: Arc<Mutex<Option<Arc<dyn ToolProgressSink>>>>,
    /// Stored pane-channel fallback; see `cancel`. Ordinary engine turns carry the surface's sink
    /// lexically with the rest of [`RuntimeTurnContext`].
    surface: Arc<Mutex<Option<Arc<dyn SurfaceSink>>>>,
    /// Stored sub-agent supervisor fallback for deliberately pinned spawned runtimes. Ordinary
    /// conversational turns carry it lexically with the rest of [`RuntimeTurnContext`].
    spawn_supervisor: Arc<Mutex<Option<Arc<SpawnTaskSupervisor>>>>,
    /// Immutable identity pinned when a fresh one-shot context deliberately inherits a lexical
    /// turn before crossing a task boundary. Ordinary engines leave this empty and scope identity
    /// lexically instead.
    identity: Option<TurnIdentity>,
    /// The **capability-scope stack**: each entry is the effective tool-name allowlist of one active
    /// `with_tools` block, narrow-only (an entry is always the intersection of its own declared set
    /// with the one below it — see [`Executor::push_cap_scope`]). Empty stack = no scope active = every
    /// tool the policy/permission layers already allow stays allowed (a strict no-op, so flows that
    /// never use `with_tools` are unaffected). Shared (not `Executor`-private) so a spawned sub-agent's
    /// `TaskTool` can read the *parent's* active scope at the moment it delegates — the same `Arc` the
    /// dispatch gate checks, which is what makes the sub-agent intersection non-bypassable too.
    cap_scopes: Arc<Mutex<Vec<Vec<String>>>>,
}

impl ToolContext {
    pub fn new(system: Arc<System>) -> Self {
        Self::over_workspace(WorkspaceContext::new(system))
    }

    /// A context over an **existing** workspace handle (C-122). The session surfaces create the
    /// [`WorkspaceContext`] early — before plugin loading — and hand the same handle to the plugin
    /// host capabilities and to this context, so plugin ops and built-in tools observe the same
    /// worktree transitions. [`ToolContext::new`] mints a fresh handle for everything else.
    pub fn over_workspace(workspace: WorkspaceContext) -> Self {
        Self {
            workspace,
            redactor: Redactor::new(),
            spawner: None,
            skill_loader: None,
            loop_host: None,
            composite_registrar: None,
            read_times: Arc::new(Mutex::new(HashMap::new())),
            evidence: Arc::new(Mutex::new(EvidenceLog::new())),
            cancel: Arc::new(Mutex::new(None)),
            session: Arc::new(Mutex::new(None)),
            spawn_activity: Arc::new(Mutex::new(None)),
            spawn_supervisor: Arc::new(Mutex::new(None)),
            tool_progress: Arc::new(Mutex::new(None)),
            surface: Arc::new(Mutex::new(None)),
            identity: None,
            cap_scopes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Snapshot the currently active guarded system for this context. This is the only way tools
    /// reach IO; the snapshot is stable for the duration of one call, and a worktree transition
    /// (C-97) is observed by the next call.
    pub fn system(&self) -> Arc<System> {
        self.workspace.active()
    }

    /// The context-local workspace handle — the worktree ops drive transitions through this.
    pub fn workspace_context(&self) -> &WorkspaceContext {
        &self.workspace
    }

    /// The effective tool-name allowlist of the innermost active capability scope, if any. `None`
    /// means no scope is active (every tool stays subject only to policy/permission rules). Used by
    /// [`Executor::dispatch`]'s gate and by [`Spawner`] implementations to intersect a sub-agent role's
    /// tools with the block it was invoked from.
    pub fn active_cap_scope(&self) -> Option<Vec<String>> {
        self.cap_scopes.lock().unwrap().last().cloned()
    }

    /// Install a stored cancellation fallback. New turn drivers should use
    /// [`scope_runtime_turn`] with [`RuntimeTurnContext::with_cancel`]; this setter remains for
    /// compatibility with direct drivers and for pinning a fresh context across a spawned task.
    pub fn set_cancel(&self, token: tokio_util::sync::CancellationToken) {
        *self.cancel.lock().unwrap() = Some(token);
    }

    /// The active lexical turn's cancellation token, falling back to a deliberately stored value
    /// only when no lexical scope exists.
    pub fn cancel_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        self.runtime_turn_context().cancel_token()
    }

    /// Install stored session lineage. New turn drivers should scope a [`RuntimeTurnContext`]; this
    /// setter remains for direct authored-flow compatibility and fresh-context pinning.
    pub fn set_session(&self, session_id: impl Into<String>) {
        *self.session.lock().unwrap() = Some(session_id.into());
    }

    /// The active lexical parent session, falling back to a deliberately stored value only when no
    /// lexical scope exists.
    pub fn session_id(&self) -> Option<String> {
        self.runtime_turn_context().session_id()
    }

    /// Install a stored child-activity reporter. Prefer a lexical [`RuntimeTurnContext`] for live
    /// turn drives; this setter is intended for fresh-context pinning and compatibility.
    pub fn set_spawn_activity_sink(&self, sink: Arc<dyn SpawnActivitySink>) {
        *self.spawn_activity.lock().unwrap() = Some(sink);
    }

    /// Snapshot the current turn's child-activity reporter for a [`SpawnRequest`].
    pub fn spawn_activity_sink(&self) -> Option<Arc<dyn SpawnActivitySink>> {
        self.runtime_turn_context().spawn_activity_sink()
    }

    /// Snapshot the current turn's owner for sub-agent tasks.
    pub fn spawn_supervisor(&self) -> Option<Arc<SpawnTaskSupervisor>> {
        self.runtime_turn_context().spawn_supervisor()
    }

    /// A redacting handle `tool` uses to report its own in-flight output (C-158), or `None` when no
    /// surface installed a live channel — a tool then runs exactly as before, reporting nothing.
    ///
    /// This is the ONLY way to reach a [`ToolProgressSink`], and it binds the context's own
    /// [`Redactor`] — the same one [`Executor::dispatch`] scrubs the final result with — so a
    /// reported line cannot skip redaction even if the tool wanted it to.
    pub fn progress_reporter(&self, tool: &str) -> Option<ToolProgressReporter> {
        self.runtime_turn_context()
            .tool_progress_sink()
            .map(|sink| ToolProgressReporter {
                tool: tool.to_string(),
                redactor: self.redactor.clone(),
                sink,
            })
    }

    /// Install a stored pane channel. Prefer a lexical [`RuntimeTurnContext`] for live turn drives;
    /// this setter is intended for fresh-context pinning and compatibility.
    pub fn set_surface_sink(&self, sink: Arc<dyn SurfaceSink>) {
        *self.surface.lock().unwrap() = Some(sink);
    }

    /// A redacting handle a tool uses to address the human surface (C-220), or `None` when no host
    /// installed a sink — the posture of [`ToolContext::progress_reporter`]. A headless `flux run`,
    /// `flux-server` or SDK embedding therefore has no pane channel at all, and a caller learns
    /// that instead of writing into a void.
    ///
    /// This is the ONLY way to reach a [`SurfaceSink`], and it binds the context's own
    /// [`Redactor`] — the same one [`Executor::dispatch`] scrubs the final result with — so pane
    /// content cannot skip redaction even if the tool wanted it to.
    pub fn surface(&self) -> Option<SurfaceReporter> {
        self.runtime_turn_context()
            .surface_sink()
            .map(|sink| SurfaceReporter {
                redactor: self.redactor.clone(),
                sink,
            })
    }

    /// Snapshot the identity frozen for the active lexical turn. Direct one-shot runtimes may see
    /// an inherited construction-time snapshot; an ordinary context outside a turn returns `None`.
    pub fn turn_identity(&self) -> Option<TurnIdentity> {
        self.runtime_turn_context().identity()
    }

    /// The complete effective runtime-turn snapshot. An active lexical scope is authoritative,
    /// including absent fields; only outside such a scope are the stored compatibility values read.
    /// This distinction prevents a reused context from reviving a prior turn's cancellation or
    /// session lineage inside an explicitly empty one-shot run.
    pub fn runtime_turn_context(&self) -> RuntimeTurnContext {
        active_runtime_turn_context().unwrap_or_else(|| RuntimeTurnContext {
            cancel: self.cancel.lock().unwrap().clone(),
            session: self.session.lock().unwrap().clone(),
            spawn_activity: self.spawn_activity.lock().unwrap().clone(),
            spawn_supervisor: self.spawn_supervisor.lock().unwrap().clone(),
            identity: self.identity.clone(),
            tool_progress: self.tool_progress.lock().unwrap().clone(),
            surface: self.surface.lock().unwrap().clone(),
        })
    }

    /// Replace all stored runtime-turn fallbacks with one snapshot. This is the explicit bridge for
    /// a **fresh** nested context that is about to cross a spawned-task boundary; long-lived engine
    /// contexts should scope the turn instead, so retained contexts cannot keep obsolete state.
    pub fn set_runtime_turn_context(&mut self, turn: RuntimeTurnContext) {
        *self.cancel.lock().unwrap() = turn.cancel;
        *self.session.lock().unwrap() = turn.session;
        *self.spawn_activity.lock().unwrap() = turn.spawn_activity;
        *self.spawn_supervisor.lock().unwrap() = turn.spawn_supervisor;
        *self.tool_progress.lock().unwrap() = turn.tool_progress;
        *self.surface.lock().unwrap() = turn.surface;
        self.identity = turn.identity;
    }

    /// Record that `path` was read at `mtime` (called by `read`/`read_many`).
    pub fn record_read(&self, path: &str, mtime: std::time::SystemTime) {
        self.read_times
            .lock()
            .unwrap()
            .insert(path.to_string(), mtime);
    }

    /// The mtime `path` had when it was last read this session, if ever.
    pub fn read_mtime(&self, path: &str) -> Option<std::time::SystemTime> {
        self.read_times.lock().unwrap().get(path).copied()
    }

    pub fn with_spawner(mut self, spawner: Arc<dyn Spawner>) -> Self {
        self.spawner = Some(spawner);
        self
    }

    /// Install the authored outer-loop capability (the engine does this per turn).
    pub fn with_loop_host(mut self, loop_host: Arc<dyn LoopHost>) -> Self {
        self.loop_host = Some(loop_host);
        self
    }

    /// Install the composite-op registration capability.
    pub fn with_composite_registrar(mut self, registrar: Arc<dyn CompositeRegistrar>) -> Self {
        self.composite_registrar = Some(registrar);
        self
    }

    /// Install the on-demand skill-body loading capability (D-188).
    pub fn with_skill_loader(mut self, loader: Arc<dyn SkillLoader>) -> Self {
        self.skill_loader = Some(loader);
        self
    }

    /// Set the secret redactor (seeded with known secret values; see [`SecretResolver`]).
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.redactor = redactor;
        self
    }
}

/// Resolves secret references to their materialized values and seeds a [`Redactor`]. Only the
/// `env/KEY` scheme is resolved at runtime today; `plugin`/`kubernetes` refs are resolved by their
/// providers later. Resolution is the only place env secrets are read for redaction.
#[derive(Default, Clone)]
pub struct SecretResolver;

impl SecretResolver {
    pub fn new() -> Self {
        Self
    }

    /// Resolve a single reference to its [`Material`](flux_secret::Material), if available.
    pub fn resolve(&self, r: &flux_secret::Ref) -> Option<flux_secret::Material> {
        match r.scheme {
            flux_secret::Scheme::Env => {
                std::env::var(&r.slot)
                    .ok()
                    .map(|value| flux_secret::Material {
                        reference: r.clone(),
                        kind: flux_secret::Kind::ApiKey,
                        value,
                        media_type: None,
                    })
            }
            _ => None,
        }
    }

    /// Register the values of every resolvable ref in `refs` with `redactor`, so they are scrubbed
    /// from tool output and logs.
    pub fn seed_redactor(&self, redactor: &mut Redactor, refs: &[flux_secret::Ref]) {
        for r in refs {
            if let Some(m) = self.resolve(r) {
                redactor.add_secret(m.value);
            }
        }
    }
}

/// One exact authorization request required by a concrete tool invocation.
///
/// This is the shared authority vocabulary between plan preview and dispatch. `action` names the
/// behavior being authorized; `resource` carries the resource family and invocation-level subject
/// (path, datasource, provider, connection target, or operation identity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityRequirement {
    pub action: Action,
    pub resource: ResourceRef,
}

impl AuthorityRequirement {
    pub fn new(action: impl Into<Action>, resource: ResourceRef) -> Self {
        Self {
            action: action.into(),
            resource,
        }
    }

    pub fn operation(action: impl Into<Action>, operation: impl Into<String>) -> Self {
        Self::new(
            action,
            ResourceRef::named(ResourceKind::Operation, operation),
        )
    }

    pub fn workspace_read(path: impl Into<String>) -> Self {
        Self::new("workspace.read", ResourceRef::path(path))
    }

    pub fn workspace_write(path: impl Into<String>) -> Self {
        Self::new("workspace.write", ResourceRef::path(path))
    }

    pub fn datasource_read(subject: impl Into<String>) -> Self {
        Self::new(
            "datasource.read",
            ResourceRef::named(ResourceKind::Datasource, subject),
        )
    }

    pub fn datasource_write(subject: impl Into<String>) -> Self {
        Self::new(
            "datasource.write",
            ResourceRef::named(ResourceKind::Datasource, subject),
        )
    }

    pub fn network_fetch(subject: impl Into<String>) -> Self {
        Self::new(
            "network.fetch",
            ResourceRef::named(ResourceKind::Network, subject),
        )
    }

    pub fn connection_dial(subject: impl Into<String>) -> Self {
        Self::new(
            "connection.dial",
            ResourceRef::named(ResourceKind::Connection, subject),
        )
    }

    pub fn process_exec(subject: impl Into<String>) -> Self {
        Self::new(
            "process.exec",
            ResourceRef::named(ResourceKind::Process, subject),
        )
    }

    pub fn secret_read(subject: impl Into<String>) -> Self {
        Self::new(
            "secret.read",
            ResourceRef::named(ResourceKind::Secret, subject),
        )
    }

    pub fn provider_invoke(subject: impl Into<String>) -> Self {
        Self::new(
            "model.invoke",
            ResourceRef::named(ResourceKind::Provider, subject),
        )
    }

    pub fn browser_navigate(subject: impl Into<String>) -> Self {
        Self::new(
            "browser.navigate",
            ResourceRef::named(ResourceKind::Network, subject),
        )
    }

    pub fn host_read(subject: impl Into<String>) -> Self {
        Self::new("host.read", ResourceRef::named(ResourceKind::Host, subject))
    }

    pub fn host_write(subject: impl Into<String>) -> Self {
        Self::new(
            "host.write",
            ResourceRef::named(ResourceKind::Host, subject),
        )
    }

    /// Whether this requirement represents mutation, process execution, or outbound contact for
    /// the whole-plan disclosure. Dispatch does not rely on this classification.
    pub fn is_mutating(&self) -> bool {
        !matches!(
            self.action.0.as_str(),
            "workspace.read" | "datasource.read" | "host.read" | "secret.read" | "model.invoke"
        )
    }

    pub fn is_destructive(&self) -> bool {
        matches!(self.action.0.as_str(), "flow.delete" | "flow.money")
    }
}

/// A tool the agent can invoke. Permission metadata and intents are declared here so the
/// dispatcher can gate, render, and audit the call.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    /// Permission subjects for this invocation (e.g. `["src/main.rs"]` for read, `["git:status"]`
    /// for bash). Empty means the tool is gated only by its bare name.
    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        Vec::new()
    }

    /// Pre-execution intents (the approval-risk signal).
    fn intents(&self, _params: &Value) -> IntentSet {
        IntentSet::new()
    }

    /// Whether the adaptive loop may use this operation while gathering evidence or must capture
    /// it for later approval. This is never an authorization bypass: concrete intents and the
    /// tool's risk/effect/idempotency contract can only make the effective disposition stricter.
    fn staging_disposition(&self) -> StagingDisposition {
        StagingDisposition::Infer
    }

    /// Declared SEMANTIC-effect tags this tool carries beyond its host [`ToolSpec::effects`] — e.g.
    /// `"money"`, `"delete"`, `"send_external"` (the `flux_lang::ast::FlowEffect` tag vocabulary,
    /// D-138). Plain strings rather than the typed `FlowEffect` enum so this trait — the safety
    /// envelope's core seam, implemented far outside the language crate too — stays free of a
    /// `flux-lang` dependency; a Flux-Lang-aware catalog adapter (`flux-flow`'s `OpRegistry`) parses
    /// them back via `FlowEffect::from_tag` onto `OpSignature::semantic_effects`. Default empty:
    /// most tools have no semantic tier beyond their host effects, and every existing `impl Tool`
    /// keeps compiling unchanged.
    fn semantic_effects(&self) -> Vec<String> {
        Vec::new()
    }

    /// Exact authorization requirements for this invocation.
    ///
    /// The dispatcher and whole-plan preview both consume this typed contract. The default adapter
    /// is deliberately resource-aware: a generic `Read` is pure unless paired with a concrete
    /// access kind, while unknown semantic actions fail closed. Tools whose resource identity is
    /// richer than filesystem/network/process access (notably datasources and plugins) override
    /// this method and return their invocation-level subjects directly.
    fn authority_requirements(
        &self,
        _params: &Value,
        subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        authority_requirements_from_declaration(&self.spec(), subjects, &self.semantic_effects())
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult>;
}

/// The result of [`ToolRegistry::resolve_disabled`] (C-162): which concrete op names a `[tools]
/// disable` list resolves to, and which of its patterns matched nothing.
#[derive(Debug, Clone, Default)]
pub struct ResolvedDisabledOps {
    /// Concrete op names to exclude from the surfaced/advertised set and refuse at dispatch.
    pub disabled: HashSet<String>,
    /// Patterns that matched no registered op — the caller should warn, naming the entry, rather
    /// than silently treating it as a no-op.
    pub unmatched: Vec<String>,
}

/// A registry of tools keyed by name.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    sources: HashMap<String, String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool from an unnamed direct source.
    ///
    /// This compatibility API fails closed by panicking on an invalid or duplicate declaration;
    /// fallible production assembly should use [`try_register`](Self::try_register) or
    /// [`try_register_from`](Self::try_register_from) and propagate the path-aware error.
    ///
    /// # Deprecated
    ///
    /// Kept source-compatible for integrations written before registration became fallible. New
    /// assembly code must use a `try_*` method and return the error to its caller.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.try_register(tool)
            .unwrap_or_else(|err| panic!("tool registration failed: {err}"));
    }

    /// Fallible registration for direct/programmatic tools.
    pub fn try_register(&mut self, tool: Arc<dyn Tool>) -> Result<()> {
        self.try_register_from("direct", tool)
    }

    /// Fallible registration with a source label used in duplicate diagnostics (for example a
    /// plugin descriptor path or built-in pack name).
    pub fn try_register_from(
        &mut self,
        source: impl Into<String>,
        tool: Arc<dyn Tool>,
    ) -> Result<()> {
        let source = source.into();
        let spec = tool.spec();
        let name = spec.name.clone();
        if name.trim().is_empty() {
            return Err(Error::Other(format!(
                "tool from `{source}` has an empty operation name"
            )));
        }
        if let Some(existing) = self.tools.get(&name) {
            let existing_source = self
                .sources
                .get(&name)
                .map(String::as_str)
                .unwrap_or("unknown");
            let same =
                serde_json::to_value(existing.spec()).ok() == serde_json::to_value(&spec).ok();
            let shape = if same { "identical" } else { "conflicting" };
            return Err(Error::Other(format!(
                "duplicate operation `{name}` from `{source}` ({shape} declaration; already registered from `{existing_source}`)"
            )));
        }

        let input = json!({});
        let subjects = tool.permission_subjects(&input);
        tool.authority_requirements(&input, &subjects)
            .map_err(|err| {
                Error::Other(format!(
                    "invalid authority contract for `{name}` from `{source}`: {err}"
                ))
            })?;
        self.tools.insert(name.clone(), tool);
        self.sources.insert(name, source);
        Ok(())
    }

    /// Atomically register a pack of tools under one auditable source label.
    ///
    /// If any declaration is invalid or collides, none of the pack is installed. Independently
    /// assembled packs should use distinct source labels so the duplicate diagnostic names both
    /// contributors.
    pub fn try_register_all_from<I>(&mut self, source: impl Into<String>, tools: I) -> Result<()>
    where
        I: IntoIterator<Item = Arc<dyn Tool>>,
    {
        let source = source.into();
        let mut assembled = self.clone();
        for tool in tools {
            assembled.try_register_from(source.clone(), tool)?;
        }
        *self = assembled;
        Ok(())
    }

    /// Explicitly replace a registered tool, returning the previous handler. Callers must name the
    /// replacement source so the audit/catalog owner is visible; ordinary registration never
    /// overwrites.
    pub fn replace_from(
        &mut self,
        source: impl Into<String>,
        tool: Arc<dyn Tool>,
    ) -> Result<Option<Arc<dyn Tool>>> {
        let source = source.into();
        let spec = tool.spec();
        let name = spec.name.clone();
        if name.trim().is_empty() {
            return Err(Error::Other(format!(
                "replacement tool from `{source}` has an empty operation name"
            )));
        }
        let input = json!({});
        let subjects = tool.permission_subjects(&input);
        tool.authority_requirements(&input, &subjects)
            .map_err(|err| {
                Error::Other(format!(
                    "invalid authority contract for replacement `{name}` from `{source}`: {err}"
                ))
            })?;
        self.sources.insert(name.clone(), source);
        Ok(self.tools.insert(name, tool))
    }

    /// Merge another assembled catalog while preserving each operation's source label.
    ///
    /// This is the fallible composition seam for independently assembled packs (for example an L6
    /// integration catalog joining the App's built-ins). A collision remains an error; intentional
    /// control-plane overrides must use [`Self::replace_from`] explicitly.
    pub fn try_extend(&mut self, mut other: ToolRegistry) -> Result<()> {
        let mut assembled = self.clone();
        let mut names: Vec<String> = other.tools.keys().cloned().collect();
        names.sort();
        for name in names {
            let Some(tool) = other.tools.remove(&name) else {
                return Err(Error::Other(format!(
                    "registry changed while composing operation `{name}`"
                )));
            };
            let source = other
                .sources
                .remove(&name)
                .unwrap_or_else(|| "unknown".to_string());
            assembled.try_register_from(source, tool)?;
        }
        *self = assembled;
        Ok(())
    }

    /// The auditable source label attached to an operation, when registered.
    pub fn source(&self, name: &str) -> Option<&str> {
        self.sources.get(name).map(String::as_str)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Remove a tool by name, returning it if present. Used to scope a sub-agent's registry (e.g.
    /// drop `task` so a sub-agent can't spawn further sub-agents).
    pub fn remove(&mut self, name: &str) -> Option<Arc<dyn Tool>> {
        self.sources.remove(name);
        self.tools.remove(name)
    }

    /// Specs for every registered tool (e.g. to advertise to the model), **name-sorted**: the
    /// backing map is a `HashMap` whose iteration order changes per process, and anything rendered
    /// into the model prompt from here must be byte-stable or the provider prompt cache can never
    /// hit across invocations (A-03).
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self.tools.values().map(|t| t.spec()).collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    /// A registry scoped to a sub-agent's allowed tools. `None` (the role declared no `tools` key)
    /// inherits all parent tools; `Some(names)` keeps only those — so `Some(&[])`, an *explicitly
    /// empty* allowlist, yields an empty registry. (Previously an empty slice meant "all", which
    /// silently turned the most-restrictive declaration into the least-restrictive outcome.)
    pub fn subset(&self, names: Option<&[String]>) -> ToolRegistry {
        let Some(names) = names else {
            return self.clone();
        };
        let tools: HashMap<String, Arc<dyn Tool>> = self
            .tools
            .iter()
            .filter(|(k, _)| names.iter().any(|n| n == *k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let sources = self
            .sources
            .iter()
            .filter(|(name, _)| tools.contains_key(*name))
            .map(|(name, source)| (name.clone(), source.clone()))
            .collect();
        ToolRegistry { tools, sources }
    }

    /// Every registered tool name, sorted (see [`specs`](Self::specs) for why order must be stable).
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }

    /// Resolve `[tools] disable` patterns (C-162) against this registry's known op names — an
    /// exact op name or a `family.*` glob (see [`flux_config::tool_disable_matches`]) — into the
    /// concrete set to hide/refuse, plus any pattern that matched no registered op (a likely typo
    /// or a stale entry naming a retired op, so the caller can warn instead of silently no-op-ing).
    /// Resolving once against a fixed registry snapshot is what keeps the result stable for the
    /// life of the executor it's installed on (the A-95 cache-stability lesson): the set can never
    /// churn mid-session.
    pub fn resolve_disabled(&self, patterns: &[String]) -> ResolvedDisabledOps {
        let known = self.names();
        let mut disabled = HashSet::new();
        let mut unmatched = Vec::new();
        for pattern in patterns {
            let mut matched_any = false;
            for name in &known {
                if flux_config::tool_disable_matches(pattern, name) {
                    disabled.insert(name.clone());
                    matched_any = true;
                }
            }
            if !matched_any {
                unmatched.push(pattern.clone());
            }
        }
        ResolvedDisabledOps {
            disabled,
            unmatched,
        }
    }

    /// Validate every registered tool's static authority declaration.
    ///
    /// Registration owners call this after composing catalogs; dispatch still revalidates with the
    /// concrete invocation. The empty object intentionally represents the least-specific call, so
    /// an operation must either produce a conservative wildcard requirement or reject registration
    /// rather than rely on runtime parameters to invent its resource family.
    pub fn validate_authority_contracts(&self) -> Result<()> {
        for tool in self.tools.values() {
            let input = json!({});
            let subjects = tool.permission_subjects(&input);
            tool.authority_requirements(&input, &subjects)
                .map_err(|err| {
                    Error::Other(format!(
                        "invalid authority contract for `{}`: {err}",
                        tool.spec().name
                    ))
                })?;
        }
        Ok(())
    }

    /// Specs for the ops that should be **advertised to the model** given the group manifest and the
    /// active group set: core ops (in no group) always; a grouped op only when its group is active.
    /// See [`is_advertised`]. An empty manifest with no group-tagged specs advertises everything.
    /// Name-sorted, like [`specs`](Self::specs).
    pub fn active_specs(
        &self,
        groups: &[flux_evidence::ToolGroup],
        active: &HashSet<String>,
    ) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .tools
            .values()
            .map(|t| t.spec())
            .filter(|s| is_advertised(s, groups, active))
            .collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }
}

/// `FLUX_SURFACE_ALL=1` (or `true`) disables evidence gating — every op is advertised, as before
/// surfacing existed. An escape hatch for debugging and parity.
pub fn surface_all_override() -> bool {
    std::env::var("FLUX_SURFACE_ALL").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// In-process override for the shell opt-in (0 = unset, 1 = forced off, 2 = forced on). The CLI's
/// config wiring and REPL `/shell` toggle flip this instead of mutating `FLUX_ENABLE_BASH`:
/// `setenv` on a live multi-threaded runtime races any concurrent `getenv` (UB on glibc — the
/// reason Rust 2024 marks `set_var` unsafe), while the env var itself stays the cross-process
/// channel an operator exports.
static SHELL_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Force the generic `bash` op on/off for this process (config `enable_shell`, REPL `/shell`),
/// overriding `FLUX_ENABLE_BASH` in both directions. Takes effect at the next catalog
/// recomputation ([`detect_signals`] runs per turn).
pub fn set_shell_opt_in(on: bool) {
    SHELL_OVERRIDE.store(if on { 2 } else { 1 }, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the generic `bash` op is opted in: the in-process override ([`set_shell_opt_in`]) when
/// set, else `FLUX_ENABLE_BASH=1` (or `true`). [`detect_signals`] turns it into the `shell`
/// signal that surfaces the off-by-default `shell` group.
pub fn shell_opt_in() -> bool {
    match SHELL_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => std::env::var("FLUX_ENABLE_BASH")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
    }
}

/// The group tag for authored outer-loop machinery. It is never surfaced by a workspace signal, so
/// these ops stay out of the model-facing catalog while remaining dispatchable by the agent loop.
/// Shared so the tag and the catalog filters cannot drift.
pub const REFLECT_GROUP: &str = "reflect";

/// The group an op effectively belongs to: a manifest group that lists it in `tools` wins (so config
/// can (re)assign membership), otherwise the op's own [`ToolSpec::group`] tag. `None` ⇒ *core*.
pub fn effective_group<'a>(
    spec: &'a ToolSpec,
    groups: &'a [flux_evidence::ToolGroup],
) -> Option<&'a str> {
    groups
        .iter()
        .find(|g| g.tools.iter().any(|t| t == &spec.name))
        .map(|g| g.name.as_str())
        .or(spec.group.as_deref())
}

/// Whether `spec` should be advertised to the model: core ops (no effective group) always; a grouped
/// op only when its group is in `active`. `FLUX_SURFACE_ALL` forces everything on. Membership comes
/// from the manifest's `tools` or the op's own [`ToolSpec::group`] tag (see [`effective_group`]).
pub fn is_advertised(
    spec: &ToolSpec,
    groups: &[flux_evidence::ToolGroup],
    active: &HashSet<String>,
) -> bool {
    surface_all_override()
        || match effective_group(spec, groups) {
            None => true,
            Some(g) => active.contains(g),
        }
}

/// The set of op names to advertise to the model — [`is_advertised`] applied across `specs`. Handy
/// for filtering a name-keyed catalog (e.g. the Flux-Lang op catalog in `flux-flow`).
pub fn advertised_op_names(
    specs: &[ToolSpec],
    groups: &[flux_evidence::ToolGroup],
    active: &HashSet<String>,
) -> HashSet<String> {
    specs
        .iter()
        .filter(|s| is_advertised(s, groups, active))
        .map(|s| s.name.clone())
        .collect()
}

/// Probe `cwd` (walking up to the nearest marker) for the workspace signals currently true, as
/// `project.signal` [`Observation`]s. Cheap enough to run every turn — a handful of `exists()`
/// checks. The emitted `signal` strings are the contract that group `surface_when` matches against
/// (see `flux-tools`' `builtin_groups`).
pub fn detect_signals(cwd: &std::path::Path) -> Vec<Observation> {
    let mut out = Vec::new();
    let mut push = |sig: &str| {
        out.push(Observation::signal(sig));
    };
    // Marker signals via a SINGLE upward walk (cwd→root): at each ancestor level, test every
    // not-yet-found marker — a marker in any parent still counts (running from a subdir, or a git
    // worktree where `.git` is a file) — instead of re-walking the whole ancestor chain once per
    // marker. Push order is preserved (callers sort anyway, but keep it stable).
    type Marker = (&'static str, fn(&std::path::Path) -> bool);
    let markers: [Marker; 7] = [
        ("git_repo", |p| p.join(".git").exists()),
        ("go", |p| p.join("go.mod").exists()),
        ("rust", |p| p.join("Cargo.toml").exists()),
        ("node", |p| p.join("package.json").exists()),
        ("python", |p| {
            p.join("pyproject.toml").exists() || p.join("requirements.txt").exists()
        }),
        ("make", |p| {
            p.join("Makefile").exists() || p.join("makefile").exists()
        }),
        ("eval", |p| p.join(".flux").join("evals").is_dir()),
    ];
    let mut found = [false; 7];
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        for (i, (_, pred)) in markers.iter().enumerate() {
            if !found[i] {
                found[i] = pred(d);
            }
        }
        if found.iter().all(|&f| f) {
            break;
        }
        dir = d.parent();
    }
    for (i, (sig, _)) in markers.iter().enumerate() {
        if found[i] {
            push(sig);
        }
    }
    // `shell` is an explicit opt-in, not a filesystem marker: it surfaces the off-by-default `shell`
    // group (the generic `bash` op). The CLI sets `FLUX_ENABLE_BASH` from config `enable_shell`, the
    // `/shell` toggle, or the user exports it directly.
    if shell_opt_in() {
        push("shell");
    }
    // `kubernetes` is ambient (a kubeconfig is reachable), not a workspace-walk marker: it surfaces
    // the `endpoint` discovery group (D-28). True when `KUBECONFIG` is set OR `~/.kube/config` exists.
    if kubeconfig_present() {
        push("kubernetes");
    }
    // `browser` is ambient too (a Chromium binary is discoverable): it surfaces the native `browser`
    // group (flux-web, D-121). Advertising a browser that isn't installed only misleads the planner,
    // so the ops stay out of the catalog until a binary is found.
    if chromium_present() {
        push("browser");
    }
    // `agent_triggerable` (D-187): at least one discovered command file or skill in this session
    // opts into agent invocation (`agent-triggerable: true`, default false). Unlike the marker
    // checks above this parses frontmatter, not just `exists()` — bounded by the same discovery
    // dirs `command.invoke` itself re-reads at call time, so the signal and the op's own
    // "accessible" gate can never disagree about what is discoverable. Discovery failures (a
    // symlink escape, an unreadable dir) degrade to "no signal" rather than surfacing the op on an
    // error path.
    if agent_triggerable_target_present(cwd) {
        push("agent_triggerable");
    }
    out
}

/// Whether `cwd`'s discoverable command files or skills include at least one marked
/// `agent-triggerable: true`. Lenient: any discovery error is treated as "none found".
fn agent_triggerable_target_present(cwd: &std::path::Path) -> bool {
    let commands = metadata::discover_commands(cwd)
        .map(|d| d.commands)
        .unwrap_or_default();
    if commands.iter().any(|c| c.agent_triggerable) {
        return true;
    }
    metadata::discover_skills(cwd, &[])
        .unwrap_or_default()
        .iter()
        .any(|s| s.agent_triggerable)
}

/// Whether a kubeconfig is reachable: `KUBECONFIG` is set (non-empty) OR `~/.kube/config` exists. This
/// is ambient (host environment / home dir), independent of `cwd` — kubectl finds its config this way.
/// Both halves of that check reach the spawned `kubectl`: `KUBECONFIG` and `HOME` are on
/// `flux_system`'s `SAFE_ENV` allow-list (C-207), so what this probe reads is what the executor
/// forwards. Adding a signal here that the guarded process path drops would surface ops that
/// cannot work.
fn kubeconfig_present() -> bool {
    if std::env::var_os("KUBECONFIG").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".kube").join("config"))
        .is_some_and(|p| p.exists())
}

/// Whether a Chromium binary is discoverable (the `browser`-group signal): `FLUX_BROWSER_BIN` is set,
/// or one of the well-known Chromium binaries is on `PATH`. Ambient (env/PATH), independent of `cwd`.
/// Mirrors `flux_web::discover_chrome`'s candidate order — L2 can't depend on the L5 web crate.
fn chromium_present() -> bool {
    if std::env::var_os("FLUX_BROWSER_BIN").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    const CANDIDATES: [&str; 6] = [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "google-chrome-unstable",
    ];
    std::env::split_paths(&path).any(|dir| CANDIDATES.iter().any(|c| dir.join(c).is_file()))
}

/// Cap an oversized tool result for the model transcript: within `cap` chars it is returned
/// unchanged; otherwise it is truncated to `cap` and a one-line notice is appended recording how much
/// was dropped and pointing the model at a follow-up read for the exact bytes. Keeps a single huge
/// `bash`/`read`/`grep` result from blowing the context budget. `cap == 0` disables trimming.
pub fn trim_tool_output(content: String, cap: usize, label: &str) -> String {
    if cap == 0 {
        return content;
    }
    let total = content.chars().count();
    if total <= cap {
        return content;
    }
    let kept: String = content.chars().take(cap).collect();
    let omitted = total - cap;
    format!(
        "{kept}\n…[{label} output truncated: {omitted} of {total} chars omitted — narrow the range \
         or do a follow-up read for the full output]"
    )
}

/// The per-result transcript cap (chars) for [`trim_tool_output`], from `FLUX_TOOL_OUTPUT_CAP`
/// (default 20000). `0` disables per-result trimming. Mirrors the session-compaction knob but acts on
/// a single tool/op result so one huge output can't blow the budget before compaction runs.
pub fn tool_output_cap() -> usize {
    std::env::var("FLUX_TOOL_OUTPUT_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000)
}

/// The user's response to an approval request.
#[derive(Debug, Clone)]
pub enum ApprovalChoice {
    Allow,
    /// Allow and remember this rule (added to the allow list).
    AllowAlways(String),
    Deny,
    /// Deny and tell the model why (C-113). The reason is APPENDED to the canonical
    /// `` `{op}` denied by user `` result text — never a mutation of it — so denial
    /// classification (structural via `DispatchOutcome.denied`, see L-32) and every
    /// existing `Deny` construction site are unaffected.
    DenyWithReason(String),
}

/// What a whole-plan approval decides on: the plan's statically-visible behavior, aggregated from
/// every op call the risk preview walked. `intents` carries the SAME pre-execution risk signal the
/// per-op gate sees (so a headless approver like the sub-agent one can apply its per-op policy to
/// the plan as a unit); `destructive` additionally covers spec-level `Risk::Destructive` ops whose
/// concrete intents aren't statically visible (e.g. composite ops declaring destructive risk).
#[derive(Debug, Clone, Default)]
pub struct PlanApprovalRequest {
    /// One-line human risk summary (shown at the approval prompt).
    pub summary: String,
    /// The distinct op names the plan calls, in first-seen order.
    pub ops: Vec<String>,
    /// True when the plan contains a destructive-shaped op (by intent heuristic or declared risk).
    pub destructive: bool,
    /// True when any op writes / executes / connects out.
    pub mutating: bool,
    /// Aggregate statically-visible intents across the plan's op calls. Only literal args are known
    /// at approval time — a command assembled from `$symbols` at runtime is NOT in here, which is
    /// why an *undisclosed* destructive op re-fires the per-op gate inside an approved scope.
    pub intents: IntentSet,
    /// Exact typed authority requirements derived from the same invocation contract dispatch will
    /// evaluate. Dynamic arguments may make this a conservative preview; dispatch always re-derives
    /// and enforces the concrete set.
    pub requirements: Vec<AuthorityRequirement>,
}

impl PlanApprovalRequest {
    /// The prompt subject line (`N op(s) · summary`).
    pub fn subject(&self) -> String {
        format!("{} op(s) · {}", self.ops.len(), self.summary)
    }
}

/// How the runtime asks for human approval when a call isn't covered by a rule.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn request(&self, tool: &str, subjects: &[String], intents: &IntentSet)
        -> ApprovalChoice;

    /// Approve a whole compiled plan as one unit (the "approve the graph, not each node" path).
    /// Surfaces are not guaranteed to have rendered the plan beforehand — an interactive approver
    /// should present the request's own content (ops, requirements, intents) in its prompt.
    /// `AllowAlways` here means "trust every plan for the rest of the session".
    /// The default delegates to [`request`](Self::request) with the plan's REAL aggregate intents —
    /// so a single-method approver applies its per-op policy (e.g. deny-destructive) to the plan too.
    async fn request_plan(&self, plan: &PlanApprovalRequest) -> ApprovalChoice {
        self.request("run plan", &[plan.subject()], &plan.intents)
            .await
    }
}

/// A headless approver that denies anything not pre-allowed by rules.
pub struct DenyApprover;

#[async_trait]
impl Approver for DenyApprover {
    async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
        ApprovalChoice::Deny
    }
}

/// A headless approver that allows everything (e.g. `flux run --yes`, the served daemon). Use with
/// care — it approves destructive plans and ops alike (the human opted in at the surface). Never
/// install it for sub-agents: `SubAgentApprover` (flux-orchestrate) is the sub-agent default and
/// denies destructive work outright.
pub struct AllowApprover;

#[async_trait]
impl Approver for AllowApprover {
    async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
        ApprovalChoice::Allow
    }
}

/// The outcome of a pre-tool hook.
pub enum HookOutcome {
    /// Proceed unchanged.
    Continue,
    /// Replace the tool input with this value, then proceed.
    Modify(serde_json::Value),
    /// Block the call with this reason.
    Deny(String),
}

/// A hook run before a tool executes — may observe, modify the input, or deny the call. Engine-
/// agnostic so `flux-runtime` doesn't depend on a JS runtime; `flux_plugin::hooks` provides a JS impl.
pub trait PreToolHook: Send + Sync {
    fn pre_tool(&self, tool: &str, input: &serde_json::Value) -> HookOutcome;
}

/// The immutable assembly-time `(Caller, Trust)` behind a cloneable shared handle.
///
/// An [`Executor`] and its sub-agent spawner may share this fallback snapshot. Multi-principal
/// surfaces do not retarget it; they pass a [`TurnIdentity`] through the engine's lexical turn API.
#[derive(Clone)]
pub struct IdentityCell(Arc<(Caller, Trust)>);

impl IdentityCell {
    pub fn new(caller: Caller, trust: Trust) -> Self {
        Self(Arc::new((caller, trust)))
    }

    /// The local single-user identity (the default when a surface never resolves one).
    pub fn local() -> Self {
        let (caller, trust) = local_identity("local");
        Self::new(caller, trust)
    }

    /// Snapshot the immutable identity as its legacy tuple shape.
    pub fn get(&self) -> (Caller, Trust) {
        self.0.as_ref().clone()
    }

    /// Snapshot the immutable identity as a lexical turn value.
    pub fn snapshot(&self) -> TurnIdentity {
        let (caller, trust) = self.get();
        TurnIdentity::new(caller, trust)
    }
}

/// The authorization floor and resolved identity installed atomically on an [`Executor`].
///
/// Use [`local`](Self::local) for the documented single-user profile, or [`new`](Self::new) /
/// [`with_identity_cell`](Self::with_identity_cell) when a surface resolves its own principal.
/// There is deliberately no "disabled" profile: approval rules may narrow this floor, never remove
/// it.
#[derive(Clone)]
pub struct ExecutionAuthorization {
    policy: AuthorizationPolicy,
    identity: IdentityCell,
}

impl ExecutionAuthorization {
    /// Pair an explicit policy with a fixed caller and trust assertion.
    pub fn new(policy: AuthorizationPolicy, caller: Caller, trust: Trust) -> Self {
        Self::with_identity_cell(policy, IdentityCell::new(caller, trust))
    }

    /// Pair an explicit policy with an immutable identity snapshot shared by assembly components.
    pub fn with_identity_cell(policy: AuthorizationPolicy, identity: IdentityCell) -> Self {
        Self { policy, identity }
    }

    /// The documented local single-user profile: canonical local identity plus the default local
    /// grants. This is the safe compatibility profile used by [`Executor::new`].
    pub fn local() -> Self {
        Self {
            policy: default_local_grants(),
            identity: IdentityCell::local(),
        }
    }

    /// The policy floor carried by this profile.
    pub fn policy(&self) -> &AuthorizationPolicy {
        &self.policy
    }

    /// The shared immutable identity snapshot carried by this profile.
    pub fn identity(&self) -> IdentityCell {
        self.identity.clone()
    }
}

/// Mechanical assembly of one guarded execution environment.
///
/// Surfaces decide *which* workspace, tools, permission rules, approver, policy, and identity apply;
/// this type owns the invariant-preserving mechanics that turn those decisions into an
/// [`Executor`]. In particular, the guarded [`System`] is explicit and is reused for the
/// [`ToolContext`] instead of consulting the process current directory during lazy construction.
/// Plugin, endpoint, and datasource operations are ordinary entries in `registry`, so they take the
/// same path once their L6 host-specific audit capabilities have been assembled.
#[derive(Clone)]
pub struct ExecutionEnvironment {
    system: Arc<System>,
    registry: ToolRegistry,
    permissions: PermissionManager,
    approver: Arc<dyn Approver>,
    authorization: ExecutionAuthorization,
    redactor: Redactor,
    spawner: Option<Arc<dyn Spawner>>,
    runtime_turn: Option<RuntimeTurnContext>,
    hooks: Vec<Arc<dyn PreToolHook>>,
    /// A pre-created workspace handle (C-122): when set, the derived context is built over this
    /// exact handle instead of minting a fresh one, so a surface that also bound the same handle
    /// into its plugin host capabilities gets one shared view of worktree transitions.
    workspace: Option<WorkspaceContext>,
    // Compatibility bridge for callers that already built a richer context. New assembly should
    // use the explicit `System` constructor above so every derived executor receives a fresh
    // evidence/read-set context over the same root.
    exact_context: Option<ToolContext>,
    /// The resolved `[tools] disable` set (C-162/C-183) every executor derived from this
    /// environment installs via [`Executor::with_disabled_ops`]. Empty by default. Carrying this
    /// on the environment — rather than requiring every call site to re-apply it after
    /// `into_executor()` — is what lets a shared environment (e.g. `flux-app`'s per-journey and
    /// per-agent-target executors, both derived from one template) install the same resolved set
    /// without a second matching implementation.
    disabled_ops: HashSet<String>,
}

impl ExecutionEnvironment {
    /// Start an environment from all mandatory safety-envelope inputs.
    pub fn new(
        system: Arc<System>,
        registry: ToolRegistry,
        permissions: PermissionManager,
        approver: Arc<dyn Approver>,
        authorization: ExecutionAuthorization,
    ) -> Self {
        Self {
            system,
            registry,
            permissions,
            approver,
            authorization,
            redactor: Redactor::new(),
            spawner: None,
            runtime_turn: None,
            hooks: Vec::new(),
            workspace: None,
            exact_context: None,
            disabled_ops: HashSet::new(),
        }
    }

    /// Compatibility bridge for pre-builder callers that already own a [`ToolContext`].
    ///
    /// Prefer [`Self::new`]. This door exists so deprecated assembly shims can delegate to the same
    /// executor construction path; it is scheduled for removal with those shims in the next minor
    /// API cleanup.
    pub fn from_context(
        registry: ToolRegistry,
        permissions: PermissionManager,
        approver: Arc<dyn Approver>,
        authorization: ExecutionAuthorization,
        context: ToolContext,
    ) -> Self {
        Self {
            system: context.system(),
            registry,
            permissions,
            approver,
            authorization,
            redactor: context.redactor.clone(),
            spawner: context.spawner.clone(),
            runtime_turn: None,
            hooks: Vec::new(),
            workspace: None,
            exact_context: Some(context),
            disabled_ops: HashSet::new(),
        }
    }

    /// The exact guarded system shared by every executor derived from this environment.
    pub fn system(&self) -> &Arc<System> {
        &self.system
    }

    /// The operation catalog this environment will install.
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Mutate the catalog while assembling a surface-specific tool set.
    pub fn registry_mut(&mut self) -> &mut ToolRegistry {
        &mut self.registry
    }

    /// Replace the catalog while retaining every other environment invariant.
    pub fn with_registry(mut self, registry: ToolRegistry) -> Self {
        self.registry = registry;
        self
    }

    /// Replace the pre-approval permission rules.
    pub fn with_permissions(mut self, permissions: PermissionManager) -> Self {
        self.permissions = permissions;
        self
    }

    /// Replace the surface-owned approval handler.
    pub fn with_approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = approver;
        self
    }

    /// Replace the mandatory policy/identity profile.
    pub fn with_authorization(mut self, authorization: ExecutionAuthorization) -> Self {
        self.authorization = authorization;
        self
    }

    /// Install the shared secret redactor on every derived context.
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.redactor = redactor.clone();
        if let Some(context) = self.exact_context.take() {
            self.exact_context = Some(context.with_redactor(redactor));
        }
        self
    }

    /// Install a sub-agent spawner on every derived context.
    pub fn with_spawner(mut self, spawner: Arc<dyn Spawner>) -> Self {
        self.spawner = Some(spawner.clone());
        if let Some(context) = self.exact_context.take() {
            self.exact_context = Some(context.with_spawner(spawner));
        }
        self
    }

    /// Pin a complete lexical runtime-turn snapshot onto the next fresh context.
    ///
    /// This is intentionally opt-in: one-shot runtimes that cross `tokio::spawn` need it, while a
    /// long-lived engine context must not retain a parent turn's cancellation/session lineage.
    pub fn with_runtime_turn(mut self, runtime_turn: RuntimeTurnContext) -> Self {
        self.runtime_turn = Some(runtime_turn);
        self
    }

    /// Snapshot the currently effective lexical turn for a one-shot runtime that may cross a task
    /// boundary. Outside a turn this pins an explicitly empty context.
    pub fn inherit_runtime_turn(self) -> Self {
        self.with_runtime_turn(active_runtime_turn_context().unwrap_or_default())
    }

    /// Attach ordered pre-tool hooks.
    pub fn with_hooks(mut self, hooks: Vec<Arc<dyn PreToolHook>>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Build the derived context over this exact workspace handle (C-122) instead of a fresh one.
    /// The handle's active system should be the environment's `system` at call time; the surfaces
    /// that use this create the handle from that same system a few lines earlier.
    pub fn with_workspace(mut self, workspace: WorkspaceContext) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Install the resolved `[tools] disable` set (C-162's [`ToolRegistry::resolve_disabled`]) on
    /// every executor this environment builds. Empty by default (nothing disabled). Callers that
    /// derive several executors from one cloned environment (surface-contributed catalogs share
    /// this template) only need to call this once on the shared template — every clone carries it
    /// through [`Self::into_executor`].
    pub fn with_disabled_ops(mut self, disabled: HashSet<String>) -> Self {
        self.disabled_ops = disabled;
        self
    }

    /// The resolved `[tools] disable` set this environment will install on derived executors.
    pub fn disabled_ops(&self) -> &HashSet<String> {
        &self.disabled_ops
    }

    /// Build the guarded executor. No ambient path lookup or policy defaulting occurs here.
    pub fn into_executor(mut self) -> Executor {
        let context = match self.exact_context.take() {
            Some(context) => context,
            None => {
                let mut context = match self.workspace.take() {
                    // C-122: the surface pre-created the workspace handle and bound it into its
                    // plugin caps too — build over the same handle so both sides see transitions.
                    Some(workspace) => ToolContext::over_workspace(workspace),
                    None => ToolContext::new(self.system),
                }
                .with_redactor(self.redactor);
                if let Some(spawner) = self.spawner {
                    context = context.with_spawner(spawner);
                }
                if let Some(runtime_turn) = self.runtime_turn {
                    context.set_runtime_turn_context(runtime_turn);
                }
                context
            }
        };
        Executor::new_with_authorization(
            self.registry,
            self.permissions,
            self.approver,
            context,
            self.authorization,
        )
        .with_hooks(self.hooks)
        .with_disabled_ops(self.disabled_ops)
    }
}

/// Compatibility adapter from the catalog declaration to exact invocation requirements.
///
/// Effects by themselves are not resources. In particular, a generic `Read` can describe a pure
/// transform, evidence lookup, datasource query, or filesystem read, so it never implies
/// `workspace.read`. Concrete access kinds select the resource family; richer tools override
/// [`Tool::authority_requirements`]. Inconsistent declarations and unknown semantic actions fail
/// closed instead of being silently dropped.
fn push_requirement(
    requirements: &mut Vec<AuthorityRequirement>,
    requirement: AuthorityRequirement,
) {
    if !requirements.contains(&requirement) {
        requirements.push(requirement);
    }
}

/// Normalize the untyped compatibility subjects that can identify ordinary resource families.
///
/// `datasource:` is the one typed subject carried through this legacy seam: it feeds the separate
/// datasource/`flow.write_db` requirements and must never become a network, process, browser, or
/// provider identity. Blank and wildcard placeholders are not concrete; duplicates are collapsed
/// while preserving declaration order.
fn concrete_authority_subjects(subjects: &[String]) -> Vec<&str> {
    let mut concrete = Vec::new();
    for subject in subjects {
        let subject = subject.trim();
        if subject.is_empty() || subject == "*" || subject.starts_with("datasource:") {
            continue;
        }
        if !concrete.contains(&subject) {
            concrete.push(subject);
        }
    }
    concrete
}

fn push_declared_resource_requirements(
    requirements: &mut Vec<AuthorityRequirement>,
    subjects: &[String],
    action: &'static str,
    kind: ResourceKind,
) {
    let subjects = concrete_authority_subjects(subjects);
    if subjects.is_empty() {
        push_requirement(
            requirements,
            AuthorityRequirement::new(action, ResourceRef::any(kind)),
        );
    } else {
        for subject in subjects {
            push_requirement(
                requirements,
                AuthorityRequirement::new(action, ResourceRef::named(kind, subject)),
            );
        }
    }
}

/// Derive exact requirements from a catalog declaration plus invocation-level subjects.
///
/// Most built-in tools use this through [`Tool::authority_requirements`]. Adapters such as the
/// plugin host may call it and then replace coarse manifest-wide resources with their exact
/// declared hosts, programs, or connection targets.
pub fn authority_requirements_from_declaration(
    spec: &ToolSpec,
    subjects: &[String],
    semantic_effects: &[String],
) -> Result<Vec<AuthorityRequirement>> {
    let has_effect = |effect| spec.effects.contains(&effect);
    let has_access = |access| spec.access.contains(&access);
    let mut requirements = Vec::new();
    let concrete_subjects = || {
        if subjects.is_empty() {
            vec!["".to_string()]
        } else {
            subjects.to_vec()
        }
    };

    if has_access(AccessKind::Filesystem) {
        if has_effect(Effect::Write) {
            for subject in concrete_subjects() {
                push_requirement(
                    &mut requirements,
                    AuthorityRequirement::workspace_write(subject),
                );
            }
        } else if has_effect(Effect::Read) || has_effect(Effect::Filesystem) {
            for subject in concrete_subjects() {
                push_requirement(
                    &mut requirements,
                    AuthorityRequirement::workspace_read(subject),
                );
            }
        } else {
            return Err(Error::Other(format!(
                "tool `{}` declares filesystem access without a read/write effect",
                spec.name
            )));
        }
    }
    if has_access(AccessKind::Process) {
        push_declared_resource_requirements(
            &mut requirements,
            subjects,
            "process.exec",
            ResourceKind::Process,
        );
    }
    if has_access(AccessKind::Network) {
        push_declared_resource_requirements(
            &mut requirements,
            subjects,
            "network.fetch",
            ResourceKind::Network,
        );
    }
    if has_access(AccessKind::Connection) {
        if subjects.is_empty() {
            push_requirement(
                &mut requirements,
                AuthorityRequirement::new(
                    "connection.dial",
                    ResourceRef::any(ResourceKind::Connection),
                ),
            );
        } else {
            for subject in subjects {
                push_requirement(
                    &mut requirements,
                    AuthorityRequirement::new(
                        "connection.dial",
                        ResourceRef::named(ResourceKind::Connection, subject),
                    ),
                );
            }
        }
    }
    if has_access(AccessKind::Datasource) {
        let action = if has_effect(Effect::Write)
            || semantic_effects.iter().any(|effect| effect == "write_db")
        {
            "datasource.write"
        } else {
            "datasource.read"
        };
        if subjects.is_empty() {
            push_requirement(
                &mut requirements,
                AuthorityRequirement::new(action, ResourceRef::any(ResourceKind::Datasource)),
            );
        } else {
            for subject in subjects {
                let subject = subject.strip_prefix("datasource:").unwrap_or(subject);
                push_requirement(
                    &mut requirements,
                    AuthorityRequirement::new(
                        action,
                        ResourceRef::named(ResourceKind::Datasource, subject),
                    ),
                );
            }
        }
    }
    if has_access(AccessKind::Browser) {
        push_declared_resource_requirements(
            &mut requirements,
            subjects,
            "browser.navigate",
            ResourceKind::Network,
        );
    }
    if has_access(AccessKind::Provider) {
        push_declared_resource_requirements(
            &mut requirements,
            subjects,
            "model.invoke",
            ResourceKind::Provider,
        );
    }
    if has_access(AccessKind::Secret) {
        if subjects.is_empty() {
            push_requirement(
                &mut requirements,
                AuthorityRequirement::new("secret.read", ResourceRef::any(ResourceKind::Secret)),
            );
        } else {
            for subject in subjects {
                push_requirement(
                    &mut requirements,
                    AuthorityRequirement::new(
                        "secret.read",
                        ResourceRef::named(ResourceKind::Secret, subject),
                    ),
                );
            }
        }
    }
    if has_access(AccessKind::Auth) {
        push_requirement(
            &mut requirements,
            AuthorityRequirement::new("host.read", ResourceRef::named(ResourceKind::Host, "auth")),
        );
    }
    if has_access(AccessKind::LocalSystem) && !has_access(AccessKind::Process) {
        let action = if has_effect(Effect::Write) || has_effect(Effect::LocalSystem) {
            "host.write"
        } else {
            "host.read"
        };
        push_requirement(
            &mut requirements,
            AuthorityRequirement::new(action, ResourceRef::named(ResourceKind::Host, &spec.name)),
        );
    }

    if has_effect(Effect::Filesystem) && !has_access(AccessKind::Filesystem) {
        return Err(Error::Other(format!(
            "tool `{}` declares a filesystem effect without filesystem access",
            spec.name
        )));
    }
    if has_effect(Effect::Process) && !has_access(AccessKind::Process) {
        return Err(Error::Other(format!(
            "tool `{}` declares a process effect without process access",
            spec.name
        )));
    }
    if has_effect(Effect::Browser) && !has_access(AccessKind::Browser) {
        return Err(Error::Other(format!(
            "tool `{}` declares a browser effect without browser access",
            spec.name
        )));
    }
    // `Process` is a carrier here because a subprocess reaches the network on the tool's behalf —
    // `git_push` shells `git`, the kubernetes plugin shells `kubectl`. The gate is the
    // `process.exec` requirement on the named program pushed above, not a network resource, so the
    // envelope still names something concrete. Without this an integration whose only capability is
    // an allow-listed CLI would have to claim network access it never uses in order to register.
    if has_effect(Effect::Network)
        && !has_access(AccessKind::Network)
        && !has_access(AccessKind::Connection)
        && !has_access(AccessKind::Browser)
        && !has_access(AccessKind::Provider)
        && !has_access(AccessKind::Process)
    {
        return Err(Error::Other(format!(
            "tool `{}` declares a network effect without network, process, browser, or provider access",
            spec.name
        )));
    }
    if has_effect(Effect::LocalSystem)
        && !has_access(AccessKind::LocalSystem)
        && !has_access(AccessKind::Process)
    {
        return Err(Error::Other(format!(
            "tool `{}` declares a host-state effect without local-system access",
            spec.name
        )));
    }
    if has_effect(Effect::Write)
        && !has_access(AccessKind::Filesystem)
        && !has_access(AccessKind::LocalSystem)
    {
        // Same reasoning as the network carrier above: a subprocess mutates remote state the tool
        // has no typed resource for (a cluster deployment, a cloud resource). The mutation is
        // pinned to the operation itself, on top of the `process.exec` gate on the program.
        if has_access(AccessKind::Network)
            || has_access(AccessKind::Connection)
            || has_access(AccessKind::Browser)
            || has_access(AccessKind::Process)
        {
            push_requirement(
                &mut requirements,
                AuthorityRequirement::operation("operation.mutate", &spec.name),
            );
        } else if !semantic_effects
            .iter()
            .any(|effect| matches!(effect.as_str(), "write_db" | "delete"))
        {
            return Err(Error::Other(format!(
                "tool `{}` declares a write effect without a typed write resource",
                spec.name
            )));
        }
    }

    for effect in semantic_effects {
        match effect.as_str() {
            "pure" | "read" | "human_visible" => {}
            "model" => push_declared_resource_requirements(
                &mut requirements,
                subjects,
                "model.invoke",
                ResourceKind::Provider,
            ),
            "network" => push_declared_resource_requirements(
                &mut requirements,
                subjects,
                "network.fetch",
                ResourceKind::Network,
            ),
            "write_file" => {
                if !requirements
                    .iter()
                    .any(|req| req.action.0 == "workspace.write")
                {
                    return Err(Error::Other(format!(
                        "tool `{}` declares `write_file` without a filesystem write resource",
                        spec.name
                    )));
                }
            }
            "write_db" => {
                let datasource_subjects: Vec<&str> = subjects
                    .iter()
                    .filter_map(|subject| subject.strip_prefix("datasource:"))
                    .collect();
                if datasource_subjects.is_empty() {
                    return Err(Error::Other(format!(
                        "tool `{}` declares `write_db` without a `datasource:` subject",
                        spec.name
                    )));
                }
                for subject in datasource_subjects {
                    push_requirement(
                        &mut requirements,
                        AuthorityRequirement::new(
                            "flow.write_db",
                            ResourceRef::named(ResourceKind::Datasource, subject),
                        ),
                    );
                }
            }
            "send_external" => {
                push_declared_resource_requirements(
                    &mut requirements,
                    subjects,
                    "network.fetch",
                    ResourceKind::Network,
                );
                push_requirement(
                    &mut requirements,
                    AuthorityRequirement::operation("flow.send_external", &spec.name),
                );
            }
            "delete" => push_requirement(
                &mut requirements,
                AuthorityRequirement::operation("flow.delete", &spec.name),
            ),
            "money" => push_requirement(
                &mut requirements,
                AuthorityRequirement::operation("flow.money", &spec.name),
            ),
            // Deprecated (C-184): kept parseable so a manifest that declares it still loads, but
            // `flow.calendar` has no default-policy grant — the op is default-deny unless a policy
            // grants it explicitly. Removed at the next protocol major.
            "calendar" => push_requirement(
                &mut requirements,
                AuthorityRequirement::operation("flow.calendar", &spec.name),
            ),
            unknown => {
                return Err(Error::Other(format!(
                    "tool `{}` declares unknown semantic authority `{unknown}`",
                    spec.name
                )));
            }
        }
    }

    Ok(requirements)
}

/// The dispatcher: runs pre-tool hooks, enforces the authorization policy + permission rules +
/// approval, then executes through the guarded system.
pub struct Executor {
    registry: ToolRegistry,
    perms: Mutex<PermissionManager>,
    /// Interior-mutable so a surface can swap the approver (e.g. the TUI's modal) even when the executor
    /// is shared as an `Arc<Executor>` — which it is once the authored loop host is installed.
    approver: Mutex<Arc<dyn Approver>>,
    ctx: ToolContext,
    hooks: Vec<Arc<dyn PreToolHook>>,
    /// The mandatory authorization floor. Approval and permission rules may narrow it, never
    /// disable it.
    policy: AuthorizationPolicy,
    /// The immutable assembly-time identity. Engine-driven turns override it only through their
    /// lexical [`RuntimeTurnContext`], never by mutating this executor.
    identity: IdentityCell,
    /// Depth of the active "pre-approved plan" scope. `>0` means the ops being dispatched belong to a
    /// plan the user already approved as a whole, so the per-op approval gate is skipped (deny rules
    /// still win). A depth (not a bool) so a plan that runs a nested plan stays approved throughout.
    plan_scope: AtomicU32,
    /// Stack of approved-plan scopes' destructive-disclosure flags, one frame per currently-open
    /// scope in nesting order (pushed by [`Executor::enter_approved_scope`], popped when its guard
    /// drops). A frame is `true` iff that scope's own approval DISCLOSED a destructive op (the
    /// plan's risk preview carried `destructive: true`, so whoever approved it saw the badge). The
    /// undisclosed-destructive gate keys on the INNERMOST (top-of-stack) frame only — a bare shared
    /// depth counter would let a nested plan approved `destructive:false` inherit an outer scope's
    /// disclosure (C-27). While the innermost frame is `false` (or the stack is empty), a
    /// destructive-intent op re-fires the per-op approval gate even inside an approved scope — the
    /// closed loophole is a destructive command assembled from `$symbols` at runtime, invisible to
    /// the static plan risk that the approval was based on.
    destructive_scope: Mutex<Vec<bool>>,
    /// Set when the user answered `always` at a plan prompt: every subsequent plan this session runs
    /// without asking. Deliberately does NOT disclose destructiveness: a statically-visible
    /// destructive plan still discloses per plan via its scope guard, and a runtime-assembled
    /// destructive op still asks — "trust all plans" is not "never ask about `rm -rf` again".
    trust_all: AtomicBool,
    /// Content-addressed result cache for deterministic read-only ops (L-54). Keyed on op
    /// identity + canonical input JSON + input-schema fingerprint + the invalidation-domain
    /// generation below. Sits AFTER the whole authorization → approval envelope in
    /// [`Executor::dispatch_outcome`], so a hit is served only to a caller the op is *currently*
    /// admissible for; only redacted, successful results are stored.
    op_cache: Mutex<HashMap<u64, ToolResult>>,
    /// The invalidation-domain generation: every dispatch carrying a non-`Read` effect (a
    /// workspace/process/network mutation — conservatively, anything that could change what a
    /// read observes) starts a new generation. Keys embed the generation, so all older entries
    /// become unreachable at once.
    cache_gen: AtomicU64,
    /// Monotonic correlation id for lifecycle observations emitted by each dispatch.
    dispatch_seq: AtomicU64,
    /// `FLUX_OP_CACHE=off|0` kill switch (resolved at construction); `with_op_cache` overrides.
    cache_enabled: bool,
    /// `[tools] disable` (C-162), resolved to concrete op names once at construction time via
    /// [`ToolRegistry::resolve_disabled`] — never recomputed mid-session, so it can't churn the
    /// prompt prefix (the A-95 cache-stability lesson). Consulted twice: the engine layer narrows
    /// the per-turn advertised set by it via [`Executor::disabled_ops`] (surface-only), and
    /// [`Executor::gate`] refuses a dispatch that names one directly, so a cached plan or a resumed
    /// session can't call it either. Surface-only and defense-in-depth — the authorization policy is
    /// still the actual security control and wins if the two ever disagree; this never widens what a
    /// call may do, only what is offered.
    disabled_ops: HashSet<String>,
}

/// Holds an approved-plan scope open. While alive, [`Executor::dispatch`] skips the per-op approval
/// prompt; `Drop` closes the scope (decrementing the depth so re-planning asks again next round).
/// When the plan's approval disclosed a destructive op, the guard also holds the destructive
/// disclosure open (see [`Executor::enter_approved_scope`]).
pub struct PlanScopeGuard<'a> {
    plan: &'a AtomicU32,
    /// The disclosure stack this guard pushed its own frame onto; popped on drop so the innermost
    /// frame always reflects the currently-active scope, never a closed one.
    destructive: &'a Mutex<Vec<bool>>,
}

impl Drop for PlanScopeGuard<'_> {
    fn drop(&mut self) {
        self.plan.fetch_sub(1, Ordering::SeqCst);
        self.destructive.lock().unwrap().pop();
    }
}

/// Holds a capability scope open (see [`Executor::push_cap_scope`]). `Drop` pops it unconditionally —
/// on normal completion, an early `return`, or a propagating error — so the stack always unwinds to
/// the outer scope's allowlist no matter how the `with_tools` body exits. Also records the
/// `cap_scope_exit` evidence observation on drop, mirroring `push_cap_scope`'s `cap_scope_enter` — so
/// enter/exit bracket the body exactly like the stack push/pop do, with the same unconditional
/// guarantee.
pub struct CapScopeGuard<'a> {
    cap_scopes: &'a Mutex<Vec<Vec<String>>>,
    evidence: &'a Mutex<EvidenceLog>,
}

/// The full outcome of [`Executor::dispatch_outcome`]: the ordinary [`ToolResult`] every caller
/// already gets from [`Executor::dispatch`], plus a **structural** flag for whether the envelope
/// itself refused to run the op.
///
/// L-32: before this existed, a denial was inferred downstream by prefix-matching `content` against
/// the envelope's own refusal wording (`` `{op}` denied by `` ) — so an op that *ran* and merely
/// relayed foreign text shaped like that wording (e.g. a wrapped CLI surfacing its own "denied by"
/// stderr) was misclassified as a deliberate authorization refusal and escalated to a fatal,
/// never-retried error. `denied` is set at the exact call site inside [`Executor::dispatch_outcome`]
/// that refuses the call, so classification never has to guess from prose again.
pub struct DispatchOutcome {
    pub result: ToolResult,
    /// `true` iff the envelope itself refused to run the op: an unknown tool name (D-184 — matches
    /// [`Executor::authorize`]'s `Deny`, since a typo'd op can never succeed no matter how many times
    /// `retry` tries it), an op named in `[tools] disable` (C-162 — surface-only, but refused at
    /// dispatch too as defense-in-depth), a capability-scope miss, the authorization policy floor, a
    /// permission-rule deny, or the approver declining. A pre-tool hook's `Deny` is deliberately
    /// excluded — hook denials are meant to stay retryable/repairable rather than a terminal
    /// authorization refusal, exactly as before this flag existed (hook denials never matched the
    /// old prefix heuristic either, since their wording is `` `{op}` blocked by hook `` , not
    /// `` `{op}` denied by `` ).
    pub denied: bool,
    /// Monotonic phase attribution measured inside the safety envelope.
    pub timing: OperationTiming,
}

/// The verdict of [`Executor::authorize`] (D-177) — an authorization **decision**, taken without
/// dispatching anything.
///
/// `#[non_exhaustive]`: a new gate that can refuse a call for a new reason lands as a new variant
/// (or a new refusal message), and a caller must not assume today's three are all there will be.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthorizeVerdict {
    /// Admissible under the current capability scope, authorization policy, and permission rules —
    /// and the envelope would not prompt for it.
    Allow,
    /// Admissible, but the envelope would gate it through the approver before running it: a
    /// destructive op, an effect the policy marked `requires_approval`, an unscoped write, or a call
    /// the permission rules only "ask" for.
    ApprovalRequired,
    /// Refused, carrying the same message the live envelope returns for this refusal.
    Deny(String),
}

impl AuthorizeVerdict {
    /// Whether the envelope refused the call outright.
    pub fn is_denied(&self) -> bool {
        matches!(self, AuthorizeVerdict::Deny(_))
    }

    /// The refusal message, if this is a [`Deny`](Self::Deny).
    pub fn reason(&self) -> Option<&str> {
        match self {
            AuthorizeVerdict::Deny(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Whether [`Executor::gate`] is running on the live dispatch path (which records its audit
/// observations) or serving an authorize-only decision (which records nothing — a hypothetical call
/// has no business writing to the audit log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateAudit {
    Live,
    DecisionOnly,
}

/// Everything the deterministic gates computed for a call that passed them — handed back to the
/// live path so it doesn't recompute any of it, and read by [`Executor::authorize`] to classify the
/// verdict.
struct GatedCall {
    spec: flux_spec::ToolSpec,
    subjects: Vec<String>,
    intents: IntentSet,
    perm: PermDecision,
    policy_requires_approval: bool,
    identity: TurnIdentity,
}

impl Drop for CapScopeGuard<'_> {
    fn drop(&mut self) {
        let popped = self.cap_scopes.lock().unwrap().pop();
        self.evidence.lock().unwrap().record(Observation::new(
            "cap_scope_exit",
            Phase::Turn,
            json!({ "scope": popped }),
        ));
    }
}

impl Executor {
    fn record_dispatch_event(
        &self,
        kind: &str,
        dispatch: u64,
        name: &str,
        started: Instant,
        extra: serde_json::Value,
    ) {
        let mut data = serde_json::Map::from_iter([
            ("dispatch".to_string(), json!(dispatch)),
            ("tool".to_string(), json!(name)),
            (
                "elapsed_us".to_string(),
                json!(started.elapsed().as_micros().min(u64::MAX as u128) as u64),
            ),
        ]);
        if let Some(fields) = extra.as_object() {
            data.extend(fields.clone());
        }
        self.ctx.evidence.lock().unwrap().record(Observation::new(
            kind,
            Phase::Turn,
            Value::Object(data),
        ));
    }

    fn finish_dispatch(
        &self,
        _name: &str,
        started: Instant,
        approval_wait: Option<Duration>,
        execution: Option<Duration>,
        result: ToolResult,
        denied: bool,
    ) -> DispatchOutcome {
        let timing = OperationTiming::from_durations(started.elapsed(), approval_wait, execution);
        DispatchOutcome {
            result,
            denied,
            timing,
        }
    }

    /// Construct under the documented local single-user authorization profile.
    ///
    /// This convenience door still installs a mandatory policy floor; it never disables
    /// authorization. Multi-user and service surfaces should resolve their caller and use
    /// [`new_with_authorization`](Self::new_with_authorization) instead.
    pub fn new(
        registry: ToolRegistry,
        perms: PermissionManager,
        approver: Arc<dyn Approver>,
        ctx: ToolContext,
    ) -> Self {
        Self::new_with_authorization(
            registry,
            perms,
            approver,
            ctx,
            ExecutionAuthorization::local(),
        )
    }

    /// Construct with an explicit mandatory authorization floor and identity profile.
    pub fn new_with_authorization(
        registry: ToolRegistry,
        perms: PermissionManager,
        approver: Arc<dyn Approver>,
        ctx: ToolContext,
        authorization: ExecutionAuthorization,
    ) -> Self {
        Self {
            registry,
            perms: Mutex::new(perms),
            approver: Mutex::new(approver),
            ctx,
            hooks: Vec::new(),
            policy: authorization.policy,
            identity: authorization.identity,
            plan_scope: AtomicU32::new(0),
            destructive_scope: Mutex::new(Vec::new()),
            trust_all: AtomicBool::new(false),
            op_cache: Mutex::new(HashMap::new()),
            cache_gen: AtomicU64::new(0),
            dispatch_seq: AtomicU64::new(1),
            cache_enabled: std::env::var("FLUX_OP_CACHE")
                .map(|v| v != "off" && v != "0")
                .unwrap_or(true),
            disabled_ops: HashSet::new(),
        }
    }

    /// Enable/disable the deterministic read-only op cache (overrides `FLUX_OP_CACHE`).
    pub fn with_op_cache(mut self, on: bool) -> Self {
        self.cache_enabled = on;
        self
    }

    /// Install the resolved `[tools] disable` set (C-162) — typically
    /// [`ToolRegistry::resolve_disabled`]'s `disabled` field, computed once against this executor's
    /// own registry. Empty by default (nothing disabled). See the `disabled_ops` field doc for how
    /// this is consulted.
    pub fn with_disabled_ops(mut self, disabled: HashSet<String>) -> Self {
        self.disabled_ops = disabled;
        self
    }

    /// The resolved `[tools] disable` set this executor was built with (C-162) — the concrete op
    /// names to exclude from the advertised set and refuse at dispatch. Empty when nothing is
    /// disabled.
    pub fn disabled_ops(&self) -> &HashSet<String> {
        &self.disabled_ops
    }

    /// Turn boundary for the op cache (L-54): the engine calls this at the start of every user
    /// turn. Between turns anything outside the runtime (the user's editor, another process) may
    /// have mutated what a read observes — the executor's write-generation only tracks its OWN
    /// dispatches — so the cache's reuse window is deliberately bounded to one turn: repair
    /// rounds, retries, and nested plans within it.
    pub fn begin_cache_turn(&self) {
        self.cache_gen.fetch_add(1, Ordering::SeqCst);
        self.op_cache.lock().unwrap().clear();
    }

    /// Whether we're currently executing the ops of an already-approved plan (or the user trusts all
    /// plans). When true, [`dispatch`](Self::dispatch) skips the per-op approval prompt.
    pub fn in_approved_scope(&self) -> bool {
        self.trust_all.load(Ordering::SeqCst) || self.plan_scope.load(Ordering::SeqCst) > 0
    }

    /// Open a pre-approved scope for the duration of the returned guard — used when the act of running
    /// *is* the approval (the REPL `/run`, where the human already reviewed the plan). Inner ops dispatch
    /// without prompting; the guard closes the scope on drop. `destructive_disclosed` says whether the
    /// reviewed plan's risk preview showed a destructive op: pass the preview's `destructive` flag so a
    /// destructive op the human saw doesn't re-prompt, while one assembled at runtime still does.
    pub fn enter_approved_scope(&self, destructive_disclosed: bool) -> PlanScopeGuard<'_> {
        self.plan_scope.fetch_add(1, Ordering::SeqCst);
        // Always push a frame — even `false` — so the stack's depth tracks `plan_scope` exactly and
        // the innermost frame always reflects THIS scope's own disclosure, never an ancestor's.
        self.destructive_scope
            .lock()
            .unwrap()
            .push(destructive_disclosed);
        PlanScopeGuard {
            plan: &self.plan_scope,
            destructive: &self.destructive_scope,
        }
    }

    /// Approve a whole plan once, then keep it pre-approved while the returned guard is held. If already
    /// inside an approved scope (a nested authored flow) or the user trusts all plans, returns a guard
    /// without prompting. `None` means the approver rejected the plan. The request comes from the plan's
    /// risk preview and carries the content an interactive approver needs to present it. The
    /// scope's destructive disclosure follows the request's `destructive` flag on every arm: whoever
    /// approved (or pre-trusted) the plan did so against a preview that carried that badge.
    pub async fn approve_plan(&self, plan: &PlanApprovalRequest) -> Option<PlanScopeGuard<'_>> {
        if self.in_approved_scope() {
            return Some(self.enter_approved_scope(plan.destructive));
        }
        let approver = self.approver.lock().unwrap().clone();
        match approver.request_plan(plan).await {
            ApprovalChoice::Allow => Some(self.enter_approved_scope(plan.destructive)),
            ApprovalChoice::AllowAlways(_) => {
                self.trust_all.store(true, Ordering::SeqCst);
                Some(self.enter_approved_scope(plan.destructive))
            }
            ApprovalChoice::Deny | ApprovalChoice::DenyWithReason(_) => None,
        }
    }

    /// Ask for aggregate approval without opening an execution scope. Adaptive loops use this to
    /// mint a one-shot receipt in one stage and execute in a later stage; the caller must validate
    /// that receipt and call [`enter_approved_scope`](Self::enter_approved_scope) only while the
    /// exact approved batch is dispatched.
    pub async fn request_plan_approval(&self, plan: &PlanApprovalRequest) -> bool {
        if self.in_approved_scope() {
            return true;
        }
        let approver = self.approver.lock().unwrap().clone();
        match approver.request_plan(plan).await {
            ApprovalChoice::Allow => true,
            ApprovalChoice::AllowAlways(_) => {
                self.trust_all.store(true, Ordering::SeqCst);
                true
            }
            ApprovalChoice::Deny | ApprovalChoice::DenyWithReason(_) => false,
        }
    }

    /// Stable snapshot of the authority context an approval was made under. It contains no secret
    /// values: only caller/trust/policy metadata, allow rules, and the active capability ceiling.
    /// Receipt owners bind this byte string at approval and require an exact match at execution;
    /// dispatch still re-evaluates every policy and permission rule afterward.
    pub fn approval_context(&self) -> String {
        let identity = self.effective_identity();
        serde_json::to_string(&json!({
            "caller": identity.caller(),
            "trust": identity.trust(),
            "policy": self.policy,
            "allow_rules": self.perms.lock().unwrap().allow_rules(),
            "capability_scope": self.active_cap_scope(),
        }))
        .unwrap_or_default()
    }

    /// The effective tool-name allowlist of the innermost active `with_tools` scope, or `None` when no
    /// scope is active. Delegates to the shared [`ToolContext::active_cap_scope`] so a spawned
    /// sub-agent (built over a fresh `Executor` but a context that still carries this same `Arc`) sees
    /// the identical set [`Executor::dispatch`] just checked.
    pub fn active_cap_scope(&self) -> Option<Vec<String>> {
        self.ctx.active_cap_scope()
    }

    /// Whether an operation may be surfaced for argument selection. This is a visibility ceiling,
    /// not authorization: literal dispatch still rechecks subject-scoped rules, policy, hooks, and
    /// approval. A bare deny or an active `with_tools` miss is knowable before arguments exist and
    /// therefore removes the operation from model context entirely.
    pub fn operation_visible(&self, name: &str) -> bool {
        if self
            .active_cap_scope()
            .is_some_and(|scope| !scope.iter().any(|allowed| allowed == name))
        {
            return false;
        }
        !self.perms.lock().unwrap().is_bare_denied(name)
    }

    /// Push a new capability scope, **narrowing** the effective allowlist: the pushed set is
    /// intersected with the current top-of-stack (if any), so capabilities can only shrink as scopes
    /// nest — an inner `with_tools` can never re-grant a tool an outer scope removed. Records a
    /// `cap_scope_enter` evidence observation, and returns the guard that pops the scope (and records
    /// `cap_scope_exit`) on drop; hold it across the scope's body so the pop is guaranteed even if the
    /// body errors (mirrors [`PlanScopeGuard`]/the flux-lang `Scope` node's RAII discipline).
    pub fn push_cap_scope(&self, tools: &[String]) -> CapScopeGuard<'_> {
        let mut stack = self.ctx.cap_scopes.lock().unwrap();
        let effective: Vec<String> = match stack.last() {
            Some(outer) => tools
                .iter()
                .filter(|t| outer.contains(t))
                .cloned()
                .collect(),
            None => tools.to_vec(),
        };
        stack.push(effective.clone());
        drop(stack);
        self.ctx.evidence.lock().unwrap().record(Observation::new(
            "cap_scope_enter",
            Phase::Turn,
            json!({ "requested": tools, "effective": effective }),
        ));
        CapScopeGuard {
            cap_scopes: &self.ctx.cap_scopes,
            evidence: &self.ctx.evidence,
        }
    }

    /// Attach ordered pre-tool hooks (run before the permission gate).
    pub fn with_hooks(mut self, hooks: Vec<Arc<dyn PreToolHook>>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Replace the approval handler (e.g. a surface installing its own interactive approver before
    /// driving turns — the TUI swaps in a modal approver).
    pub fn set_approver(&self, approver: Arc<dyn Approver>) {
        *self.approver.lock().unwrap() = approver;
    }

    /// Install the [`LoopHost`] capability onto this executor's [`ToolContext`], so authored-loop
    /// stages can consult the model, run nested authored flows, and execute approved batches. Done
    /// by the engine once per turn, after the executor is built (the host holds a `Weak` back to
    /// this same executor, so it can only be wired in afterwards).
    pub fn set_loop_host(&mut self, loop_host: Arc<dyn LoopHost>) {
        self.ctx.loop_host = Some(loop_host);
    }

    /// Install the composite registration capability onto this executor's context.
    pub fn set_composite_registrar(&mut self, registrar: Arc<dyn CompositeRegistrar>) {
        self.ctx.composite_registrar = Some(registrar);
    }

    /// Install the on-demand skill-body loading capability (D-188) onto this executor's context.
    pub fn set_skill_loader(&mut self, loader: Arc<dyn SkillLoader>) {
        self.ctx.skill_loader = Some(loader);
    }

    /// Pre-allow these op names (they dispatch without an approval prompt). The engine uses this to
    /// whitelist its own loop machinery (`detect_intent`/`explore`/`approve_batch`/…) — internal
    /// control flow, not user-facing actions. A `deny` rule still wins, and leaf ops still gate
    /// individually through dispatch.
    pub fn allow(&self, rules: &[&str]) {
        let mut perms = self.perms.lock().unwrap();
        for r in rules {
            perms.add_allow(r);
        }
    }

    /// The current approver (used by flow nodes such as `confirm` that need to request approval
    /// outside of a full tool dispatch). Returns a clone of the `Arc` (the approver is interior-mutable).
    pub fn approver(&self) -> Arc<dyn Approver> {
        self.approver.lock().unwrap().clone()
    }

    /// Replace the mandatory authorization-policy floor. Every tool call's effects are evaluated
    /// against it (default-deny) before the permission rules run.
    pub fn with_policy(mut self, policy: AuthorizationPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Replace the policy and fixed identity atomically.
    pub fn with_authorization(
        mut self,
        policy: AuthorizationPolicy,
        caller: Caller,
        trust: Trust,
    ) -> Self {
        let authorization = ExecutionAuthorization::new(policy, caller, trust);
        self.policy = authorization.policy;
        self.identity = authorization.identity;
        self
    }

    /// Replace the policy and shared immutable identity snapshot atomically.
    pub fn with_authorization_cell(
        mut self,
        policy: AuthorizationPolicy,
        identity: IdentityCell,
    ) -> Self {
        self.policy = policy;
        self.identity = identity;
        self
    }

    /// Set the resolved caller + trust the policy evaluates against (default: the local
    /// single-user identity). Surfaces resolve this via `flux-auth` before constructing the agent.
    /// Replaces the identity snapshot with a fresh, unshared one — to share the assembly-time
    /// fallback with a spawner, use [`with_identity_cell`](Self::with_identity_cell).
    pub fn with_identity(mut self, caller: Caller, trust: Trust) -> Self {
        self.identity = IdentityCell::new(caller, trust);
        self
    }

    /// Share an externally-owned immutable identity snapshot with sibling assembly components.
    pub fn with_identity_cell(mut self, cell: IdentityCell) -> Self {
        self.identity = cell;
        self
    }

    /// The shared immutable assembly-time identity handle.
    pub fn identity(&self) -> IdentityCell {
        self.identity.clone()
    }

    /// The identity effective in the current lexical turn, or the immutable assembly-time default
    /// when no turn scope is active.
    pub fn effective_identity(&self) -> TurnIdentity {
        self.ctx
            .turn_identity()
            .unwrap_or_else(|| self.identity.snapshot())
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// The execution context (guarded system, redactor, spawner). Lets a caller derive a sibling
    /// executor over the *same* guarded surface — e.g. a read-only research executor scoped to a
    /// subset of tools for the planner.
    pub fn context(&self) -> &ToolContext {
        &self.ctx
    }

    /// The current allow rules (for persistence by the caller).
    pub fn allow_rules(&self) -> Vec<String> {
        self.perms.lock().unwrap().allow_rules()
    }

    /// Record an externally-derived observation (e.g. a startup toolchain scan) into the shared log.
    pub fn observe(&self, observation: Observation) {
        self.ctx.evidence.lock().unwrap().record(observation);
    }

    /// A snapshot of the evidence log accumulated so far (shared with the context, so flow-emitted
    /// `observe(…)` observations are part of this same trail).
    pub fn evidence(&self) -> EvidenceLog {
        self.ctx.evidence.lock().unwrap().clone()
    }

    /// Run a tool call through the full safety envelope.
    pub async fn dispatch(&self, name: &str, params: Value) -> ToolResult {
        self.dispatch_outcome(name, params).await.result
    }

    /// **Authorize-only** (D-177): would this call be admissible right now — and if not, why?
    ///
    /// Runs exactly the deterministic gates of [`dispatch_outcome`](Self::dispatch_outcome) — the
    /// capability-scope floor, filesystem subject normalization, the authority contract, the
    /// mandatory authorization-policy floor, and the permission rules — and **stops there**. It
    /// shares one implementation with the live path (`gate`), so the two can't drift; drift here
    /// would be a safety bug, not a cosmetic one.
    ///
    /// This is a *decision*, not a dispatch. It is deliberately **synchronous**, which is what makes
    /// the "no execution side effect" property structural rather than a promise: `Tool::execute` and
    /// `Approver::request` are both `async`, so neither is reachable from a non-`async` function.
    /// Nothing else is touched either — no evidence observation is recorded, no permission rule is
    /// added, no cache generation is bumped, the approver is never consulted.
    ///
    /// **Not a bypass, and not a substitute for dispatching.** A verdict here grants nothing: the
    /// real call still goes through the full envelope, which re-decides everything from scratch and
    /// additionally runs the pre-tool hooks (skipped here — a hook may rewrite `params`, and running
    /// hooks for a hypothetical call would be a real side effect) and the approval gate. `Allow` from
    /// this function means "the deterministic gates admit it", never "it may now run unchecked".
    pub fn authorize(&self, name: &str, params: &Value) -> AuthorizeVerdict {
        let Some(tool) = self.registry.get(name) else {
            return AuthorizeVerdict::Deny(format!("unknown tool: {name}"));
        };
        match self.gate(name, params, tool.as_ref(), GateAudit::DecisionOnly) {
            Err(reason) => AuthorizeVerdict::Deny(reason),
            Ok(call) => {
                // Mirrors `dispatch_outcome`'s `approval_sensitive`, minus the evidence-driven
                // destructive escalation (recording evidence is a side effect) — a destructive op is
                // reported as approval-gated on its own `Risk`/intent, which is the same conclusion
                // the escalation reaction reaches for it.
                let unscoped_write =
                    call.spec.effects.contains(&Effect::Write) && call.subjects.is_empty();
                if call.policy_requires_approval
                    || call.spec.risk == Risk::Destructive
                    || call.intents.is_destructive()
                    || unscoped_write
                    || call.perm != PermDecision::Allow
                {
                    AuthorizeVerdict::ApprovalRequired
                } else {
                    AuthorizeVerdict::Allow
                }
            }
        }
    }

    /// The deterministic authorization gates shared by [`dispatch_outcome`](Self::dispatch_outcome)
    /// (which then goes on to evidence, approval, and execution) and
    /// [`authorize`](Self::authorize) (which stops here). `Err` is the refusal message the live path
    /// returns verbatim, so the two surfaces always agree on *why*, not just *whether*.
    ///
    /// `audit` exists only because the live path records a `cap_scope_denied` observation that an
    /// authorize-only decision must not: a hypothetical call has no business writing to the audit
    /// log.
    ///
    /// The capability-scope floor is factored out into [`cap_scope_gate`](Self::cap_scope_gate)
    /// because the live path must check it BEFORE the pre-tool hooks run (a hook must not observe —
    /// or rewrite the input of — a call the scope already forbids), while everything below needs the
    /// hooks' possibly-rewritten `params`. Checking it twice on the live path is harmless: it is a
    /// pure read of the scope stack, and a denial returns before the second check.
    fn gate(
        &self,
        name: &str,
        params: &Value,
        tool: &dyn Tool,
        audit: GateAudit,
    ) -> std::result::Result<GatedCall, String> {
        // C-162: `[tools] disable` is checked first, unconditionally — before the capability-scope
        // floor, hooks, policy, or permission rules — so a disabled op is refused the same way
        // regardless of scope/rules/plan. This is deliberately NOT the authorization boundary (the
        // policy below still governs everything that isn't disabled); it exists so a cached plan or
        // a resumed session can't call an op this workspace configured off, even where the policy
        // would otherwise allow it. Surface-only + defense-in-depth, never a second permission system.
        if self.disabled_ops.contains(name) {
            return Err(format!("`{name}` disabled by config ([tools] disable)"));
        }
        self.cap_scope_gate(name, audit)?;

        let spec = tool.spec();
        let subjects = tool.permission_subjects(params);
        // Filesystem grants bind to the physical target, not the caller's lexical alias. Without
        // this normalization an allow like `read(allowed/**)` could reach `secret/**` through an
        // in-workspace symlink even though guarded IO correctly kept both paths inside the workspace.
        let subjects = if spec.access.contains(&AccessKind::Filesystem) {
            let access = if spec.effects.contains(&Effect::Write) {
                PathAccess::Write
            } else {
                PathAccess::Read
            };
            let mut physical = Vec::with_capacity(subjects.len());
            for subject in subjects {
                match self.ctx.system().path_identity(&subject, access) {
                    Ok(subject) => physical.push(subject),
                    Err(err) => {
                        return Err(format!("`{name}` denied by filesystem path guard: {err}"));
                    }
                }
            }
            physical
        } else {
            subjects
        };
        let intents = tool.intents(params);

        let requirements = tool
            .authority_requirements(params, &subjects)
            .map_err(|err| format!("`{name}` denied by invalid authority contract: {err}"))?;

        // 1. Mandatory authorization-policy floor: default-deny on any ungranted requirement. A
        //    `Deny` short-circuits; an `ApprovalRequired` (e.g. a grant marked `requires_approval`,
        //    like the default `process.exec`) forces the approval gate below even if a permissive
        //    allow-rule would otherwise satisfy it — the policy is the floor, rules can't widen it.
        let mut policy_requires_approval = false;
        // Snapshot once per dispatch from the immutable lexical turn identity. The engine installs
        // it only after acquiring its single-active-turn gate.
        let identity = self.effective_identity();
        for requirement in &requirements {
            let req = PolicyRequest {
                caller: identity.caller(),
                trust: identity.trust(),
                action: &requirement.action,
                resource: &requirement.resource,
            };
            match evaluate(&self.policy, &req).decision {
                Decision::Deny => {
                    return Err(format!(
                        "`{name}` denied by policy ({} on {:?})",
                        requirement.action.0, requirement.resource.kind
                    ));
                }
                Decision::ApprovalRequired => policy_requires_approval = true,
                Decision::Allow => {}
            }
        }

        // 2. Permission rules (coder-style): deny wins; otherwise allow/ask for tool + subjects.
        let perm = self.perms.lock().unwrap().check(name, &subjects);
        if perm == PermDecision::Deny {
            return Err(format!("`{name}` denied by permission rules"));
        }

        Ok(GatedCall {
            spec,
            subjects,
            intents,
            perm,
            policy_requires_approval,
            identity,
        })
    }

    /// The capability-scope floor (step 0) — see [`gate`](Self::gate). A pure read of the scope
    /// stack; the only effect is the `cap_scope_denied` audit observation, recorded on the live path
    /// and deliberately not on an authorize-only decision.
    fn cap_scope_gate(&self, name: &str, audit: GateAudit) -> std::result::Result<(), String> {
        // Checked FIRST, before pre-tool hooks or the policy/permission layers, and on EVERY dispatch
        // (there is no other path to a tool's `execute`), so a composite op, a sub-agent's inner
        // call, or any nested reentry that eventually calls `dispatch` again is caught exactly like a
        // direct call. An empty stack (no `with_tools` scope active) is a strict no-op — every
        // existing flow that never opens a scope is unaffected. A denial here can never be a false
        // negative: the top of stack is always the *narrowed* effective set (see `push_cap_scope`),
        // so this can only ever be as strict as, or stricter than, the outer session policy.
        if let Some(scope) = self.active_cap_scope() {
            if !scope.iter().any(|t| t == name) {
                if matches!(audit, GateAudit::Live) {
                    self.ctx.evidence.lock().unwrap().record(Observation::new(
                        "cap_scope_denied",
                        Phase::Turn,
                        json!({ "tool": name, "scope": scope }),
                    ));
                }
                return Err(format!(
                    "`{name}` denied by capability scope (not in the active with_tools allowlist)"
                ));
            }
        }
        Ok(())
    }

    /// Like [`dispatch`](Self::dispatch), but also reports — structurally, not by inference —
    /// whether the envelope itself denied the call. See [`DispatchOutcome`].
    pub async fn dispatch_outcome(&self, name: &str, params: Value) -> DispatchOutcome {
        let started = Instant::now();
        let dispatch = self.dispatch_seq.fetch_add(1, Ordering::Relaxed);
        let mut approval_wait = None;
        let Some(tool) = self.registry.get(name) else {
            // D-184: an unknown tool is a structural refusal — the same bucket `authorize` already
            // puts it in (`Deny`, below), and the one `ast::Node::Retry`'s own doc comment names as
            // fatal ("policy denial, unknown op") — never a transient failure worth retrying. Before
            // this fix `denied` was `false` here, so `flux_lang::runtime::call_failure` wrapped a
            // typo'd op name in `FlowError::Runtime` and `retry`/`loop` burned attempts on a call that
            // could never succeed.
            return self.finish_dispatch(
                name,
                started,
                approval_wait,
                None,
                ToolResult::error(format!("unknown tool: {name}")),
                true,
            );
        };

        // 0. Capability-scope floor — checked before the pre-tool hooks, so a hook never observes
        //    (or rewrites the input of) a call the active `with_tools` scope already forbids.
        if let Err(reason) = self.cap_scope_gate(name, GateAudit::Live) {
            return self.finish_dispatch(
                name,
                started,
                approval_wait,
                None,
                ToolResult::error(reason),
                true,
            );
        }

        // Pre-tool hooks (system-priority first): may modify the input or deny the call.
        let mut params = params;
        for hook in &self.hooks {
            match hook.pre_tool(name, &params) {
                HookOutcome::Continue => {}
                HookOutcome::Modify(p) => params = p,
                HookOutcome::Deny(reason) => {
                    // Not an authorization refusal — hooks are meant to stay retryable/repairable
                    // (see `DispatchOutcome::denied`'s doc comment).
                    return self.finish_dispatch(
                        name,
                        started,
                        approval_wait,
                        None,
                        ToolResult::error(format!("`{name}` blocked by hook: {reason}")),
                        false,
                    );
                }
            }
        }

        // 1-2. The deterministic authorization gates — filesystem subject normalization, the
        //    authority contract, the mandatory policy floor, and the permission rules — shared
        //    verbatim with the authorize-only `Executor::authorize` (D-177) so the two can never
        //    disagree about whether, or why, a call is admissible.
        let GatedCall {
            spec,
            subjects,
            intents,
            perm,
            policy_requires_approval,
            identity,
        } = match self.gate(name, &params, tool.as_ref(), GateAudit::Live) {
            Ok(call) => call,
            Err(reason) => {
                return self.finish_dispatch(
                    name,
                    started,
                    approval_wait,
                    None,
                    ToolResult::error(reason),
                    true,
                );
            }
        };

        // 3. Evidence + reactions: record this call (and a destructive marker when matched), then
        //    let the built-in escalation reaction decide whether approval must be forced.
        let mut observations = vec![Observation::new(
            "tool_call",
            Phase::Turn,
            json!({
                "tool": name,
                "subjects": subjects,
                "caller": identity.caller().principal.id.as_str(),
                // The principal's *kind*, not just its id: `flux policy simulate` re-evaluates this
                // dispatch against a candidate policy, and subject matching discriminates on kind
                // (a `user` subject never matches an `agent` caller). Without it the replay cannot
                // tell the two apart and must report the op as indeterminate. Additive — records
                // written before this key exists simply lack it.
                "caller_kind": match identity.caller().principal.kind {
                    PolicyCallerKind::User => "user",
                    PolicyCallerKind::Agent => "agent",
                    PolicyCallerKind::System => "system",
                },
            }),
        )];
        if intents.is_destructive() {
            observations.push(Observation::new(
                KIND_DESTRUCTIVE,
                Phase::Turn,
                json!({ "tool": name, "subjects": subjects }),
            ));
        }
        let escalate = observations
            .iter()
            .any(|o| !DestructiveEscalation.react(o).is_empty());
        self.ctx.evidence.lock().unwrap().extend(observations);

        // 4. Approval gate. Destructive operations — and any effect the policy marked
        //    `requires_approval` — are forced to approval even under a permissive allow-rule;
        //    everything else asks only when the rules didn't already allow it. A write tool that
        //    reports no path subjects is also forced to prompt: its effect would otherwise resolve
        //    to an unscoped (`path:"*"`-matching) authorization rather than a specific file.
        let unscoped_write = spec.effects.contains(&Effect::Write) && subjects.is_empty();
        let force_approval = escalate
            || spec.risk == Risk::Destructive
            || policy_requires_approval
            || unscoped_write;
        //    Inside an approved-plan scope the prompt is skipped — the user approved the plan as a
        //    whole — EXCEPT for a destructive op the CURRENT (innermost) scope's approval never
        //    disclosed (the risk preview only sees literal args, so a destructive command assembled
        //    from `$symbols` at runtime is invisible to it). Such an undisclosed destructive op
        //    re-fires the gate: the interactive approver prompts, `--yes` allows, the sub-agent
        //    approver denies. This deliberately also holds under `trust_all` ("always"). Hard denies
        //    (steps 1-2 above) always apply. C-27: keyed on the innermost scope's own disclosure flag
        //    (top of `destructive_scope`), not a shared depth counter — a nested plan approved
        //    `destructive:false` must re-fire even when an outer scope disclosed.
        let undisclosed_destructive = intents.is_destructive()
            && !self
                .destructive_scope
                .lock()
                .unwrap()
                .last()
                .copied()
                .unwrap_or(false);
        let approval_sensitive = force_approval || perm != PermDecision::Allow;
        if (!self.in_approved_scope() || undisclosed_destructive) && approval_sensitive {
            let approver = self.approver.lock().unwrap().clone();
            self.record_dispatch_event("approval.requested", dispatch, name, started, json!({}));
            let approval_started = Instant::now();
            let choice = approver.request(name, &subjects, &intents).await;
            approval_wait = Some(approval_started.elapsed());
            match choice {
                ApprovalChoice::Allow => self.record_dispatch_event(
                    "approval.approved",
                    dispatch,
                    name,
                    started,
                    json!({ "choice": "allow" }),
                ),
                ApprovalChoice::AllowAlways(rule) => {
                    self.record_dispatch_event(
                        "approval.approved",
                        dispatch,
                        name,
                        started,
                        json!({ "choice": "always" }),
                    );
                    self.perms.lock().unwrap().add_allow(&rule);
                }
                ApprovalChoice::Deny | ApprovalChoice::DenyWithReason(_) => {
                    // C-113: a reason-carrying denial keeps the canonical op-anchored shape and
                    // appends the user's why, giving the loop something to adapt to instead of
                    // re-proposing a near-identical call.
                    let reason = match choice {
                        ApprovalChoice::DenyWithReason(reason) => Some(reason),
                        _ => None,
                    };
                    self.record_dispatch_event(
                        "approval.denied",
                        dispatch,
                        name,
                        started,
                        match &reason {
                            Some(reason) => json!({ "reason": reason }),
                            None => json!({}),
                        },
                    );
                    let text = match reason {
                        Some(reason) => format!("`{name}` denied by user — reason: {reason}"),
                        None => format!("`{name}` denied by user"),
                    };
                    return self.finish_dispatch(
                        name,
                        started,
                        approval_wait,
                        None,
                        ToolResult::error(text),
                        true,
                    );
                }
            }
        }

        // 4½. Content-addressed op cache (L-54) — probed only AFTER every gate above passed, so a
        //    hit is served strictly to a caller for whom the op is admissible RIGHT NOW. Cacheable =
        //    deterministic (`Idempotent`) + read-only (every effect `Read`) + low-risk +
        //    approval-insensitive + non-destructive; model calls, writes, unknown ops (no spec ⇒
        //    returned above), and anything approval-shaped never enter the cache.
        let cacheable = self.cache_enabled
            && spec.effects.iter().all(|e| matches!(e, Effect::Read))
            && spec.idempotency == Idempotency::Idempotent
            && spec.risk == Risk::Low
            && !approval_sensitive
            && !intents.is_destructive();
        let cache_key = cacheable.then(|| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            // Op identity + normalized input (serde_json objects are key-sorted, so `to_string`
            // is canonical) + schema fingerprint (the op's version-bearing surface) + the
            // invalidation-domain generation.
            name.hash(&mut h);
            params.to_string().hash(&mut h);
            spec.input_schema.to_string().hash(&mut h);
            self.cache_gen.load(Ordering::SeqCst).hash(&mut h);
            h.finish()
        });
        if let Some(key) = cache_key {
            // Bind the hit FIRST so the op_cache guard drops before the evidence lock below —
            // holding both pinned a lock order and serialized hits (review, 2026-07-09).
            let hit = self.op_cache.lock().unwrap().get(&key).cloned();
            if let Some(mut hit) = hit {
                // Re-redact against the CURRENT secret set: a secret registered after this
                // result was stored must not replay in cleartext (review, 2026-07-09).
                hit.content = self.ctx.redactor.redact(&hit.content);
                hit.view = hit.view.map(|v| self.ctx.redactor.redact(&v));
                // Audit-distinguishable from a fresh execution: the `tool_call` observation above
                // fired as usual, and this marker says the result was replayed, not re-fetched.
                self.ctx.evidence.lock().unwrap().record(Observation::new(
                    "op_cache_hit",
                    Phase::Turn,
                    json!({ "tool": name }),
                ));
                self.record_dispatch_event("tool.cache_hit", dispatch, name, started, json!({}));
                return self.finish_dispatch(name, started, approval_wait, None, hit, false);
            }
        }

        // 4¾. A mutating dispatch starts a new invalidation generation BEFORE its IO runs (and
        //    clears again after, step 7): pre-bumping closes the window where a concurrent read
        //    could be served a pre-write value after the write's IO already landed (review,
        //    2026-07-09). A failed write invalidates too — conservative and sound.
        let mutating = spec.effects.iter().any(|e| !matches!(e, Effect::Read));
        if mutating {
            self.cache_gen.fetch_add(1, Ordering::SeqCst);
            self.op_cache.lock().unwrap().clear();
        }

        // 5. System boundary: the only place real IO happens. Redact secrets from the result —
        //    both the success content and any error — before it reaches the model or the logs.
        self.record_dispatch_event("tool.started", dispatch, name, started, json!({}));
        let execution_started = Instant::now();
        // Keep the complete runtime-turn context live only for this guarded tool future. A nested
        // runtime assembled by an adapter tool can inherit cancellation, session lineage and the
        // reporter together; task-local scoping isolates concurrent turns and restores an outer
        // scope after a nested dispatch.
        let execution = tool.execute(&self.ctx, params);
        let executed = scope_runtime_turn(self.ctx.runtime_turn_context(), execution).await;
        let result = match executed {
            Ok(mut r) => {
                // Redact BOTH faces: the view can carry file content / diffs that include secrets.
                r.content = self.ctx.redactor.redact(&r.content);
                r.view = r.view.map(|v| self.ctx.redactor.redact(&v));
                r
            }
            Err(e) => ToolResult::error(self.ctx.redactor.redact(&e.to_string())),
        };
        let execution = Some(execution_started.elapsed());
        self.record_dispatch_event(
            "tool.ended",
            dispatch,
            name,
            started,
            json!({
                "status": if result.is_error { "error" } else { "ok" },
                "execution_us": execution.map(|d| d.as_micros().min(u64::MAX as u128) as u64),
            }),
        );
        // 6. Record a `tool_error` observation on a failed call (an op that ran and errored), so
        //    `metrics()`/`evidence` give a model-in-the-loop the failure signal to retry/stop on. The
        //    matching `tool_call` was already recorded above, so the shared log carries both.
        if result.is_error {
            self.ctx.evidence.lock().unwrap().record(Observation::new(
                "tool_error",
                Phase::Turn,
                json!({ "tool": name }),
            ));
        }
        // 7. Cache maintenance (L-54). A mutating dispatch invalidated BEFORE its IO (step 4¾);
        //    clear once more now that the IO landed so anything cached concurrently during the
        //    write is dropped too. A cacheable success is stored (already redacted) for replay
        //    within this generation.
        if mutating {
            self.cache_gen.fetch_add(1, Ordering::SeqCst);
            self.op_cache.lock().unwrap().clear();
        } else if let Some(key) = cache_key {
            if !result.is_error {
                let mut cache = self.op_cache.lock().unwrap();
                // Crude but safe size bound: a full reset never affects correctness, only reuse.
                if cache.len() >= 512 {
                    cache.clear();
                }
                cache.insert(key, result.clone());
            }
        }
        // The op ran (successfully or not) — never a `denied` outcome, no matter what its own
        // content says (L-32).
        self.finish_dispatch(name, started, approval_wait, execution, result, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_policy::{CallerKind, Principal, TrustKind, TrustLevel};
    use flux_system::Workspace;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Default)]
    struct RecordingProgressSink(Mutex<Vec<ToolProgress>>);

    impl ToolProgressSink for RecordingProgressSink {
        fn emit(&self, progress: ToolProgress) {
            self.0.lock().unwrap().push(progress);
        }
    }

    /// C-158 acceptance: partial output flows through the SAME redaction the final result gets.
    /// The reporter binds the context's redactor, so a tool cannot report a raw secret even by
    /// mistake — there is no un-redacted path to the sink at all.
    #[tokio::test]
    async fn reported_tool_progress_is_redacted_like_a_result() {
        let (dir, system) = temp_workspace("progress-redact");
        let ctx = ToolContext::new(system);
        ctx.redactor.add_secret("hunter2-super-secret");
        let sink = Arc::new(RecordingProgressSink::default());
        let installed: Arc<dyn ToolProgressSink> = sink.clone();

        scope_runtime_turn(
            RuntimeTurnContext::new().with_tool_progress_sink(installed),
            async {
                let reporter = ctx
                    .progress_reporter("bash")
                    .expect("a sink is installed for this turn");
                reporter.report("connecting with hunter2-super-secret now");
                // A credential-SHAPED token the redactor has never been told about is caught too,
                // by the same pattern matcher that scrubs results.
                reporter.report("token sk-ant-abcdefghijklmnop");
            },
        )
        .await;

        let seen = sink.0.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert!(
            !seen[0].line.contains("hunter2-super-secret"),
            "registered secret survived onto the progress channel: {:?}",
            seen[0].line
        );
        assert!(
            !seen[1].line.contains("sk-ant-abcdefghijklmnop"),
            "credential-shaped token survived onto the progress channel: {:?}",
            seen[1].line
        );
        assert_eq!(seen[0].tool, "bash");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// With no surface installed there is no reporter, so a tool runs exactly as it did before —
    /// the capability is opt-in and absent by default.
    #[tokio::test]
    async fn no_installed_sink_means_no_reporter() {
        let (dir, system) = temp_workspace("progress-absent");
        let ctx = ToolContext::new(system);
        assert!(ctx.progress_reporter("bash").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[derive(Default)]
    struct RecordingSurfaceSink(Mutex<Vec<PaneCommand>>);

    impl SurfaceSink for RecordingSurfaceSink {
        fn emit(&self, command: PaneCommand) {
            self.0.lock().unwrap().push(command);
        }
    }

    /// C-220 acceptance: a pane crosses to the surface through the SAME redaction a result gets.
    /// The reporter binds the context's redactor, so a tool cannot title a pane with a secret or
    /// fill one with secret-bearing rows — there is no unredacted path to the sink at all.
    #[tokio::test]
    async fn pane_content_is_redacted_before_it_reaches_a_surface() {
        let (dir, system) = temp_workspace("surface-redact");
        let ctx = ToolContext::new(system);
        ctx.redactor.add_secret("hunter2-super-secret");
        let sink = Arc::new(RecordingSurfaceSink::default());
        let installed: Arc<dyn SurfaceSink> = sink.clone();

        scope_runtime_turn(
            RuntimeTurnContext::new().with_surface_sink(installed),
            async {
                let surface = ctx.surface().expect("a sink is installed for this turn");
                surface
                    .send(PaneCommand::Open(PaneSpec::new(
                        "creds",
                        "deploy hunter2-super-secret",
                        PaneSlot::Right,
                        PaneLifetime::Session,
                        PaneData::Log {
                            lines: vec![
                                "connecting with hunter2-super-secret".to_string(),
                                // A credential-SHAPED token the redactor has never been told
                                // about is caught too, by the same pattern matcher.
                                "token sk-ant-abcdefghijklmnop".to_string(),
                            ],
                        },
                    )))
                    .expect("a session-lifetime pane is accepted");
            },
        )
        .await;

        let seen = sink.0.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        let PaneCommand::Open(spec) = &seen[0] else {
            panic!("expected an open command, got {:?}", seen[0]);
        };
        assert!(
            !spec.title.contains("hunter2-super-secret"),
            "registered secret survived onto a pane title: {:?}",
            spec.title
        );
        let PaneData::Log { lines } = &spec.data else {
            panic!("expected log data, got {:?}", spec.data);
        };
        assert!(
            !lines[0].contains("hunter2-super-secret"),
            "registered secret survived onto pane data: {:?}",
            lines[0]
        );
        assert!(
            !lines[1].contains("sk-ant-abcdefghijklmnop"),
            "credential-shaped token survived onto pane data: {:?}",
            lines[1]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// With no surface installed there is no reporter, so the pane channel is opt-in and absent by
    /// default — a headless run cannot accidentally acquire one.
    #[tokio::test]
    async fn no_installed_surface_sink_means_no_reporter() {
        let (dir, system) = temp_workspace("surface-absent");
        let ctx = ToolContext::new(system);
        assert!(ctx.surface().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-220 acceptance: `lifetime: project` PARSES — the wire vocabulary is stable — but the
    /// reporter refuses it, because cross-session panes have no implementation yet. The caller
    /// learns that; it does not get a pane that silently fails to come back.
    #[tokio::test]
    async fn project_lifetime_parses_but_the_reporter_rejects_it() {
        let (dir, system) = temp_workspace("surface-project");
        let ctx = ToolContext::new(system);
        let sink = Arc::new(RecordingSurfaceSink::default());
        let installed: Arc<dyn SurfaceSink> = sink.clone();

        let parsed: PaneLifetime = serde_json::from_value(json!("project")).unwrap();
        assert_eq!(parsed, PaneLifetime::Project);

        scope_runtime_turn(
            RuntimeTurnContext::new().with_surface_sink(installed),
            async {
                let surface = ctx.surface().expect("a sink is installed for this turn");
                let err = surface
                    .send(PaneCommand::Open(PaneSpec::new(
                        "notes",
                        "Notes",
                        PaneSlot::Left,
                        PaneLifetime::Project,
                        PaneData::Markdown {
                            text: "hello".to_string(),
                        },
                    )))
                    .expect_err("project lifetime has no implementation yet");
                let message = err.to_string();
                assert!(
                    message.contains("project") && message.contains("not supported yet"),
                    "rejection should say plainly what is unsupported: {message}"
                );
            },
        )
        .await;

        assert!(
            sink.0.lock().unwrap().is_empty(),
            "a rejected pane must never reach the surface"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Field names anywhere in a serialized pane that would let a model reach a `Style`. A model
    /// that can paint a region inside a trusted terminal can imitate the approval sheet, so the
    /// absence of these is the trust property C-222 rests on.
    ///
    /// This is a **denylist of whole key names**, and it is the weaker half of the pin — see
    /// [`pane_spec_carries_no_style_bearing_field`] for which half actually guarantees what.
    const STYLE_BEARING: &[&str] = &[
        "color",
        "colour",
        "style",
        "fg",
        "bg",
        "background",
        "foreground",
        "width",
        "height",
        "rect",
        "area",
        "x",
        "y",
        "z",
        "z_order",
        "zorder",
        "layer",
        "theme",
        "bold",
        "italic",
        "underline",
        "modifier",
        "border",
        "margin",
        "padding",
        "align",
        "font",
    ];

    /// Every key in `value`, recursively.
    fn collect_keys(value: &Value, into: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    into.push(key.clone());
                    collect_keys(child, into);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect_keys(item, into)),
            _ => {}
        }
    }

    /// C-220 acceptance: the pane vocabulary is content and structure only, so C-222's trust
    /// property cannot be relaxed by accident in a later story.
    ///
    /// **The two halves of this test are not equally strong, and C-222 should not over-trust the
    /// second one.**
    ///
    /// 1. The top-level assertion on `PaneSpec` is **exact-set**: the wire form must have precisely
    ///    the six documented keys. This half is a real guarantee — any added field fails it, whatever
    ///    it is called.
    /// 2. The recursive walk over `PaneData` is a **denylist** of whole key names ([`STYLE_BEARING`]).
    ///    It catches accidental widening, not determined widening: a style-bearing payload field
    ///    named `tint`, `emphasis`, `indent`, `order` or `column_width` would pass, as would any
    ///    `#[serde(skip)]` field, since this inspects the serialized form only.
    ///
    /// An exact-set assertion over the nested variants was considered and rejected: it fights
    /// serde's tagging shapes for little gain, given that the payload types are defined in this
    /// file and reviewed with it. If a future story lets a pane payload carry externally-authored
    /// fields, that trade changes and this half needs to become exact too.
    #[test]
    fn pane_spec_carries_no_style_bearing_field() {
        let every_kind = vec![
            PaneData::Rows {
                header: vec!["file".into()],
                rows: vec![vec!["a.rs".into()]],
            },
            PaneData::Kv {
                pairs: vec![("branch".into(), "main".into())],
            },
            PaneData::Log {
                lines: vec!["building".into()],
            },
            PaneData::Progress {
                label: "tests".into(),
                done: 3,
                total: 9,
            },
            PaneData::Tree {
                roots: vec![PaneNode {
                    label: "root".into(),
                    children: vec![PaneNode {
                        label: "child".into(),
                        children: vec![],
                    }],
                }],
            },
            PaneData::Markdown {
                text: "# hi".into(),
            },
        ];

        for data in every_kind {
            let spec = PaneSpec::new("p", "Title", PaneSlot::Right, PaneLifetime::Turn, data);
            let wire = serde_json::to_value(&spec).unwrap();

            // The vocabulary is exactly the six documented fields — no more.
            let mut top: Vec<&str> = wire
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            top.sort_unstable();
            assert_eq!(
                top,
                ["data", "id", "kind", "lifetime", "slot", "title"],
                "PaneSpec grew or lost a field: {wire}"
            );

            let mut keys = Vec::new();
            collect_keys(&wire, &mut keys);
            for key in keys {
                let normalized = key.to_ascii_lowercase();
                assert!(
                    !STYLE_BEARING.contains(&normalized.as_str()),
                    "pane payload gained a style-bearing field '{key}': a model could then paint a \
                     region that imitates the approval sheet (see C-222): {wire}"
                );
            }
        }
    }

    /// The three closed vocabularies are exactly what the design names, and an unknown value is a
    /// parse failure rather than a silently-accepted default.
    #[test]
    fn pane_vocabularies_are_closed_sets() {
        for (wire, slot) in [
            ("left", PaneSlot::Left),
            ("right", PaneSlot::Right),
            ("bottom", PaneSlot::Bottom),
            ("overlay", PaneSlot::Overlay),
        ] {
            assert_eq!(
                serde_json::from_value::<PaneSlot>(json!(wire)).unwrap(),
                slot
            );
        }
        for (wire, kind) in [
            ("rows", PaneKind::Rows),
            ("kv", PaneKind::Kv),
            ("log", PaneKind::Log),
            ("progress", PaneKind::Progress),
            ("tree", PaneKind::Tree),
            ("markdown", PaneKind::Markdown),
        ] {
            assert_eq!(
                serde_json::from_value::<PaneKind>(json!(wire)).unwrap(),
                kind
            );
        }
        for (wire, lifetime) in [
            ("turn", PaneLifetime::Turn),
            ("session", PaneLifetime::Session),
            ("project", PaneLifetime::Project),
        ] {
            assert_eq!(
                serde_json::from_value::<PaneLifetime>(json!(wire)).unwrap(),
                lifetime
            );
        }
        assert!(serde_json::from_value::<PaneSlot>(json!("floating")).is_err());
        assert!(serde_json::from_value::<PaneKind>(json!("canvas")).is_err());
        assert!(serde_json::from_value::<PaneLifetime>(json!("forever")).is_err());
    }

    /// A deserialized spec carries `kind` and `data` independently, so the two can disagree. The
    /// reporter refuses rather than leaving the surface to pick which one to believe.
    #[tokio::test]
    async fn a_spec_whose_kind_contradicts_its_data_is_rejected() {
        let (dir, system) = temp_workspace("surface-mismatch");
        let ctx = ToolContext::new(system);
        let sink = Arc::new(RecordingSurfaceSink::default());
        let installed: Arc<dyn SurfaceSink> = sink.clone();

        let spec: PaneSpec = serde_json::from_value(json!({
            "id": "p",
            "title": "Title",
            "slot": "right",
            "kind": "rows",
            "lifetime": "turn",
            "data": { "markdown": { "text": "# hi" } },
        }))
        .expect("both fields are on the wire, so this parses");

        scope_runtime_turn(
            RuntimeTurnContext::new().with_surface_sink(installed),
            async {
                let err = ctx
                    .surface()
                    .expect("a sink is installed for this turn")
                    .send(PaneCommand::Open(spec))
                    .expect_err("a contradictory spec is refused");
                assert!(err.to_string().contains("does not match its data"));
            },
        )
        .await;

        assert!(sink.0.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `update` and `close` reach the surface through the same redaction `open` does — there is one
    /// gate, not one per command shape.
    #[tokio::test]
    async fn update_and_close_are_redacted_on_the_same_path() {
        let (dir, system) = temp_workspace("surface-update");
        let ctx = ToolContext::new(system);
        ctx.redactor.add_secret("hunter2-super-secret");
        let sink = Arc::new(RecordingSurfaceSink::default());
        let installed: Arc<dyn SurfaceSink> = sink.clone();

        scope_runtime_turn(
            RuntimeTurnContext::new().with_surface_sink(installed),
            async {
                let surface = ctx.surface().expect("a sink is installed for this turn");
                surface
                    .send(PaneCommand::Update {
                        id: "creds".to_string(),
                        data: PaneData::Kv {
                            pairs: vec![("token".into(), "hunter2-super-secret".into())],
                        },
                    })
                    .unwrap();
                surface
                    .send(PaneCommand::Close {
                        id: "creds".to_string(),
                    })
                    .unwrap();
            },
        )
        .await;

        let seen = sink.0.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        let PaneCommand::Update { data, .. } = &seen[0] else {
            panic!("expected an update, got {:?}", seen[0]);
        };
        let PaneData::Kv { pairs } = data else {
            panic!("expected kv data, got {data:?}");
        };
        assert!(
            !pairs[0].1.contains("hunter2-super-secret"),
            "registered secret survived a pane update: {:?}",
            pairs[0].1
        );
        // An id with no secret in it is unchanged, so open/update/close still address one pane.
        assert_eq!(
            seen[1],
            PaneCommand::Close {
                id: "creds".to_string()
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn temp_workspace(tag: &str) -> (std::path::PathBuf, Arc<System>) {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("flux-rt-wsctx-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        (dir, system)
    }

    /// C-97: a worktree transition is context-local. Two contexts share one initial system;
    /// transitioning one changes what its clones observe, while the sibling context and the
    /// process-wide cwd stay untouched.
    #[test]
    fn worktree_transition_is_context_local() {
        let (origin, system) = temp_workspace("origin");
        let (target, _) = temp_workspace("target");
        let ctx_a = ToolContext::new(system.clone());
        let ctx_b = ToolContext::new(system.clone());
        let ctx_a_clone = ctx_a.clone();
        let cwd_before = std::env::current_dir().unwrap();

        let rerooted = Arc::new(system.rerooted(&target).unwrap());
        ctx_a
            .workspace_context()
            .enter_worktree(
                WorktreeSession {
                    original: system.clone(),
                    base_commit: "deadbeef".into(),
                    branch: "flux/worktree/test".into(),
                    checkout: target.clone(),
                    parent_dir: target.clone(),
                    phase: WorktreePhase::Active,
                },
                rerooted,
            )
            .unwrap();

        let canon = |p: &std::path::Path| p.canonicalize().unwrap();
        // The transitioned context and its pre-existing clone both observe the new root…
        assert_eq!(ctx_a.system().workspace().root(), canon(&target));
        assert_eq!(ctx_a_clone.system().workspace().root(), canon(&target));
        // …the sibling context does not…
        assert_eq!(ctx_b.system().workspace().root(), canon(&origin));
        // …and the process-wide cwd never moves.
        assert_eq!(std::env::current_dir().unwrap(), cwd_before);

        // Nesting is rejected as a recoverable error.
        let again = ctx_a.workspace_context().enter_worktree(
            WorktreeSession {
                original: system.clone(),
                base_commit: "deadbeef".into(),
                branch: "flux/worktree/test2".into(),
                checkout: target.clone(),
                parent_dir: target.clone(),
                phase: WorktreePhase::Active,
            },
            Arc::new(system.rerooted(&target).unwrap()),
        );
        assert!(again.is_err());

        // Leaving restores the original system for the whole context.
        ctx_a.workspace_context().leave_worktree().unwrap();
        assert_eq!(ctx_a.system().workspace().root(), canon(&origin));
        assert_eq!(ctx_a_clone.system().workspace().root(), canon(&origin));
        assert!(ctx_a.workspace_context().worktree_session().is_none());
        assert!(ctx_a.workspace_context().leave_worktree().is_err());
    }

    /// C-122: an environment given a pre-created workspace handle builds its executor context
    /// OVER that handle — a transition driven through the external handle (what a surface's
    /// plugin-caps adapter shares) is observed by the executor's tools, and vice versa. The
    /// pre-C-122 behaviour minted a fresh handle in `into_executor`, so the surface's copy and
    /// the executor's copy could never see each other's transitions.
    #[test]
    fn with_workspace_shares_one_handle_between_surface_and_executor() {
        let (origin, system) = temp_workspace("c122-env");
        let (target, _) = temp_workspace("c122-env-target");
        let workspace = WorkspaceContext::new(system.clone());

        let executor = ExecutionEnvironment::new(
            system.clone(),
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            ExecutionAuthorization::local(),
        )
        .with_workspace(workspace.clone())
        .into_executor();

        let canon = |p: &std::path::Path| p.canonicalize().unwrap();
        assert_eq!(
            executor.context().system().workspace().root(),
            canon(&origin)
        );

        // The surface-held handle transitions (what git_worktree_enter does through the tool
        // context — here driven externally, as the plugin-caps adapter would observe it).
        let rerooted = Arc::new(system.rerooted(&target).unwrap());
        workspace
            .enter_worktree(
                WorktreeSession {
                    original: system.clone(),
                    base_commit: "deadbeef".into(),
                    branch: "flux/worktree/c122".into(),
                    checkout: target.clone(),
                    parent_dir: target.clone(),
                    phase: WorktreePhase::Active,
                },
                rerooted,
            )
            .unwrap();
        assert_eq!(
            executor.context().system().workspace().root(),
            canon(&target),
            "the executor context must observe the surface handle's transition"
        );

        // And the reverse direction: the executor context's handle IS the surface handle.
        executor
            .context()
            .workspace_context()
            .leave_worktree()
            .unwrap();
        assert_eq!(workspace.active().workspace().root(), canon(&origin));
    }

    /// C-97: a retried leave after a partial cleanup must know the merge already landed.
    #[test]
    fn worktree_session_phase_marks_merged() {
        let (_origin, system) = temp_workspace("phase");
        let (target, _) = temp_workspace("phase-target");
        let ctx = ToolContext::new(system.clone());
        ctx.workspace_context()
            .enter_worktree(
                WorktreeSession {
                    original: system.clone(),
                    base_commit: "deadbeef".into(),
                    branch: "flux/worktree/phase".into(),
                    checkout: target.clone(),
                    parent_dir: target,
                    phase: WorktreePhase::Active,
                },
                system.clone(),
            )
            .unwrap();
        ctx.workspace_context().mark_merged();
        assert_eq!(
            ctx.workspace_context().worktree_session().unwrap().phase,
            WorktreePhase::Merged
        );
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn spawn_supervisor_aborts_and_reaps_only_after_its_grace() {
        let supervisor = Arc::new(SpawnTaskSupervisor::default());
        let dropped = Arc::new(AtomicBool::new(false));
        let task_dropped = dropped.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let handle = supervisor.spawn(async move {
            let _drop = DropFlag(task_dropped);
            let _ = entered_tx.send(());
            std::future::pending::<()>().await;
        });
        entered_rx.await.unwrap();
        drop(handle);

        assert!(!dropped.load(Ordering::SeqCst));
        let graceful = supervisor
            .shutdown(std::time::Duration::from_millis(20))
            .await;

        assert!(!graceful, "the cancellation-insensitive task needed abort");
        assert!(dropped.load(Ordering::SeqCst), "abort must reap task state");
        assert!(supervisor.is_idle());
    }

    struct DelayedAllowApprover;

    #[async_trait]
    impl Approver for DelayedAllowApprover {
        async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            ApprovalChoice::Allow
        }
    }

    #[tokio::test]
    async fn dispatch_attributes_approval_wait_separately_from_execution() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        let executor = Executor::new(
            registry,
            PermissionManager::new(),
            Arc::new(DelayedAllowApprover),
            test_ctx(),
        );
        let outcome = executor
            .dispatch_outcome("echo", json!({"text": "hi"}))
            .await;
        assert!(!outcome.result.is_error);
        assert!(
            outcome.timing.approval_wait_us.unwrap_or_default() >= 20_000,
            "approval delay was not attributed: {:?}",
            outcome.timing
        );
        assert!(
            outcome.timing.execution_us.unwrap_or(u64::MAX) < 20_000,
            "instant tool was mislabeled as slow: {:?}",
            outcome.timing
        );
        let evidence = executor.evidence();
        let lifecycle: Vec<&Observation> = evidence
            .all()
            .iter()
            .filter(|o| {
                matches!(
                    o.kind.as_str(),
                    "approval.requested"
                        | "approval.approved"
                        | "approval.denied"
                        | "tool.started"
                        | "tool.ended"
                )
            })
            .collect();
        let kinds: Vec<&str> = lifecycle.iter().map(|o| o.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "approval.requested",
                "approval.approved",
                "tool.started",
                "tool.ended"
            ]
        );
        assert!(
            lifecycle
                .windows(2)
                .all(|pair| pair[0].data["elapsed_us"].as_u64()
                    <= pair[1].data["elapsed_us"].as_u64()),
            "lifecycle elapsed times must be monotonic: {lifecycle:?}"
        );
        assert!(lifecycle
            .iter()
            .all(|o| o.data["dispatch"] == lifecycle[0].data["dispatch"]));
    }

    fn test_ctx() -> ToolContext {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-rt-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    /// A tool that echoes a `text` param, with the value as its permission subject.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("echo", "echo text", json!({"type": "object"}))
        }
        fn permission_subjects(&self, params: &Value) -> Vec<String> {
            params
                .get("text")
                .and_then(|v| v.as_str())
                .map(|s| vec![s.to_string()])
                .unwrap_or_default()
        }
        async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                params["text"].as_str().unwrap_or("").to_string(),
            ))
        }
    }

    struct NoopSpawnActivitySink;

    impl SpawnActivitySink for NoopSpawnActivitySink {
        fn emit(&self, _activity: SpawnActivity) {}
    }

    /// A nested runtime (for example a one-shot `FlowClient` opened by an adapter tool) constructs
    /// its own context inside the outer tool future. It must inherit the active turn's child reporter
    /// or a sub-agent spawned by that nested runtime becomes invisible to the parent channel.
    struct NestedContextProbe {
        captured: Arc<Mutex<Option<ToolContext>>>,
    }

    #[async_trait]
    impl Tool for NestedContextProbe {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "nested_context_probe",
                "probe nested context inheritance",
                json!({"type": "object"}),
            )
        }

        async fn execute(&self, ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
            let nested = ToolContext::new(ctx.system());
            let inherited = nested.spawn_activity_sink().is_some()
                && nested.cancel_token().is_some()
                && nested.session_id().as_deref() == Some("parent-session");
            *self.captured.lock().unwrap() = Some(nested);
            Ok(ToolResult::ok(if inherited {
                "inherited"
            } else {
                "missing"
            }))
        }
    }

    #[tokio::test]
    async fn nested_tool_context_inherits_the_active_spawn_reporter() {
        let captured = Arc::new(Mutex::new(None));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(NestedContextProbe {
            captured: captured.clone(),
        }));
        let ctx = test_ctx();
        ctx.set_cancel(tokio_util::sync::CancellationToken::new());
        ctx.set_session("parent-session");
        ctx.set_spawn_activity_sink(Arc::new(NoopSpawnActivitySink));
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["nested_context_probe".to_string()], &[]),
            Arc::new(AllowApprover),
            ctx,
        );

        let result = executor.dispatch("nested_context_probe", json!({})).await;

        assert_eq!(result.content, "inherited");
        let retained = captured.lock().unwrap().as_ref().unwrap().clone();
        assert!(retained.runtime_turn_context().is_empty());
    }

    #[tokio::test]
    async fn parallel_runtime_turn_scopes_do_not_exchange_cancel_or_session() {
        let ctx = test_ctx();
        let left_cancel = tokio_util::sync::CancellationToken::new();
        let right_cancel = tokio_util::sync::CancellationToken::new();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let left = scope_runtime_turn(
            RuntimeTurnContext::new()
                .with_cancel(left_cancel.clone())
                .with_session("left"),
            {
                let ctx = ctx.clone();
                let barrier = barrier.clone();
                async move {
                    barrier.wait().await;
                    left_cancel.cancel();
                    tokio::task::yield_now().await;
                    assert!(ctx.cancel_token().unwrap().is_cancelled());
                    assert_eq!(ctx.session_id().as_deref(), Some("left"));
                }
            },
        );
        let right = scope_runtime_turn(
            RuntimeTurnContext::new()
                .with_cancel(right_cancel)
                .with_session("right"),
            {
                let ctx = ctx.clone();
                async move {
                    barrier.wait().await;
                    tokio::task::yield_now().await;
                    assert!(!ctx.cancel_token().unwrap().is_cancelled());
                    assert_eq!(ctx.session_id().as_deref(), Some("right"));
                }
            },
        );

        tokio::join!(left, right);
        assert!(ctx.runtime_turn_context().is_empty());
    }

    #[tokio::test]
    async fn empty_runtime_turn_scope_suppresses_and_restores_stored_fallbacks() {
        let ctx = test_ctx();
        ctx.set_cancel(tokio_util::sync::CancellationToken::new());
        ctx.set_session("obsolete");
        ctx.set_spawn_activity_sink(Arc::new(NoopSpawnActivitySink));

        scope_runtime_turn(RuntimeTurnContext::new(), async {
            assert!(ctx.runtime_turn_context().is_empty());
            let retained = ToolContext::new(ctx.system());
            assert!(retained.runtime_turn_context().is_empty());
        })
        .await;

        assert_eq!(ctx.session_id().as_deref(), Some("obsolete"));
        assert!(ctx.cancel_token().is_some());
        assert!(ctx.spawn_activity_sink().is_some());
    }

    #[tokio::test]
    async fn nested_reporter_scope_on_a_cloned_snapshot_restores_the_outer_reporter() {
        let outer: Arc<dyn SpawnActivitySink> = Arc::new(NoopSpawnActivitySink);
        let inner: Arc<dyn SpawnActivitySink> = Arc::new(NoopSpawnActivitySink);
        let ctx = test_ctx();
        let cloned = ctx.clone();

        scope_runtime_turn(
            RuntimeTurnContext::new().with_spawn_activity_sink(outer.clone()),
            async {
                assert!(Arc::ptr_eq(&cloned.spawn_activity_sink().unwrap(), &outer));
                let nested = cloned
                    .runtime_turn_context()
                    .with_spawn_activity_sink(inner.clone());
                scope_runtime_turn(nested, async {
                    assert!(Arc::ptr_eq(&cloned.spawn_activity_sink().unwrap(), &inner));
                })
                .await;
                assert!(Arc::ptr_eq(&cloned.spawn_activity_sink().unwrap(), &outer));
            },
        )
        .await;
        assert!(cloned.spawn_activity_sink().is_none());
    }

    /// Minimal guarded filesystem reader used to prove that permission subjects name the physical
    /// target, not a symlink alias supplied by the caller.
    struct FileReadTool;

    #[async_trait]
    impl Tool for FileReadTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("file_read", "read a file", json!({"type": "object"}))
                .with_access(vec![flux_spec::AccessKind::Filesystem])
        }

        fn permission_subjects(&self, params: &Value) -> Vec<String> {
            params
                .get("path")
                .and_then(Value::as_str)
                .map(|path| vec![path.to_string()])
                .unwrap_or_default()
        }

        async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
            let path = params
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(ToolResult::ok(ctx.system().read_file(path).await?))
        }
    }

    /// Records whether it was asked, and returns a fixed choice.
    struct RecordingApprover {
        asked: AtomicBool,
        choice: fn() -> ApprovalChoice,
    }
    #[async_trait]
    impl Approver for RecordingApprover {
        async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
            self.asked.store(true, Ordering::Relaxed);
            (self.choice)()
        }
    }

    /// Builds a plan-approval request the way the flow layer does from its risk preview.
    fn plan_request(summary: &str, ops: usize) -> PlanApprovalRequest {
        PlanApprovalRequest {
            summary: summary.into(),
            ops: (0..ops).map(|i| format!("op{i}")).collect(),
            ..Default::default()
        }
    }

    /// A tool with a destructive-shaped process intent (the per-op gate's force-approval trigger),
    /// used to prove the disclosed/undisclosed destructive-scope semantics.
    struct RmTool;
    #[async_trait]
    impl Tool for RmTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("rm", "rm", json!({"type": "object"}))
                .with_effects(vec![Effect::Process])
                .with_access(vec![AccessKind::Process])
        }
        fn intents(&self, _p: &Value) -> IntentSet {
            let mut s = IntentSet::new();
            s.push(flux_spec::Intent {
                behavior: flux_spec::IntentBehavior::CommandExecution,
                target: flux_spec::IntentTarget::Process {
                    command: "rm -rf scratch".into(),
                },
                role: flux_spec::IntentRole::ProcessCommand,
                certainty: flux_spec::IntentCertainty::Certain,
            });
            s
        }
        async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("removed"))
        }
    }

    /// A second read-only tool, distinct from `echo`, used to prove capability-scope narrowing (one
    /// tool allowed inside the scope, the other denied). Dogfoods [`crate::tool_fn`] (D-59): a plain
    /// closure tool needs no bespoke `impl Tool` struct.
    fn ping_tool() -> Arc<dyn Tool> {
        crate::tool_fn(
            ToolSpec::read_only("ping", "ping", json!({"type": "object"})),
            |_params: Value| async move { Ok(Value::String("pong".to_string())) },
        )
    }

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(EchoTool));
        r
    }

    // ---- L-54: content-addressed op cache -------------------------------------------------

    /// A deterministic read-only tool that counts real executions — the cache-observability probe.
    fn counting_read_tool(counter: Arc<std::sync::atomic::AtomicUsize>) -> Arc<dyn Tool> {
        crate::tool_fn(
            ToolSpec::read_only("cread", "counting read", json!({"type": "object"})),
            move |params: Value| {
                let counter = counter.clone();
                async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok(Value::String(format!("result-{params}-{n}")))
                }
            },
        )
    }

    /// A cache-test executor: the cache-probe tools allowed without prompting.
    fn cache_executor(tools: Vec<Arc<dyn Tool>>) -> Executor {
        let mut r = ToolRegistry::new();
        for t in tools {
            r.register(t);
        }
        let mut perms = PermissionManager::new();
        perms.add_allow("cread");
        perms.add_allow("cwrite");
        perms.add_allow("cnow");
        Executor::new(r, perms, Arc::new(AllowApprover), test_ctx()).with_op_cache(true)
    }

    #[tokio::test]
    async fn repeated_deterministic_read_hits_the_cache() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ex = cache_executor(vec![counting_read_tool(count.clone())]);

        let first = ex.dispatch("cread", json!({"path": "a"})).await;
        let second = ex.dispatch("cread", json!({"path": "a"})).await;
        assert!(!first.is_error && !second.is_error);
        assert_eq!(
            first.content, second.content,
            "hit replays the exact result"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "guarded IO ran exactly once"
        );

        // Audit evidence distinguishes the hit from a fresh execution.
        let hits = ex
            .ctx
            .evidence
            .lock()
            .unwrap()
            .all()
            .iter()
            .filter(|o| o.kind == "op_cache_hit")
            .count();
        assert_eq!(hits, 1, "exactly the second dispatch was a cache hit");

        // Different input → different content address → fresh execution.
        let other = ex.dispatch("cread", json!({"path": "b"})).await;
        assert!(!other.is_error);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_write_invalidates_the_cache() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let write_tool = crate::tool_fn(
            ToolSpec::read_only("cwrite", "mutates", json!({"type": "object"}))
                .with_effects(vec![Effect::Write, Effect::Filesystem])
                .with_access(vec![AccessKind::Filesystem]),
            |_params: Value| async move { Ok(Value::String("wrote".to_string())) },
        );
        let ex = cache_executor(vec![counting_read_tool(count.clone()), write_tool]);

        ex.dispatch("cread", json!({"path": "a"})).await;
        // The write starts a new invalidation generation…
        let w = ex.dispatch("cwrite", json!({"path": "a"})).await;
        assert!(!w.is_error, "{}", w.content);
        // …so the same read re-runs its guarded IO instead of replaying a stale value.
        ex.dispatch("cread", json!({"path": "a"})).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "the post-write read must not be served from cache"
        );
    }

    #[tokio::test]
    async fn non_idempotent_and_disabled_reads_bypass_the_cache() {
        // A read-only but NON-deterministic op (a clock) is never cached.
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = count.clone();
        let mut now_spec = ToolSpec::read_only("cnow", "clock", json!({"type": "object"}));
        now_spec.idempotency = Idempotency::NonIdempotent;
        let now_tool = crate::tool_fn(now_spec, move |_params: Value| {
            let c = c2.clone();
            async move {
                Ok(Value::String(format!(
                    "t{}",
                    c.fetch_add(1, Ordering::SeqCst)
                )))
            }
        });
        let ex = cache_executor(vec![now_tool]);
        ex.dispatch("cnow", json!({})).await;
        ex.dispatch("cnow", json!({})).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "non-idempotent: never cached"
        );

        // And with the cache disabled, even a deterministic read re-runs.
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut r = ToolRegistry::new();
        r.register(counting_read_tool(count.clone()));
        let mut perms = PermissionManager::new();
        perms.add_allow("cread");
        let ex = Executor::new(r, perms, Arc::new(AllowApprover), test_ctx()).with_op_cache(false);
        ex.dispatch("cread", json!({"path": "a"})).await;
        ex.dispatch("cread", json!({"path": "a"})).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "kill switch bypasses the cache"
        );
    }

    /// A-03: everything rendered into the model prompt must be byte-stable — the backing `HashMap`'s
    /// iteration order changes per process, so `specs()`/`names()` must sort. (Registration order is
    /// deliberately non-alphabetical here.)
    #[test]
    fn registry_specs_and_names_are_name_sorted() {
        let mut r = ToolRegistry::new();
        r.register(ping_tool());
        r.register(Arc::new(EchoTool));
        assert_eq!(r.names(), vec!["echo".to_string(), "ping".to_string()]);
        let spec_names: Vec<String> = r.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(spec_names, vec!["echo".to_string(), "ping".to_string()]);
    }

    /// Like [`registry`], plus [`ping_tool`] — used only by the capability-scope tests below, which
    /// need two distinct tools to prove narrowing (one allowed inside a scope, the other denied). Kept
    /// separate from `registry()` so the many pre-existing tests asserting the registry's exact name
    /// set (e.g. `subset_none_inherits_all_some_empty_grants_none`) are unaffected.
    fn registry_two_tools() -> ToolRegistry {
        let mut r = registry();
        r.register(ping_tool());
        r
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            test_ctx(),
        );
        let r = ex.dispatch("nope", json!({})).await;
        assert!(r.is_error);
        assert!(r.content.contains("unknown tool"));
    }

    #[tokio::test]
    async fn ask_then_allow_executes() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Allow,
        });
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            approver.clone(),
            test_ctx(),
        );
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(!r.is_error);
        assert_eq!(r.content, "hi");
        assert!(approver.asked.load(Ordering::Relaxed), "should have asked");
    }

    #[tokio::test]
    async fn deny_rule_blocks_without_asking() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Allow,
        });
        let perms = PermissionManager::from_rules(&[], &["echo".into()]);
        let ex = Executor::new(registry(), perms, approver.clone(), test_ctx());
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(r.is_error);
        assert!(r.content.contains("denied by permission rules"));
        assert!(!approver.asked.load(Ordering::Relaxed), "deny must not ask");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filesystem_permission_denies_granted_alias_to_ungranted_target() {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("flux-rt-path-identity-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join("allowed")).unwrap();
        std::fs::create_dir_all(dir.join("secret")).unwrap();
        std::fs::write(dir.join("secret/value.txt"), "classified").unwrap();
        std::os::unix::fs::symlink("../secret", dir.join("allowed/alias")).unwrap();

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FileReadTool));
        let perms = PermissionManager::from_rules(
            &["file_read(allowed/**)".to_string()],
            &["file_read(secret/**)".to_string()],
        );
        let ctx = ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())));
        let executor = Executor::new(registry, perms, Arc::new(DenyApprover), ctx);

        let result = executor
            .dispatch("file_read", json!({"path": "allowed/alias/value.txt"}))
            .await;
        assert!(result.is_error, "the physical target's deny must win");
        assert!(
            result.content.contains("denied by permission rules"),
            "unexpected denial: {}",
            result.content
        );
        assert!(!result.content.contains("classified"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filesystem_permission_allows_symlink_that_stays_in_granted_tree() {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("flux-rt-path-alias-ok-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join("allowed/real")).unwrap();
        std::fs::write(dir.join("allowed/real/value.txt"), "safe").unwrap();
        std::os::unix::fs::symlink("real", dir.join("allowed/alias")).unwrap();

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FileReadTool));
        let perms = PermissionManager::from_rules(&["file_read(allowed/**)".to_string()], &[]);
        let ctx = ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())));
        let executor = Executor::new(registry, perms, Arc::new(DenyApprover), ctx);

        let result = executor
            .dispatch("file_read", json!({"path": "allowed/alias/value.txt"}))
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(result.content, "safe");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-113: a reason-carrying denial APPENDS to the canonical op-anchored shape — the
    /// `` `{op}` denied by user `` prefix (and the structural `denied` classification) are
    /// unchanged, and the user's why rides behind it for the model to adapt to.
    #[tokio::test]
    async fn deny_with_reason_appends_to_the_canonical_denial_text() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::DenyWithReason("wrong environment, use staging".into()),
        });
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            approver.clone(),
            test_ctx(),
        );
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(r.is_error);
        assert!(approver.asked.load(Ordering::Relaxed));
        assert!(
            r.content.contains("`echo` denied by user"),
            "canonical shape must be intact: {}",
            r.content
        );
        assert!(
            r.content
                .contains("— reason: wrong environment, use staging"),
            "reason must be appended: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn approved_scope_skips_the_per_op_prompt() {
        // The approver would DENY if asked, so a skipped prompt is the only way the op can run.
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Deny,
        });
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            approver.clone(),
            test_ctx(),
        );

        // Outside any approved scope: the op prompts (and is denied).
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(r.is_error, "outside a scope the op prompts and is denied");
        assert!(approver.asked.load(Ordering::Relaxed));

        // Inside an approved-plan scope: no prompt, the op runs.
        approver.asked.store(false, Ordering::Relaxed);
        let r = {
            let _scope = ex.enter_approved_scope(false);
            ex.dispatch("echo", json!({"text": "hi"})).await
        };
        assert!(
            !r.is_error,
            "inside an approved scope the op runs: {}",
            r.content
        );
        assert_eq!(r.content, "hi");
        assert!(
            !approver.asked.load(Ordering::Relaxed),
            "no per-op prompt inside an approved scope"
        );

        // Scope closed (guard dropped): prompts again next time.
        approver.asked.store(false, Ordering::Relaxed);
        let _ = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "scope closed → prompts again"
        );
    }

    #[tokio::test]
    async fn approved_scope_still_respects_deny_rules() {
        let perms = PermissionManager::from_rules(&[], &["echo".into()]);
        let ex = Executor::new(registry(), perms, Arc::new(AllowApprover), test_ctx());
        let _scope = ex.enter_approved_scope(false);
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(
            r.is_error,
            "a deny rule still blocks inside an approved plan"
        );
        assert!(r.content.contains("denied by permission rules"));
    }

    // ---- capability scopes (`with_tools` / L-11) ----

    #[tokio::test]
    async fn no_active_scope_is_a_strict_no_op() {
        // Empty stack: every existing flow that never opens a `with_tools` scope is unaffected.
        let ex = Executor::new(
            registry_two_tools(),
            PermissionManager::new(),
            Arc::new(AllowApprover),
            test_ctx(),
        );
        assert_eq!(ex.active_cap_scope(), None);
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(!r.is_error);
        let r = ex.dispatch("ping", json!({})).await;
        assert!(!r.is_error);
    }

    #[tokio::test]
    async fn scope_allows_the_named_tool_and_denies_the_rest() {
        let ex = Executor::new(
            registry_two_tools(),
            PermissionManager::new(),
            Arc::new(AllowApprover),
            test_ctx(),
        );
        let _scope = ex.push_cap_scope(&["ping".to_string()]);

        let allowed = ex.dispatch("ping", json!({})).await;
        assert!(!allowed.is_error, "ping is in the scope's allowlist");
        assert_eq!(allowed.content, "pong");

        let denied = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(denied.is_error, "echo is outside the scope's allowlist");
        assert!(
            denied.content.contains("denied by capability scope"),
            "got: {}",
            denied.content
        );
    }

    #[test]
    fn operation_visibility_intersects_bare_denies_and_active_capability_scope() {
        let permissions = PermissionManager::from_rules(&[], &["echo".into()]);
        let executor = Executor::new(
            registry_two_tools(),
            permissions,
            Arc::new(AllowApprover),
            test_ctx(),
        );
        assert!(!executor.operation_visible("echo"));
        assert!(executor.operation_visible("ping"));

        let _scope = executor.push_cap_scope(&["echo".to_string()]);
        assert!(!executor.operation_visible("echo"), "deny still wins");
        assert!(
            !executor.operation_visible("ping"),
            "the active capability scope is also a visibility ceiling"
        );
    }

    #[tokio::test]
    async fn scope_denial_wins_even_when_policy_and_permissions_would_allow() {
        // The permission rules explicitly allow `echo`, and there's no policy floor configured — the
        // outer session would allow the call. The active scope must still deny it: capabilities only
        // ever narrow, never widen, what the outer layers already permit.
        let perms = PermissionManager::from_rules(&["echo".into()], &[]);
        let ex = Executor::new(
            registry_two_tools(),
            perms,
            Arc::new(AllowApprover),
            test_ctx(),
        );
        let _scope = ex.push_cap_scope(&["ping".to_string()]);
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(r.is_error, "scope denies even a permission-allowed tool");
        assert!(r.content.contains("denied by capability scope"));
    }

    #[tokio::test]
    async fn scope_closes_on_guard_drop_and_restores_the_outer_set() {
        let ex = Executor::new(
            registry_two_tools(),
            PermissionManager::new(),
            Arc::new(AllowApprover),
            test_ctx(),
        );
        {
            let _scope = ex.push_cap_scope(&["ping".to_string()]);
            assert!(ex.dispatch("echo", json!({"text": "hi"})).await.is_error);
        }
        // Guard dropped: the scope stack is empty again, so echo is allowed once more.
        assert_eq!(ex.active_cap_scope(), None);
        assert!(!ex.dispatch("echo", json!({"text": "hi"})).await.is_error);
    }

    #[tokio::test]
    async fn scope_pops_even_when_the_body_errors() {
        // A denial inside the scope must not leak/corrupt the stack — the guard's `Drop` runs
        // regardless of how the caller's scope block exits.
        let ex = Executor::new(
            registry_two_tools(),
            PermissionManager::new(),
            Arc::new(AllowApprover),
            test_ctx(),
        );
        {
            let _scope = ex.push_cap_scope(&["ping".to_string()]);
            let _ = ex.dispatch("echo", json!({"text": "hi"})).await; // denied, body "errors"
        }
        assert_eq!(
            ex.active_cap_scope(),
            None,
            "pop happened despite the denial"
        );
    }

    #[tokio::test]
    async fn nested_scope_narrows_and_never_widens() {
        let ex = Executor::new(
            registry_two_tools(),
            PermissionManager::new(),
            Arc::new(AllowApprover),
            test_ctx(),
        );
        let _outer = ex.push_cap_scope(&["ping".to_string()]);
        // Inner scope asks for BOTH tools, but the outer only allowed `ping` — the intersection must
        // still exclude `echo`, proving nesting can only narrow.
        let _inner = ex.push_cap_scope(&["ping".to_string(), "echo".to_string()]);
        assert_eq!(ex.active_cap_scope(), Some(vec!["ping".to_string()]));
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(
            r.is_error,
            "inner scope cannot re-grant what the outer removed"
        );
        let r = ex.dispatch("ping", json!({})).await;
        assert!(!r.is_error);
    }

    #[tokio::test]
    async fn denial_and_scope_boundaries_are_recorded_in_evidence() {
        let ex = Executor::new(
            registry_two_tools(),
            PermissionManager::new(),
            Arc::new(AllowApprover),
            test_ctx(),
        );
        {
            let _scope = ex.push_cap_scope(&["ping".to_string()]);
            let _ = ex.dispatch("echo", json!({"text": "hi"})).await;
        }
        let log = ex.evidence();
        assert!(
            log.all().iter().any(|o| o.kind == "cap_scope_enter"),
            "scope entry must be recorded"
        );
        assert!(
            log.all().iter().any(|o| o.kind == "cap_scope_denied"),
            "denial must be recorded"
        );
        assert!(
            log.all().iter().any(|o| o.kind == "cap_scope_exit"),
            "scope exit must be recorded"
        );
    }

    #[tokio::test]
    async fn empty_scope_denies_every_tool() {
        // `with_tools []` — the strictest scope: no tool at all, mirroring `subset(Some(&[]))`.
        let ex = Executor::new(
            registry_two_tools(),
            PermissionManager::new(),
            Arc::new(AllowApprover),
            test_ctx(),
        );
        let _scope = ex.push_cap_scope(&[]);
        assert!(ex.dispatch("ping", json!({})).await.is_error);
        assert!(ex.dispatch("echo", json!({"text": "hi"})).await.is_error);
    }

    #[tokio::test]
    async fn approve_plan_opens_scope_and_always_trusts_the_session() {
        // `RecordingApprover` only implements `request`; `request_plan` uses the trait default that
        // delegates to it, so this also covers the default delegation.
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::AllowAlways("*plans*".into()),
        });
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            approver.clone(),
            test_ctx(),
        );
        assert!(!ex.in_approved_scope());
        {
            let scope = ex.approve_plan(&plan_request("medium · mutating", 2)).await;
            assert!(scope.is_some(), "Allow/AllowAlways opens a scope");
            assert!(ex.in_approved_scope());
        }
        // `always` set the session-wide trust, so we stay approved after the guard drops.
        assert!(
            ex.in_approved_scope(),
            "`always` trusts every plan for the rest of the session"
        );
        approver.asked.store(false, Ordering::Relaxed);
        let _ = ex.approve_plan(&plan_request("low", 1)).await;
        assert!(
            !approver.asked.load(Ordering::Relaxed),
            "a trusted session does not prompt again"
        );
    }

    #[tokio::test]
    async fn approve_plan_deny_returns_none() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Deny,
        });
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            approver.clone(),
            test_ctx(),
        );
        assert!(
            ex.approve_plan(&plan_request("medium", 1)).await.is_none(),
            "Deny → no scope"
        );
        assert!(!ex.in_approved_scope());
    }

    #[tokio::test]
    async fn request_plan_approval_does_not_open_an_execution_scope() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Allow,
        });
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            approver.clone(),
            test_ctx(),
        );

        assert!(ex.request_plan_approval(&plan_request("medium", 1)).await);
        assert!(approver.asked.load(Ordering::Relaxed));
        assert!(
            !ex.in_approved_scope(),
            "approval and execution are separate phases; the receipt holder opens the scope later"
        );
        assert!(!ex.approval_context().is_empty());
    }

    #[tokio::test]
    async fn undisclosed_destructive_op_refires_approval_inside_approved_scope() {
        // The plan was approved WITHOUT a destructive badge (the risk preview only sees literal
        // args), so a destructive-intent op assembled at runtime must re-fire the per-op gate even
        // inside the approved scope — and a denying approver must block it.
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Deny,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(RmTool));
        let ex = Executor::new(reg, PermissionManager::new(), approver.clone(), test_ctx());

        let _scope = ex.enter_approved_scope(false); // approval never disclosed a destructive op
        let r = ex.dispatch("rm", json!({})).await;
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "an undisclosed destructive op must re-fire the approval gate inside the scope"
        );
        assert!(r.is_error, "the denying approver blocks it: {}", r.content);
        assert!(r.content.contains("denied by user"));
    }

    /// C-27: the undisclosed-destructive gate must key on the INNERMOST scope's own disclosure, not
    /// on whether any ancestor scope disclosed. Before the fix, `destructive_scope` was a bare shared
    /// depth counter: an outer disclosed scope left it `>0`, so a nested plan approved
    /// `destructive:false` silently inherited the outer disclosure and never re-fired the gate — a
    /// `$symbol`-assembled `rm -rf`, invisible to the nested plan's static risk preview, would then
    /// dispatch with no prompt at all.
    #[tokio::test]
    async fn undisclosed_destructive_op_refires_approval_inside_nested_disclosed_scope() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Deny,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(RmTool));
        let ex = Executor::new(reg, PermissionManager::new(), approver.clone(), test_ctx());

        // Outer plan's approval DID disclose a destructive op...
        let _outer = ex.enter_approved_scope(true);
        // ...but the nested plan's own approval did NOT.
        let _inner = ex.enter_approved_scope(false);
        let r = ex.dispatch("rm", json!({})).await;
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "the nested scope's own (undisclosed) approval must re-fire the gate, regardless of \
             the outer scope's disclosure"
        );
        assert!(r.is_error, "the denying approver blocks it: {}", r.content);
        assert!(r.content.contains("denied by user"));
    }

    #[tokio::test]
    async fn disclosed_destructive_plan_runs_without_per_op_reprompt() {
        // The plan approval DID disclose the destructive op (request.destructive == true), so the
        // per-op gate stays skipped inside the scope — no interactive double-prompt.
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Allow,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(RmTool));
        let ex = Executor::new(reg, PermissionManager::new(), approver.clone(), test_ctx());

        let request = PlanApprovalRequest {
            destructive: true,
            ..plan_request("destructive · contains a destructive operation", 1)
        };
        let scope = ex.approve_plan(&request).await;
        assert!(scope.is_some(), "the approver allowed the disclosed plan");
        approver.asked.store(false, Ordering::Relaxed);
        let r = ex.dispatch("rm", json!({})).await;
        assert!(
            !r.is_error,
            "the disclosed destructive op runs: {}",
            r.content
        );
        assert!(
            !approver.asked.load(Ordering::Relaxed),
            "no per-op re-prompt when the plan approval disclosed the destructive op"
        );
        drop(scope);

        // Once the scope closes, the disclosure closes with it: the same op prompts again.
        let _ = ex.dispatch("rm", json!({})).await;
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "scope closed → the destructive op prompts again"
        );
    }

    #[tokio::test]
    async fn allow_rule_executes_without_asking() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Deny, // would deny if asked
        });
        let perms = PermissionManager::from_rules(&["echo".into()], &[]);
        let ex = Executor::new(registry(), perms, approver.clone(), test_ctx());
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(!r.is_error);
        assert!(
            !approver.asked.load(Ordering::Relaxed),
            "allow must not ask"
        );
    }

    /// A tool that echoes a fixed string back as successful content (used to test redaction).
    struct LeakTool(String);
    #[async_trait]
    impl Tool for LeakTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("leak", "echo content", json!({"type": "object"}))
        }
        async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(self.0.clone()))
        }
    }

    #[tokio::test]
    async fn secrets_redacted_from_success_output() {
        let secret = "sk-ant-supersecretvalue123456";
        let ctx = test_ctx();
        ctx.redactor.add_secret(secret);

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(LeakTool(format!("the key is {secret} ok"))));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["leak".into()], &[]),
            Arc::new(DenyApprover),
            ctx,
        );
        let r = ex.dispatch("leak", json!({})).await;
        assert!(!r.is_error);
        assert!(!r.content.contains(secret), "secret leaked: {}", r.content);
        assert!(r.content.contains("[redacted]"));
    }

    #[test]
    fn secret_resolver_reads_env_and_seeds_redactor() {
        let key = format!("FLUX_TEST_SECRET_{}", std::process::id());
        std::env::set_var(&key, "topsecretvalue");
        let mut redactor = Redactor::new();
        SecretResolver::new().seed_redactor(&mut redactor, &[flux_secret::Ref::env(&key)]);
        assert_eq!(redactor.redact("x topsecretvalue y"), "x [redacted] y");
        std::env::remove_var(&key);
    }

    /// A tool that declares a destructive command intent (but does nothing).
    struct DestructiveTool;
    #[async_trait]
    impl Tool for DestructiveTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("danger", "destructive", json!({"type": "object"}))
                .with_effects(vec![Effect::Process])
                .with_access(vec![AccessKind::Process])
                .with_risk(Risk::High)
        }
        fn intents(&self, _p: &Value) -> IntentSet {
            use flux_spec::{Intent, IntentBehavior, IntentCertainty, IntentRole, IntentTarget};
            let mut s = IntentSet::new();
            s.push(Intent {
                behavior: IntentBehavior::CommandExecution,
                target: IntentTarget::Process {
                    command: "rm -rf /tmp/x".into(),
                },
                role: IntentRole::ProcessCommand,
                certainty: IntentCertainty::Certain,
            });
            s
        }
        async fn execute(&self, _ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("ran"))
        }
    }

    #[tokio::test]
    async fn destructive_op_is_escalated_and_recorded_even_under_allow_rule() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Deny, // user declines the forced prompt
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(DestructiveTool));
        // A bare allow-rule that would normally skip the approval prompt entirely.
        let perms = PermissionManager::from_rules(&["danger".into()], &[]);
        let ex = Executor::new(reg, perms, approver.clone(), test_ctx());

        let r = ex.dispatch("danger", json!({})).await;
        assert!(r.is_error, "the forced approval was declined → denied");
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "a destructive op must ask for approval despite the allow-rule"
        );
        let ev = ex.evidence();
        assert_eq!(ev.by_kind(KIND_DESTRUCTIVE).count(), 1);
        assert!(ev.by_kind("tool_call").count() >= 1);
    }

    /// Locks the documented `flux run --yes` contract (C-45 / beta F-003): the headless allow-all
    /// approver that `--yes` installs approves destructive ops too. The point is that the destructive
    /// gate still *fires* (the intent is escalated and recorded as `KIND_DESTRUCTIVE`) — it is answered
    /// `Allow`, not bypassed. The safety docs describe exactly this: `--yes` does not exempt destructive
    /// ops from the gate; it answers the gate "yes" for them.
    #[tokio::test]
    async fn allow_approver_auto_approves_a_destructive_op_but_still_escalates_it() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(DestructiveTool));
        // Even a bare allow-rule would force the destructive gate; `--yes` answers it allow-all.
        let perms = PermissionManager::from_rules(&["danger".into()], &[]);
        let ex = Executor::new(reg, perms, Arc::new(AllowApprover), test_ctx());

        let r = ex.dispatch("danger", json!({})).await;
        assert!(
            !r.is_error,
            "--yes (AllowApprover) approves the destructive op"
        );
        assert_eq!(r.content, "ran");
        // The gate still fired and recorded the escalation — allow-all is an approval, not a bypass.
        let ev = ex.evidence();
        assert_eq!(ev.by_kind(KIND_DESTRUCTIVE).count(), 1);
    }

    /// A tool that declares a filesystem-write effect (used to test the policy floor).
    struct WriteishTool;
    #[async_trait]
    impl Tool for WriteishTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("save", "save", json!({"type": "object"}))
                .with_effects(vec![Effect::Write, Effect::Filesystem])
                .with_access(vec![AccessKind::Filesystem])
        }
        fn permission_subjects(&self, _p: &Value) -> Vec<String> {
            vec!["out.txt".into()]
        }
        async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("saved"))
        }
    }

    #[tokio::test]
    async fn policy_denies_op_outside_grant_set_even_when_rules_allow() {
        use flux_policy::{Grant, SubjectKind, SubjectRef};
        // A policy that grants only reads — write is outside the grant set (default-deny).
        let read_only = AuthorizationPolicy {
            grants: vec![Grant {
                subjects: vec![SubjectRef {
                    kind: SubjectKind::User,
                    id: "*".into(),
                }],
                resources: vec![ResourceRef::path("*")],
                actions: vec![Action::from("workspace.read")],
                required_trust: TrustLevel::Untrusted,
                required_scopes: Vec::new(),
                requires_approval: false,
            }],
        };
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(WriteishTool));
        // A permissive allow-rule + auto-approver would normally let the write through.
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["save".into()], &[]),
            Arc::new(AllowApprover),
            test_ctx(),
        )
        .with_policy(read_only);
        let r = ex.dispatch("save", json!({})).await;
        assert!(r.is_error);
        assert!(r.content.contains("denied by policy"), "got: {}", r.content);
    }

    struct SemanticTool(&'static str);

    #[async_trait]
    impl Tool for SemanticTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                format!("semantic_{}", self.0),
                "semantic authority probe",
                json!({"type": "object"}),
            )
        }

        fn permission_subjects(&self, _params: &Value) -> Vec<String> {
            if self.0 == "write_db" {
                vec!["datasource:test.records".to_string()]
            } else {
                Vec::new()
            }
        }

        fn semantic_effects(&self) -> Vec<String> {
            vec![self.0.to_string()]
        }

        async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("ran"))
        }
    }

    fn semantic_policy(
        action: &str,
        resource: ResourceRef,
        requires_approval: bool,
    ) -> AuthorizationPolicy {
        use flux_policy::{Grant, SubjectKind, SubjectRef};
        let subject = || SubjectRef {
            kind: SubjectKind::User,
            id: "*".into(),
        };
        AuthorizationPolicy {
            grants: vec![
                // `send_external` also carries real network egress; grant that baseline so the
                // semantic action itself is the decision under test.
                Grant {
                    subjects: vec![subject()],
                    resources: vec![ResourceRef::any(ResourceKind::Network)],
                    actions: vec![Action::from("network.fetch")],
                    required_trust: TrustLevel::Untrusted,
                    required_scopes: Vec::new(),
                    requires_approval: false,
                },
                Grant {
                    subjects: vec![subject()],
                    resources: vec![resource],
                    actions: vec![Action::from(action)],
                    required_trust: TrustLevel::Untrusted,
                    required_scopes: Vec::new(),
                    requires_approval,
                },
            ],
        }
    }

    #[tokio::test]
    async fn semantic_write_db_delete_money_and_send_external_fail_closed_at_dispatch() {
        let cases = [
            ("write_db", "flow.write_db"),
            ("delete", "flow.delete"),
            ("money", "flow.money"),
            ("send_external", "flow.send_external"),
        ];
        for (tag, action) in cases {
            let mut registry = ToolRegistry::new();
            registry.register(Arc::new(SemanticTool(tag)));
            let tool = format!("semantic_{tag}");
            let executor = Executor::new(
                registry,
                PermissionManager::from_rules(std::slice::from_ref(&tool), &[]),
                Arc::new(AllowApprover),
                test_ctx(),
            )
            .with_policy(semantic_policy(
                "unrelated.action",
                ResourceRef::any(ResourceKind::Operation),
                false,
            ));

            let result = executor.dispatch(&tool, json!({})).await;
            assert!(
                result.is_error && result.content.contains(action),
                "{tag} must be denied on its exact semantic action: {}",
                result.content
            );
        }
    }

    #[tokio::test]
    async fn semantic_actions_can_require_approval_at_dispatch() {
        let cases = [
            (
                "write_db",
                "flow.write_db",
                ResourceRef::any(ResourceKind::Datasource),
            ),
            (
                "delete",
                "flow.delete",
                ResourceRef::any(ResourceKind::Operation),
            ),
            (
                "money",
                "flow.money",
                ResourceRef::any(ResourceKind::Operation),
            ),
            (
                "send_external",
                "flow.send_external",
                ResourceRef::any(ResourceKind::Operation),
            ),
        ];
        for (tag, action, resource) in cases {
            let approver = Arc::new(RecordingApprover {
                asked: AtomicBool::new(false),
                choice: || ApprovalChoice::Allow,
            });
            let mut registry = ToolRegistry::new();
            registry.register(Arc::new(SemanticTool(tag)));
            let tool = format!("semantic_{tag}");
            let executor = Executor::new(
                registry,
                PermissionManager::from_rules(std::slice::from_ref(&tool), &[]),
                approver.clone(),
                test_ctx(),
            )
            .with_policy(semantic_policy(action, resource, true));

            let result = executor.dispatch(&tool, json!({})).await;
            assert!(!result.is_error, "{tag}: {}", result.content);
            assert!(
                approver.asked.load(Ordering::Relaxed),
                "{tag} must force the approval gate"
            );
        }
    }

    #[test]
    fn unknown_semantic_authority_is_rejected_at_registration() {
        let mut registry = ToolRegistry::new();
        let error = registry
            .try_register_from("future-plugin", Arc::new(SemanticTool("future_unknown")))
            .unwrap_err();
        assert!(error.to_string().contains("unknown semantic authority"));
        assert!(registry.names().is_empty());
    }

    #[test]
    fn duplicate_registration_rejects_identical_and_conflicting_specs_with_sources() {
        let mut registry = ToolRegistry::new();
        registry
            .try_register_from("builtins", Arc::new(EchoTool))
            .unwrap();

        let identical = registry
            .try_register_from("plugin:/tmp/echo", Arc::new(EchoTool))
            .unwrap_err()
            .to_string();
        assert!(identical.contains("duplicate operation `echo`"));
        assert!(identical.contains("identical declaration"));
        assert!(identical.contains("plugin:/tmp/echo"));
        assert!(identical.contains("builtins"));

        let conflicting = tool_fn(
            ToolSpec::read_only(
                "echo",
                "different handler contract",
                json!({"type": "object"}),
            ),
            |_params| async { Ok(Value::String("replacement".into())) },
        );
        let conflict = registry
            .try_register_from("plugin:/tmp/conflict", conflicting.clone())
            .unwrap_err()
            .to_string();
        assert!(conflict.contains("conflicting declaration"));

        let old = registry
            .replace_from("explicit-test-override", conflicting)
            .unwrap();
        assert!(old.is_some());
        assert_eq!(
            registry.get("echo").unwrap().spec().description,
            "different handler contract"
        );
        assert_eq!(registry.source("echo"), Some("explicit-test-override"));
    }

    #[test]
    fn failed_pack_and_registry_composition_are_atomic() {
        let mut registry = ToolRegistry::new();
        registry
            .try_register_from("builtins", Arc::new(EchoTool))
            .unwrap();
        let fresh = tool_fn(
            ToolSpec::read_only("fresh", "fresh", json!({"type": "object"})),
            |_params| async { Ok(Value::Null) },
        );

        let error = registry
            .try_register_all_from("plugin:alpha", vec![fresh, Arc::new(EchoTool)])
            .unwrap_err()
            .to_string();
        assert!(error.contains("plugin:alpha"), "{error}");
        assert_eq!(registry.names(), vec!["echo"]);
        assert_eq!(registry.source("echo"), Some("builtins"));

        let mut contributed = ToolRegistry::new();
        contributed
            .try_register_from(
                "plugin:beta",
                tool_fn(
                    ToolSpec::read_only("another", "another", json!({"type": "object"})),
                    |_params| async { Ok(Value::Null) },
                ),
            )
            .unwrap();
        contributed
            .try_register_from("plugin:beta", Arc::new(EchoTool))
            .unwrap();

        let error = registry.try_extend(contributed).unwrap_err().to_string();
        assert!(error.contains("plugin:beta"), "{error}");
        assert!(registry.get("another").is_none());
        assert_eq!(registry.names(), vec!["echo"]);
    }

    #[test]
    fn typed_authority_catalog_matrix_is_resource_specific() {
        let schema = json!({"type": "object"});
        let pure = ToolSpec::read_only("pure", "pure", schema.clone());
        assert!(authority_requirements_from_declaration(&pure, &[], &[])
            .unwrap()
            .is_empty());

        let filesystem = ToolSpec::read_only("read", "read", schema.clone())
            .with_access(vec![AccessKind::Filesystem]);
        assert_eq!(
            authority_requirements_from_declaration(&filesystem, &["src/lib.rs".into()], &[])
                .unwrap(),
            vec![AuthorityRequirement::workspace_read("src/lib.rs")]
        );

        let filesystem_write = ToolSpec::read_only("write", "write", schema.clone())
            .with_effects(vec![Effect::Write])
            .with_access(vec![AccessKind::Filesystem]);
        assert_eq!(
            authority_requirements_from_declaration(
                &filesystem_write,
                &["src/lib.rs".into()],
                &["write_file".into()],
            )
            .unwrap(),
            vec![AuthorityRequirement::workspace_write("src/lib.rs")]
        );

        let datasource = ToolSpec::read_only("search", "search", schema.clone())
            .with_access(vec![AccessKind::Datasource]);
        assert_eq!(
            authority_requirements_from_declaration(
                &datasource,
                &["datasource:docs/page".into()],
                &[],
            )
            .unwrap(),
            vec![AuthorityRequirement::datasource_read("docs/page")]
        );

        let datasource_write = ToolSpec::read_only("index", "index", schema.clone())
            .with_effects(vec![Effect::Write])
            .with_access(vec![AccessKind::Datasource]);
        assert_eq!(
            authority_requirements_from_declaration(
                &datasource_write,
                &["datasource:docs/page".into()],
                &["write_db".into()],
            )
            .unwrap(),
            vec![
                AuthorityRequirement::new(
                    "datasource.write",
                    ResourceRef::named(ResourceKind::Datasource, "docs/page"),
                ),
                AuthorityRequirement::new(
                    "flow.write_db",
                    ResourceRef::named(ResourceKind::Datasource, "docs/page"),
                ),
            ]
        );

        let network = ToolSpec::read_only("fetch", "fetch", schema.clone())
            .with_effects(vec![Effect::Network])
            .with_access(vec![AccessKind::Network]);
        assert_eq!(
            authority_requirements_from_declaration(&network, &["https://example.com".into()], &[])
                .unwrap(),
            vec![AuthorityRequirement::network_fetch("https://example.com")]
        );

        let connection = ToolSpec::read_only("query", "query", schema.clone())
            .with_effects(vec![Effect::Network])
            .with_access(vec![AccessKind::Connection]);
        assert_eq!(
            authority_requirements_from_declaration(
                &connection,
                &["tcp:db.example:5432".into()],
                &[],
            )
            .unwrap(),
            vec![AuthorityRequirement::connection_dial("tcp:db.example:5432")]
        );

        let process = ToolSpec::read_only("run", "run", schema.clone())
            .with_effects(vec![Effect::Process])
            .with_access(vec![AccessKind::Process]);
        assert_eq!(
            authority_requirements_from_declaration(&process, &["cargo:test".into()], &[]).unwrap(),
            vec![AuthorityRequirement::process_exec("cargo:test")]
        );

        let secret = ToolSpec::read_only("credential", "credential", schema.clone())
            .with_access(vec![AccessKind::Secret]);
        assert_eq!(
            authority_requirements_from_declaration(&secret, &["TAVILY_API_KEY".into()], &[])
                .unwrap(),
            vec![AuthorityRequirement::secret_read("TAVILY_API_KEY")]
        );

        let browser = ToolSpec::read_only("browse", "browse", schema.clone())
            .with_effects(vec![Effect::Browser, Effect::Network])
            .with_access(vec![AccessKind::Browser]);
        assert_eq!(
            authority_requirements_from_declaration(
                &browser,
                &["https://example.com/app".into()],
                &[],
            )
            .unwrap(),
            vec![AuthorityRequirement::browser_navigate(
                "https://example.com/app"
            )]
        );

        let provider = ToolSpec::read_only("think", "think", schema.clone())
            .with_effects(vec![Effect::Network])
            .with_access(vec![AccessKind::Provider]);
        assert_eq!(
            authority_requirements_from_declaration(
                &provider,
                &["anthropic/claude-sonnet".into()],
                &[],
            )
            .unwrap(),
            vec![AuthorityRequirement::provider_invoke(
                "anthropic/claude-sonnet"
            )]
        );

        let host = ToolSpec::read_only("settings", "settings", schema.clone())
            .with_access(vec![AccessKind::LocalSystem]);
        assert_eq!(
            authority_requirements_from_declaration(&host, &[], &[]).unwrap(),
            vec![AuthorityRequirement::host_read("settings")]
        );

        let host_write = ToolSpec::read_only("settings.save", "settings", schema.clone())
            .with_effects(vec![Effect::LocalSystem])
            .with_access(vec![AccessKind::LocalSystem]);
        assert_eq!(
            authority_requirements_from_declaration(&host_write, &[], &[]).unwrap(),
            vec![AuthorityRequirement::host_write("settings.save")]
        );

        let auth =
            ToolSpec::read_only("login", "login", schema).with_access(vec![AccessKind::Auth]);
        assert_eq!(
            authority_requirements_from_declaration(&auth, &[], &[]).unwrap(),
            vec![AuthorityRequirement::host_read("auth")]
        );
    }

    /// A subprocess is a legitimate carrier for reach and for mutation: the CLI it runs is what
    /// touches the remote API and what changes the world. The authority gate is the
    /// `process.exec` requirement on the named program, so the declaration must be expressible
    /// instead of being rejected as inconsistent — otherwise a process-mediated integration has
    /// to claim network or filesystem access it never uses just to register.
    #[test]
    fn process_access_carries_network_and_write_effects() {
        let schema = json!({"type": "object"});

        // `kubernetes.secret.read` — reads a remote cluster through `kubectl`.
        let read = ToolSpec::read_only("kubernetes.secret.read", "read a secret", schema.clone())
            .with_effects(vec![Effect::Read, Effect::Network])
            .with_access(vec![AccessKind::Process]);
        assert_eq!(
            authority_requirements_from_declaration(&read, &["kubectl".into()], &[]).unwrap(),
            vec![AuthorityRequirement::process_exec("kubectl")]
        );

        // `kubernetes.deployment.scale` — mutates a remote cluster through `kubectl`. The write
        // is not a file and not a datasource, so it lands on the operation itself.
        let write = ToolSpec::read_only("kubernetes.deployment.scale", "scale", schema)
            .with_effects(vec![Effect::Write, Effect::Network])
            .with_access(vec![AccessKind::Process]);
        assert_eq!(
            authority_requirements_from_declaration(&write, &["kubectl".into()], &[]).unwrap(),
            vec![
                AuthorityRequirement::process_exec("kubectl"),
                AuthorityRequirement::operation("operation.mutate", "kubernetes.deployment.scale"),
            ]
        );
    }

    #[test]
    fn typed_authority_concrete_subjects_are_normalized_by_resource_family() {
        let spec = ToolSpec::read_only("mixed", "mixed", json!({"type": "object"}))
            .with_effects(vec![Effect::Process, Effect::Network, Effect::Browser])
            .with_access(vec![
                AccessKind::Process,
                AccessKind::Network,
                AccessKind::Browser,
                AccessKind::Provider,
            ]);
        let subjects = vec![
            " concrete-target ".into(),
            "datasource:web.page".into(),
            "*".into(),
            "concrete-target".into(),
            "   ".into(),
        ];

        assert_eq!(
            authority_requirements_from_declaration(&spec, &subjects, &[]).unwrap(),
            vec![
                AuthorityRequirement::process_exec("concrete-target"),
                AuthorityRequirement::network_fetch("concrete-target"),
                AuthorityRequirement::browser_navigate("concrete-target"),
                AuthorityRequirement::provider_invoke("concrete-target"),
            ]
        );
    }

    #[test]
    fn semantic_network_and_model_authority_preserve_concrete_subjects() {
        let spec = ToolSpec::read_only("semantic", "semantic", json!({"type": "object"}));

        assert_eq!(
            authority_requirements_from_declaration(
                &spec,
                &["anthropic".into()],
                &["model".into()],
            )
            .unwrap(),
            vec![AuthorityRequirement::provider_invoke("anthropic")]
        );
        assert_eq!(
            authority_requirements_from_declaration(
                &spec,
                &["https://example.com/api".into()],
                &["network".into()],
            )
            .unwrap(),
            vec![AuthorityRequirement::network_fetch(
                "https://example.com/api"
            )]
        );
        assert_eq!(
            authority_requirements_from_declaration(
                &spec,
                &["https://example.com/outbox".into()],
                &["send_external".into()],
            )
            .unwrap(),
            vec![
                AuthorityRequirement::network_fetch("https://example.com/outbox"),
                AuthorityRequirement::operation("flow.send_external", "semantic"),
            ]
        );
    }

    #[test]
    fn typed_authority_resource_families_fall_back_to_wildcards_without_concrete_subjects() {
        let schema = json!({"type": "object"});
        let cases = [
            (
                ToolSpec::read_only("process", "process", schema.clone())
                    .with_effects(vec![Effect::Process])
                    .with_access(vec![AccessKind::Process]),
                AuthorityRequirement::process_exec("*"),
            ),
            (
                ToolSpec::read_only("network", "network", schema.clone())
                    .with_effects(vec![Effect::Network])
                    .with_access(vec![AccessKind::Network]),
                AuthorityRequirement::network_fetch("*"),
            ),
            (
                ToolSpec::read_only("browser", "browser", schema.clone())
                    .with_effects(vec![Effect::Browser, Effect::Network])
                    .with_access(vec![AccessKind::Browser]),
                AuthorityRequirement::browser_navigate("*"),
            ),
            (
                ToolSpec::read_only("provider", "provider", schema)
                    .with_effects(vec![Effect::Network])
                    .with_access(vec![AccessKind::Provider]),
                AuthorityRequirement::provider_invoke("*"),
            ),
        ];

        for (spec, expected) in cases {
            assert_eq!(
                authority_requirements_from_declaration(&spec, &[], &[]).unwrap(),
                vec![expected],
                "{} must conservatively request its whole resource family",
                spec.name
            );
        }
    }

    /// A read-effect tool gated only by the policy floor (permissive rules, auto-approve).
    struct ReadishTool;
    #[async_trait]
    impl Tool for ReadishTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("peek", "read", json!({"type": "object"}))
                .with_effects(vec![Effect::Read])
                .with_access(vec![AccessKind::Filesystem])
        }
        fn permission_subjects(&self, _p: &Value) -> Vec<String> {
            vec!["input.txt".into()]
        }
        async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("read"))
        }
    }

    /// A lexical turn identity governs policy and approval context without mutating the executor's
    /// assembly-time fallback. Exiting the scope restores that fallback.
    #[tokio::test]
    async fn lexical_turn_identity_scopes_policy_subject_and_restores_default() {
        use flux_policy::{Grant, SubjectKind, SubjectRef};
        let ident = |id: &str| {
            (
                Caller {
                    principal: Principal {
                        id: id.into(),
                        name: id.into(),
                        kind: CallerKind::User,
                    },
                    groups: Vec::new(),
                    source: "test".into(),
                },
                Trust {
                    kind: TrustKind::Invocation,
                    level: TrustLevel::Verified,
                    scopes: Vec::new(),
                },
            )
        };
        // Reads granted to alice ONLY — default-deny for every other principal.
        let alice_only = AuthorizationPolicy {
            grants: vec![Grant {
                subjects: vec![SubjectRef {
                    kind: SubjectKind::User,
                    id: "alice".into(),
                }],
                resources: vec![ResourceRef::path("*")],
                actions: vec![Action::from("workspace.read")],
                required_trust: TrustLevel::Untrusted,
                required_scopes: Vec::new(),
                requires_approval: false,
            }],
        };
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadishTool));
        let (caller, trust) = ident("bob");
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["peek".into()], &[]),
            Arc::new(AllowApprover),
            test_ctx(),
        )
        .with_policy(alice_only.clone())
        .with_identity(caller, trust);

        let r = ex.dispatch("peek", json!({})).await;
        assert!(
            r.is_error && r.content.contains("denied by policy"),
            "bob is outside the grant set: {}",
            r.content
        );

        let (caller, trust) = ident("alice");
        let alice = TurnIdentity::new(caller, trust);
        let r = scope_runtime_turn(
            RuntimeTurnContext::new().with_identity(alice.clone()),
            ex.dispatch("peek", json!({})),
        )
        .await;
        assert!(!r.is_error, "alice is granted reads: {}", r.content);

        let default_context: Value = serde_json::from_str(&ex.approval_context()).unwrap();
        assert_eq!(default_context["caller"]["principal"]["id"], "bob");
        let r = ex.dispatch("peek", json!({})).await;
        assert!(
            r.is_error,
            "alice's lexical grant must not stick to the default caller: {}",
            r.content
        );

        // A fresh one-shot runtime may have to cross `tokio::spawn`, whose task does not inherit
        // Tokio task-locals. The pinned snapshot must govern policy too, not only what ToolContext
        // reports to the operation itself.
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadishTool));
        let mut pinned_ctx = test_ctx();
        pinned_ctx.set_runtime_turn_context(RuntimeTurnContext::new().with_identity(alice));
        let (caller, trust) = ident("bob");
        let pinned = Executor::new(
            reg,
            PermissionManager::from_rules(&["peek".into()], &[]),
            Arc::new(AllowApprover),
            pinned_ctx,
        )
        .with_policy(alice_only)
        .with_identity(caller, trust);
        let r = pinned.dispatch("peek", json!({})).await;
        assert!(
            !r.is_error,
            "a task-boundary snapshot must retain alice's authorization: {}",
            r.content
        );
    }

    #[test]
    fn subset_none_inherits_all_some_empty_grants_none() {
        let r = registry(); // contains "echo"
        assert_eq!(r.subset(None).names(), vec!["echo".to_string()]);
        assert!(
            r.subset(Some(&[])).names().is_empty(),
            "an explicit empty allowlist (tools: []) must grant zero tools"
        );
        assert_eq!(
            r.subset(Some(&["echo".to_string()])).names(),
            vec!["echo".to_string()]
        );
        assert!(r.subset(Some(&["nope".to_string()])).names().is_empty());
    }

    /// A non-destructive tool with a Process effect (gated only by the policy floor).
    struct ProcTool;
    #[async_trait]
    impl Tool for ProcTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("proc", "run", json!({"type": "object"}))
                .with_effects(vec![Effect::Process])
                .with_access(vec![AccessKind::Process])
        }
        async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("ran"))
        }
    }

    #[tokio::test]
    async fn policy_requires_approval_forces_prompt_even_under_allow_rule() {
        use flux_policy::{Grant, SubjectKind, SubjectRef};
        // A grant that permits process.exec but marks it requires_approval (mirrors the default
        // local grant for process exec). The op is non-destructive, so only this flag should force
        // the prompt.
        let policy = AuthorizationPolicy {
            grants: vec![Grant {
                subjects: vec![SubjectRef {
                    kind: SubjectKind::User,
                    id: "*".into(),
                }],
                resources: vec![ResourceRef::any(ResourceKind::Process)],
                actions: vec![Action::from("process.exec")],
                required_trust: TrustLevel::Untrusted,
                required_scopes: Vec::new(),
                requires_approval: true,
            }],
        };
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Allow,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ProcTool));
        // A permissive allow-rule would normally skip the prompt entirely.
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["proc".into()], &[]),
            approver.clone(),
            test_ctx(),
        )
        .with_policy(policy);
        let r = ex.dispatch("proc", json!({})).await;
        assert!(!r.is_error, "approved → executes: {}", r.content);
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "a policy grant marked requires_approval must force a prompt despite the allow-rule"
        );
    }

    /// A write-effect tool that reports no path subjects (the unscoped-write case).
    struct UnscopedWriteTool;
    #[async_trait]
    impl Tool for UnscopedWriteTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("blindwrite", "write", json!({"type": "object"}))
                .with_effects(vec![Effect::Write, Effect::Filesystem])
                .with_access(vec![AccessKind::Filesystem])
        }
        async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("wrote"))
        }
    }

    #[tokio::test]
    async fn write_without_subjects_forces_approval() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Allow,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(UnscopedWriteTool));
        // A bare allow-rule would normally skip the prompt entirely.
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["blindwrite".into()], &[]),
            approver.clone(),
            test_ctx(),
        );
        let r = ex.dispatch("blindwrite", json!({})).await;
        assert!(!r.is_error);
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "a write tool reporting no path subjects must force an approval prompt"
        );
    }

    #[tokio::test]
    async fn hook_deny_short_circuits_before_policy_and_execution() {
        use std::sync::atomic::AtomicBool;

        struct DenyHook;
        impl PreToolHook for DenyHook {
            fn pre_tool(&self, _tool: &str, _input: &Value) -> HookOutcome {
                HookOutcome::Deny("blocked for test".into())
            }
        }
        static EXECUTED: AtomicBool = AtomicBool::new(false);
        struct FlagTool;
        #[async_trait]
        impl Tool for FlagTool {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only("flag", "flag", json!({"type": "object"}))
            }
            async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
                EXECUTED.store(true, Ordering::Relaxed);
                Ok(ToolResult::ok("ran"))
            }
        }

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(FlagTool));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["flag".into()], &[]),
            Arc::new(AllowApprover),
            test_ctx(),
        )
        .with_hooks(vec![Arc::new(DenyHook)]);
        let r = ex.dispatch("flag", json!({})).await;
        assert!(r.is_error);
        assert!(r.content.contains("blocked by hook"), "got: {}", r.content);
        assert!(
            !EXECUTED.load(Ordering::Relaxed),
            "a hook deny must short-circuit before the tool executes"
        );
    }

    #[test]
    fn observe_records_into_log() {
        let ex = Executor::new(
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            test_ctx(),
        );
        ex.observe(Observation::new(
            "toolchain",
            Phase::Startup,
            json!({"tools": ["read"]}),
        ));
        assert_eq!(ex.evidence().by_kind("toolchain").count(), 1);
    }

    #[tokio::test]
    async fn allow_always_persists_rule() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::AllowAlways("echo".into()),
        });
        let ex = Executor::new(registry(), PermissionManager::new(), approver, test_ctx());
        let _ = ex.dispatch("echo", json!({"text": "a"})).await;
        assert_eq!(ex.allow_rules(), vec!["echo".to_string()]);
    }

    /// A tool standing in for a grouped op (e.g. a git op) in surfacing tests.
    struct GitishTool;
    #[async_trait]
    impl Tool for GitishTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("git_status", "git status", json!({"type": "object"}))
        }
        fn permission_subjects(&self, _p: &Value) -> Vec<String> {
            Vec::new()
        }
        async fn execute(&self, _ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("clean"))
        }
    }

    fn git_group() -> Vec<flux_evidence::ToolGroup> {
        vec![flux_evidence::ToolGroup {
            name: "git".into(),
            tools: vec!["git_status".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: "project.signal".into(),
                signal: Some("git_repo".into()),
            }],
            ..Default::default()
        }]
    }

    #[test]
    fn advertised_op_names_gates_grouped_ops() {
        let specs = vec![
            ToolSpec::read_only("read", "read", json!({"type": "object"})),
            ToolSpec::read_only("git_status", "git status", json!({"type": "object"})),
        ];
        // Inactive group → only the core op is advertised.
        let none = advertised_op_names(&specs, &git_group(), &HashSet::new());
        assert!(none.contains("read") && !none.contains("git_status"));
        // Active group → both.
        let active: HashSet<String> = ["git".to_string()].into_iter().collect();
        let both = advertised_op_names(&specs, &git_group(), &active);
        assert!(both.contains("read") && both.contains("git_status"));
        // Empty manifest, no group-tagged specs → everything (no gating).
        let all_set = advertised_op_names(&specs, &[], &HashSet::new());
        assert!(all_set.contains("read") && all_set.contains("git_status"));
    }

    #[test]
    fn spec_group_tag_is_honored_without_a_manifest_tools_list() {
        // A spec tagged via ToolSpec::with_group (the committed field) is gated even when the manifest
        // group lists no `tools` (membership falls back to the spec's own tag).
        let tagged =
            ToolSpec::read_only("git_status", "s", json!({"type": "object"})).with_group("git");
        let group = vec![flux_evidence::ToolGroup {
            name: "git".into(),
            surface_when: vec![flux_evidence::SignalMatch {
                kind: "project.signal".into(),
                signal: Some("git_repo".into()),
            }],
            ..Default::default()
        }];
        assert!(!is_advertised(&tagged, &group, &HashSet::new()));
        let active: HashSet<String> = ["git".to_string()].into_iter().collect();
        assert!(is_advertised(&tagged, &group, &active));
    }

    #[test]
    fn active_specs_filters_by_group() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(GitishTool));
        // Group inactive → git op hidden, core op kept.
        let hidden = reg.active_specs(&git_group(), &HashSet::new());
        let names: Vec<&str> = hidden.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"echo") && !names.contains(&"git_status"));
        // Group active → all specs (== specs()).
        let active: HashSet<String> = ["git".to_string()].into_iter().collect();
        assert_eq!(
            reg.active_specs(&git_group(), &active).len(),
            reg.specs().len()
        );
    }

    /// C-162: an exact name and a `family.*` glob both resolve to concrete registered op names, and
    /// a pattern matching nothing is reported back rather than silently doing nothing.
    #[test]
    fn resolve_disabled_matches_exact_names_and_family_globs_and_reports_unmatched() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool)); // "echo"
        reg.register(Arc::new(GitishTool)); // "git_status"

        let resolved = reg.resolve_disabled(&[
            "echo".to_string(),
            "git_*".to_string(), // no op named exactly this, and no "." so it's an exact-name miss
            "no-such-op".to_string(),
        ]);
        assert_eq!(
            resolved.disabled,
            HashSet::from(["echo".to_string()]),
            "an exact match resolves; `git_*` is not a `family.*` glob (no dot) so it's a plain miss"
        );
        assert_eq!(
            resolved.unmatched,
            vec!["git_*".to_string(), "no-such-op".to_string()]
        );
    }

    /// A `family.*` glob resolves to every registered op under that dotted family.
    #[test]
    fn resolve_disabled_family_glob_matches_every_op_in_the_family() {
        struct DottedTool(&'static str);
        #[async_trait]
        impl Tool for DottedTool {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only(self.0, "d", json!({"type": "object"}))
            }
            fn permission_subjects(&self, _p: &Value) -> Vec<String> {
                Vec::new()
            }
            async fn execute(&self, _ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
                Ok(ToolResult::ok(""))
            }
        }
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(DottedTool("browser.navigate")));
        reg.register(Arc::new(DottedTool("browser.click")));
        reg.register(Arc::new(EchoTool)); // unrelated, must survive

        let resolved = reg.resolve_disabled(&["browser.*".to_string()]);
        assert_eq!(
            resolved.disabled,
            HashSet::from(["browser.navigate".to_string(), "browser.click".to_string()])
        );
        assert!(resolved.unmatched.is_empty());
    }

    #[test]
    fn trim_tool_output_caps_and_annotates() {
        // Under cap → unchanged.
        assert_eq!(trim_tool_output("hello".into(), 100, "bash"), "hello");
        // cap 0 → disabled.
        let big = "x".repeat(50);
        assert_eq!(trim_tool_output(big.clone(), 0, "bash"), big);
        // Over cap → truncated + notice.
        let out = trim_tool_output("x".repeat(50), 10, "bash");
        assert!(out.starts_with(&"x".repeat(10)));
        assert!(out.contains("truncated") && out.contains("40 of 50"));
    }

    #[test]
    fn detect_signals_finds_markers_walking_up() {
        let base = std::env::temp_dir().join(format!("flux-detect-{}", std::process::id()));
        let sub = base.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(base.join(".git")).unwrap();
        std::fs::write(base.join("go.mod"), "module x\n").unwrap();
        let sigs = detect_signals(&sub);
        let has = |s: &str| {
            sigs.iter()
                .any(|o| o.data.get("signal").and_then(|v| v.as_str()) == Some(s))
        };
        // Found from a nested subdirectory (walk-up).
        assert!(has("git_repo") && has("go"));
        assert!(!has("python"));
        std::fs::remove_dir_all(&base).ok();
    }

    /// D-187: the `agent_triggerable` signal fires only when a discovered command or skill opts in
    /// via `agent-triggerable: true` — an ordinary command/skill (flag absent/false) leaves the
    /// signal off, so `command.invoke`'s owning group stays hidden for ordinary turns.
    #[test]
    fn detect_signals_surfaces_agent_triggerable_only_when_a_target_opts_in() {
        let _home_guard = crate::metadata::HOME_LOCK.lock().unwrap();
        let base = std::env::temp_dir().join(format!(
            "flux-detect-agent-triggerable-{}",
            std::process::id()
        ));
        let home = std::env::temp_dir().join(format!(
            "flux-detect-agent-triggerable-home-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(base.join(".flux/commands")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            base.join(".flux/commands/human-only.md"),
            "---\ndescription: human only\n---\nbody",
        )
        .unwrap();

        std::env::set_var("HOME", &home);
        let has_signal = |cwd: &std::path::Path| {
            detect_signals(cwd)
                .iter()
                .any(|o| o.data.get("signal").and_then(|v| v.as_str()) == Some("agent_triggerable"))
        };
        assert!(
            !has_signal(&base),
            "a command without agent-triggerable: true must not surface the signal"
        );

        std::fs::write(
            base.join(".flux/commands/triggerable.md"),
            "---\ndescription: agent ok\nagent-triggerable: true\n---\nbody",
        )
        .unwrap();
        assert!(
            has_signal(&base),
            "a discovered agent-triggerable command must surface the signal"
        );
        std::env::remove_var("HOME");

        std::fs::remove_dir_all(&base).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    // --- D-177: the authorize-only split ------------------------------------

    /// A tool that records every `execute` — the counter that must NEVER move for an authorize-only
    /// decision, no matter how many times it is asked.
    struct SideEffectTool(Arc<AtomicU64>);

    #[async_trait]
    impl Tool for SideEffectTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("boom", "has a side effect", json!({"type": "object"}))
        }
        async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult::ok("fired"))
        }
    }

    struct DenyApprover;

    #[async_trait]
    impl Approver for DenyApprover {
        async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
            ApprovalChoice::Deny
        }
    }

    fn authorize_executor(
        fired: Arc<AtomicU64>,
        allow: &[String],
        deny: &[String],
        approver: Arc<dyn Approver>,
    ) -> Executor {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(SideEffectTool(fired)));
        Executor::new(
            registry,
            PermissionManager::from_rules(allow, deny),
            approver,
            test_ctx(),
        )
    }

    /// **Adversarial**: `authorize` is a decision, not a dispatch. Asking it repeatedly — including
    /// for a call it ALLOWS — never runs the op, never records an audit observation, and never
    /// mutates the permission rules. (It is also structurally impossible for it to execute: it is a
    /// synchronous fn, and `Tool::execute`/`Approver::request` are both `async`.)
    #[tokio::test]
    async fn authorize_decides_without_any_execution_side_effect() {
        let fired = Arc::new(AtomicU64::new(0));
        let executor =
            authorize_executor(fired.clone(), &["boom".into()], &[], Arc::new(DenyApprover));

        for _ in 0..5 {
            assert_eq!(
                executor.authorize("boom", &json!({})),
                AuthorizeVerdict::Allow
            );
        }

        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "an authorization DECISION must never run the op"
        );
        assert!(
            executor.evidence().all().is_empty(),
            "a hypothetical call must not write to the audit log: {:?}",
            executor.evidence().all()
        );
    }

    /// **Adversarial**: an `Allow` verdict opens no bypass. The same call, actually dispatched, still
    /// goes through the whole envelope — here the approver's `Deny` refuses it, and the op never runs.
    #[tokio::test]
    async fn an_allow_verdict_is_not_a_bypass_of_the_real_envelope() {
        let fired = Arc::new(AtomicU64::new(0));
        // "ask" (no allow rule) + a policy that needs no approval ⇒ authorize reports the gate...
        let executor = authorize_executor(fired.clone(), &[], &[], Arc::new(DenyApprover));
        assert_eq!(
            executor.authorize("boom", &json!({})),
            AuthorizeVerdict::ApprovalRequired,
            "the rules only ask for this op — the envelope would prompt"
        );

        // ...and the real dispatch still asks, and is still refused.
        let outcome = executor.dispatch_outcome("boom", json!({})).await;
        assert!(outcome.denied, "the approver's Deny must still refuse");
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "a denied dispatch never reaches the op"
        );
    }

    /// `authorize` and the live envelope agree on WHY, not just whether: a permission-rule deny
    /// reports the same refusal message through both surfaces (they share one implementation).
    #[tokio::test]
    async fn authorize_and_dispatch_report_the_same_refusal() {
        let fired = Arc::new(AtomicU64::new(0));
        let executor = authorize_executor(
            fired.clone(),
            &[],
            &["boom".into()],
            Arc::new(DelayedAllowApprover),
        );

        let verdict = executor.authorize("boom", &json!({}));
        assert!(verdict.is_denied(), "{verdict:?}");

        let outcome = executor.dispatch_outcome("boom", json!({})).await;
        assert!(outcome.denied);
        assert_eq!(
            verdict.reason().unwrap(),
            outcome.result.content,
            "the two surfaces must not drift apart on the refusal wording"
        );
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }

    /// An unknown op is refused by both surfaces rather than silently admitted — and (D-184) the two
    /// surfaces now agree on the CLASSIFICATION too, not just the outcome: `authorize` has always
    /// called an unknown tool `Deny`, but `dispatch_outcome` used to report the identical refusal
    /// with `denied: false` (the same shape a transient tool-side failure gets), so
    /// `flux_lang::runtime::call_failure` wrapped a typo'd op name as a *retryable* `FlowError::Runtime`
    /// instead of the fatal `FlowError::Denied` — burning `retry`/`loop` attempts on a call that could
    /// never succeed. Both surfaces must classify it as denied.
    #[tokio::test]
    async fn authorize_denies_an_unknown_op() {
        let executor = authorize_executor(
            Arc::new(AtomicU64::new(0)),
            &["boom".into()],
            &[],
            Arc::new(DenyApprover),
        );
        let verdict = executor.authorize("no-such-op", &json!({}));
        assert!(verdict.is_denied(), "{verdict:?}");

        let outcome = executor.dispatch_outcome("no-such-op", json!({})).await;
        assert!(
            outcome.denied,
            "dispatch_outcome must classify an unknown tool as denied, exactly like authorize: {:?}",
            outcome.result
        );
        assert_eq!(
            verdict.reason().unwrap(),
            outcome.result.content,
            "the two surfaces must not drift apart on the refusal wording"
        );
    }

    /// C-162 defense-in-depth: an op named in `[tools] disable` is refused at dispatch even though
    /// it stays fully registered and the permission rules explicitly allow it — proving the refusal
    /// is a distinct gate, not merely "unknown tool" or "denied by rules". This is what protects a
    /// cached plan or a resumed session from calling an op the workspace has configured off.
    #[tokio::test]
    async fn disabled_op_is_refused_at_dispatch_even_though_still_registered_and_allowed() {
        let fired = Arc::new(AtomicU64::new(0));
        let executor =
            authorize_executor(fired.clone(), &["boom".into()], &[], Arc::new(DenyApprover))
                .with_disabled_ops(["boom".to_string()].into_iter().collect());

        let verdict = executor.authorize("boom", &json!({}));
        assert!(verdict.is_denied(), "{verdict:?}");
        assert!(
            verdict.reason().unwrap().contains("disabled by config"),
            "{verdict:?}"
        );

        let outcome = executor.dispatch_outcome("boom", json!({})).await;
        assert!(
            outcome.denied,
            "a config-disabled op must be refused at dispatch too (defense in depth)"
        );
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "the op must never actually run"
        );
        assert_eq!(
            verdict.reason().unwrap(),
            outcome.result.content,
            "authorize and dispatch must agree on the refusal wording"
        );
    }

    /// C-183: [`ExecutionEnvironment::with_disabled_ops`] must survive `into_executor()` — this is
    /// the seam a surface that derives several executors from ONE cloned environment template
    /// (`flux-app`'s per-journey and per-agent-target executors) relies on to install the identically
    /// resolved set everywhere without re-deriving it per executor.
    #[tokio::test]
    async fn execution_environment_carries_disabled_ops_into_executor() {
        let fired = Arc::new(AtomicU64::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(SideEffectTool(fired.clone())));
        let executor = ExecutionEnvironment::new(
            test_ctx().system(),
            registry,
            PermissionManager::from_rules(&["boom".to_string()], &[]),
            Arc::new(DenyApprover),
            ExecutionAuthorization::local(),
        )
        .with_disabled_ops(["boom".to_string()].into_iter().collect())
        .into_executor();

        assert!(executor.disabled_ops().contains("boom"));
        let outcome = executor.dispatch_outcome("boom", json!({})).await;
        assert!(
            outcome.denied,
            "an environment-level disabled set must still refuse dispatch once built into an executor"
        );
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "the op must never actually run"
        );
    }

    #[test]
    fn kubeconfig_present_detects_env() {
        // `KUBECONFIG` set (non-empty) → kubeconfig is reachable. We can't safely assert the negative
        // (the host running the test may have ~/.kube/config), so only assert the positive env case.
        let dir = std::env::temp_dir().join(format!("flux-kube-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config");
        std::fs::write(&cfg, "apiVersion: v1\n").unwrap();
        let prev = std::env::var_os("KUBECONFIG");
        std::env::set_var("KUBECONFIG", &cfg);
        assert!(kubeconfig_present());
        let sigs = detect_signals(&dir);
        assert!(sigs
            .iter()
            .any(|o| o.data.get("signal").and_then(|v| v.as_str()) == Some("kubernetes")));
        match prev {
            Some(v) => std::env::set_var("KUBECONFIG", v),
            None => std::env::remove_var("KUBECONFIG"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
