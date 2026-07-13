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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use flux_core::{OperationTiming, Result};
use flux_evidence::{
    DestructiveEscalation, EvidenceLog, Observation, Phase, Reaction, KIND_DESTRUCTIVE,
};
use flux_policy::{
    evaluate, Action, AuthorizationPolicy, Caller, CallerKind, Decision, Principal,
    Request as PolicyRequest, ResourceKind, ResourceRef, Trust, TrustKind, TrustLevel,
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

/// One sub-agent spawn, fully described. `cap_scope` is the caller's active `with_tools`
/// allowlist, if any — the spawner intersects it into the role's own `tools`, so a `task` invoked
/// from inside a capability scope can never hand the child a broader tool set than the block that
/// spawned it (capabilities only narrow on descent). `parent_session`, when known, is recorded as
/// the child session's `correlation_id` so a shared audit store correlates child streams to the
/// turn that spawned them (A-08).
#[derive(Debug, Clone, Default)]
pub struct SpawnRequest {
    pub role: String,
    pub task: String,
    pub cap_scope: Option<Vec<String>>,
    pub parent_session: Option<String>,
}

impl SpawnRequest {
    /// A bare request: no capability scope, no parent correlation.
    pub fn new(role: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            task: task.into(),
            cap_scope: None,
            parent_session: None,
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

/// Host capabilities used by model-backed stages inside an authored Flux-Lang outer loop. Defined
/// here (L2) so guarded tools can delegate without depending on the L3 engine. Models return typed
/// stage values and provider-native calls; only caller-authored Flux reaches deterministic execution.
#[async_trait]
pub trait LoopHost: Send + Sync {
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

/// What a tool is given at execution time: the guarded IO surface, the secret redactor, an optional
/// sub-agent spawner, and the per-session read-set (file → mtime at last read) used by the
/// read-before-write guard. The read-set is shared (an `Arc<Mutex<…>>`) so every op in a session sees
/// the same map: a `read` in one node records an mtime an `edit` in a later node checks against.
#[derive(Clone)]
pub struct ToolContext {
    pub system: Arc<System>,
    pub redactor: Redactor,
    pub spawner: Option<Arc<dyn Spawner>>,
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
    /// The turn's cancellation token, installed per turn by the engine (interior-mutable so the
    /// shared, long-lived context can carry a fresh token each turn — same lifecycle, and the same
    /// **one-active-turn-per-engine** assumption, as `loop_host`'s per-turn `set_turn`). A spawning
    /// tool (`task`) threads a child of this token into its sub-agent so cancelling the parent turn
    /// cancels the child; `None` (no cancellable driver, e.g. the one-shot SDK path) means the
    /// sub-agent simply runs to completion. INVARIANT: a single engine must not drive two turns
    /// concurrently — running concurrent turns would clobber this slot (and `loop_host`'s). Surfaces
    /// that fan out concurrently (e.g. a server) must use one engine per concurrent turn; the SDK's
    /// `FlowClient` is already safe (a fresh `ToolContext` per `execute`).
    cancel: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>>,
    /// The current turn's session id, installed per turn by the engine (same interior-mutable,
    /// one-active-turn-per-engine lifecycle as `cancel`). A spawning tool (`task`) reads it to
    /// correlate the child's audit stream to the parent turn (A-08).
    session: Arc<Mutex<Option<String>>>,
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
        Self {
            system,
            redactor: Redactor::new(),
            spawner: None,
            loop_host: None,
            composite_registrar: None,
            read_times: Arc::new(Mutex::new(HashMap::new())),
            evidence: Arc::new(Mutex::new(EvidenceLog::new())),
            cancel: Arc::new(Mutex::new(None)),
            session: Arc::new(Mutex::new(None)),
            cap_scopes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The effective tool-name allowlist of the innermost active capability scope, if any. `None`
    /// means no scope is active (every tool stays subject only to policy/permission rules). Used by
    /// [`Executor::dispatch`]'s gate and by [`Spawner`] implementations to intersect a sub-agent role's
    /// tools with the block it was invoked from.
    pub fn active_cap_scope(&self) -> Option<Vec<String>> {
        self.cap_scopes.lock().unwrap().last().cloned()
    }

    /// Install the turn's cancellation token (the engine calls this per turn before running the loop).
    /// Interior-mutable so a cloned, shared context picks it up.
    pub fn set_cancel(&self, token: tokio_util::sync::CancellationToken) {
        *self.cancel.lock().unwrap() = Some(token);
    }

    /// The turn's cancellation token, if a cancellable driver installed one.
    pub fn cancel_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        self.cancel.lock().unwrap().clone()
    }

    /// Install the turn's session id (the engine calls this per turn, like [`set_cancel`]
    /// (Self::set_cancel)). A spawning tool (`task`) reads it back to correlate the child's audit
    /// stream to the parent turn (A-08). Same one-active-turn-per-engine lifecycle as `cancel`.
    pub fn set_session(&self, session_id: impl Into<String>) {
        *self.session.lock().unwrap() = Some(session_id.into());
    }

    /// The current turn's session id, if a driver installed one.
    pub fn session_id(&self) -> Option<String> {
        self.session.lock().unwrap().clone()
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

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult>;
}

/// A registry of tools keyed by name.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Remove a tool by name, returning it if present. Used to scope a sub-agent's registry (e.g.
    /// drop `task` so a sub-agent can't spawn further sub-agents).
    pub fn remove(&mut self, name: &str) -> Option<Arc<dyn Tool>> {
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
        let tools = self
            .tools
            .iter()
            .filter(|(k, _)| names.iter().any(|n| n == *k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        ToolRegistry { tools }
    }

    /// Every registered tool name, sorted (see [`specs`](Self::specs) for why order must be stable).
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
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
    out
}

/// Whether a kubeconfig is reachable: `KUBECONFIG` is set (non-empty) OR `~/.kube/config` exists. This
/// is ambient (host environment / home dir), independent of `cwd` — kubectl finds its config this way.
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

    /// Approve a whole compiled plan as one unit (the "approve the graph, not each node" path). The
    /// plan itself has already been rendered for the user (the `flow.plan` observation); this is just
    /// the single confirm. `AllowAlways` here means "trust every plan for the rest of the session".
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

/// The resolved `(Caller, Trust)` the policy floor evaluates against, behind a shared handle.
///
/// One cell can back an [`Executor`] *and* the sub-agent spawner, so a per-request surface that
/// swaps the identity between turns (D-69: flux-server's principal mode) changes it for the whole
/// tree at once — a child agent must never keep executing under the service identity after the
/// surface resolved a request principal. Contract: [`set`](Self::set) is called only between turns,
/// under the surface's turn serialization (e.g. the server's turn gate); mid-turn swaps would race
/// the dispatch reads.
#[derive(Clone)]
pub struct IdentityCell(Arc<Mutex<(Caller, Trust)>>);

impl IdentityCell {
    pub fn new(caller: Caller, trust: Trust) -> Self {
        Self(Arc::new(Mutex::new((caller, trust))))
    }

    /// The local single-user identity (the default when a surface never resolves one).
    pub fn local() -> Self {
        Self::new(default_local_caller(), default_local_trust())
    }

    /// Snapshot the current identity (cloned — dispatch holds no lock across evaluation).
    pub fn get(&self) -> (Caller, Trust) {
        self.0.lock().unwrap().clone()
    }

    /// Swap the identity. Per-request surfaces call this between turns, under turn serialization.
    pub fn set(&self, caller: Caller, trust: Trust) {
        *self.0.lock().unwrap() = (caller, trust);
    }
}

/// A local single-user caller used when no identity is supplied (matches `flux-auth`'s
/// `LocalIdentity`, duplicated here so the runtime needn't depend on the auth layer).
fn default_local_caller() -> Caller {
    Caller {
        principal: Principal {
            id: "local".into(),
            name: "local".into(),
            kind: CallerKind::User,
        },
        groups: Vec::new(),
        source: "local".into(),
    }
}

fn default_local_trust() -> Trust {
    Trust {
        kind: TrustKind::Invocation,
        level: TrustLevel::Privileged,
        scopes: Vec::new(),
    }
}

/// Translate a tool's declared effects + permission subjects into the (action, resource) pairs the
/// authorization policy is evaluated against. Filesystem read/write map onto path resources (one
/// per subject); process/network/browser map onto a kind-wide resource (their subjects are gated
/// by the coder-style permission rules, not the policy).
fn effect_requests(spec: &ToolSpec, subjects: &[String]) -> Vec<(Action, ResourceRef)> {
    let mut reqs = Vec::new();
    let has = |e: Effect| spec.effects.contains(&e);
    let path_resources = || -> Vec<ResourceRef> {
        if subjects.is_empty() {
            vec![ResourceRef::path("")] // matches a `*` path glob
        } else {
            subjects
                .iter()
                .map(|s| ResourceRef::path(s.as_str()))
                .collect()
        }
    };
    if has(Effect::Write) {
        for r in path_resources() {
            reqs.push((Action::from("workspace.write"), r));
        }
    } else if has(Effect::Read) || has(Effect::Filesystem) {
        for r in path_resources() {
            reqs.push((Action::from("workspace.read"), r));
        }
    }
    if has(Effect::Process) || has(Effect::LocalSystem) {
        reqs.push((
            Action::from("process.exec"),
            ResourceRef::any(ResourceKind::Process),
        ));
    }
    if has(Effect::Network) {
        reqs.push((
            Action::from("network.fetch"),
            ResourceRef::any(ResourceKind::Network),
        ));
    }
    if has(Effect::Browser) {
        // ResourceKind has no Browser variant; browser navigation is gated as network egress.
        reqs.push((
            Action::from("browser.navigate"),
            ResourceRef::any(ResourceKind::Network),
        ));
    }
    reqs
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
    /// The authorization floor. `None` disables the policy layer (permission rules only).
    policy: Option<AuthorizationPolicy>,
    /// The resolved identity the policy evaluates against — a shared cell (see [`IdentityCell`])
    /// so per-request surfaces can swap it between turns and spawners can inherit the live value.
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
    /// `true` iff the envelope itself refused to run the op: a capability-scope miss, the
    /// authorization policy floor, a permission-rule deny, or the approver declining. A pre-tool
    /// hook's `Deny` is deliberately excluded — hook denials are meant to stay retryable/repairable
    /// rather than a terminal authorization refusal, exactly as before this flag existed (hook
    /// denials never matched the old prefix heuristic either, since their wording is `` `{op}`
    /// blocked by hook `` , not `` `{op}` denied by `` ).
    pub denied: bool,
    /// Monotonic phase attribution measured inside the safety envelope.
    pub timing: OperationTiming,
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

    pub fn new(
        registry: ToolRegistry,
        perms: PermissionManager,
        approver: Arc<dyn Approver>,
        ctx: ToolContext,
    ) -> Self {
        Self {
            registry,
            perms: Mutex::new(perms),
            approver: Mutex::new(approver),
            ctx,
            hooks: Vec::new(),
            policy: None,
            identity: IdentityCell::local(),
            plan_scope: AtomicU32::new(0),
            destructive_scope: Mutex::new(Vec::new()),
            trust_all: AtomicBool::new(false),
            op_cache: Mutex::new(HashMap::new()),
            cache_gen: AtomicU64::new(0),
            dispatch_seq: AtomicU64::new(1),
            cache_enabled: std::env::var("FLUX_OP_CACHE")
                .map(|v| v != "off" && v != "0")
                .unwrap_or(true),
        }
    }

    /// Enable/disable the deterministic read-only op cache (overrides `FLUX_OP_CACHE`).
    pub fn with_op_cache(mut self, on: bool) -> Self {
        self.cache_enabled = on;
        self
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
    /// risk preview — the plan tree itself was already rendered (the `flow.plan` observation). The
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
            ApprovalChoice::Deny => None,
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
            ApprovalChoice::Deny => false,
        }
    }

    /// Stable snapshot of the authority context an approval was made under. It contains no secret
    /// values: only caller/trust/policy metadata, allow rules, and the active capability ceiling.
    /// Receipt owners bind this byte string at approval and require an exact match at execution;
    /// dispatch still re-evaluates every policy and permission rule afterward.
    pub fn approval_context(&self) -> String {
        let (caller, trust) = self.identity.get();
        serde_json::to_string(&json!({
            "caller": caller,
            "trust": trust,
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

    /// Enable the authorization-policy floor: every tool call's effects are evaluated against
    /// `policy` (default-deny) before the permission rules run.
    pub fn with_policy(mut self, policy: AuthorizationPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Set the resolved caller + trust the policy evaluates against (default: the local
    /// single-user identity). Surfaces resolve this via `flux-auth` before constructing the agent.
    /// Replaces the identity cell with a fresh, unshared one — to share a cell with a spawner,
    /// use [`with_identity_cell`](Self::with_identity_cell).
    pub fn with_identity(mut self, caller: Caller, trust: Trust) -> Self {
        self.identity = IdentityCell::new(caller, trust);
        self
    }

    /// Share an externally-owned identity cell (the surface keeps a handle and may swap the
    /// identity between turns; the sub-agent spawner may hold the same cell so children inherit
    /// the live value). See [`IdentityCell`] for the turn-serialization contract.
    pub fn with_identity_cell(mut self, cell: IdentityCell) -> Self {
        self.identity = cell;
        self
    }

    /// The shared identity handle (for surfaces that need to swap identity per request and for
    /// wiring the same cell into spawners after construction).
    pub fn identity(&self) -> IdentityCell {
        self.identity.clone()
    }

    /// Swap the identity on the shared cell — a per-request surface calls this between turns,
    /// under its turn serialization (see [`IdentityCell::set`]).
    pub fn set_identity(&self, caller: Caller, trust: Trust) {
        self.identity.set(caller, trust);
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

    /// Like [`dispatch`](Self::dispatch), but also reports — structurally, not by inference —
    /// whether the envelope itself denied the call. See [`DispatchOutcome`].
    pub async fn dispatch_outcome(&self, name: &str, params: Value) -> DispatchOutcome {
        let started = Instant::now();
        let dispatch = self.dispatch_seq.fetch_add(1, Ordering::Relaxed);
        let mut approval_wait = None;
        let Some(tool) = self.registry.get(name) else {
            return self.finish_dispatch(
                name,
                started,
                approval_wait,
                None,
                ToolResult::error(format!("unknown tool: {name}")),
                false,
            );
        };

        // 0. Capability-scope floor — checked FIRST, before pre-tool hooks or the policy/permission
        //    layers below, and on EVERY dispatch (there is no other path to a tool's `execute`), so a
        //    composite op, a sub-agent's inner call, or any nested reentry that eventually calls
        //    `dispatch` again is caught exactly like a direct call. An empty stack (no `with_tools`
        //    scope active) is a strict no-op — every existing flow that never opens a scope is
        //    unaffected. A denial here can never be a false negative: the top of stack is always the
        //    *narrowed* effective set (see `push_cap_scope`), so this can only ever be as strict as, or
        //    stricter than, the outer session policy — never looser.
        if let Some(scope) = self.active_cap_scope() {
            if !scope.iter().any(|t| t == name) {
                self.ctx.evidence.lock().unwrap().record(Observation::new(
                    "cap_scope_denied",
                    Phase::Turn,
                    json!({ "tool": name, "scope": scope }),
                ));
                return self.finish_dispatch(
                    name,
                    started,
                    approval_wait,
                    None,
                    ToolResult::error(format!(
                        "`{name}` denied by capability scope (not in the active with_tools allowlist)"
                    )),
                    true,
                );
            }
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

        let spec = tool.spec();
        let subjects = tool.permission_subjects(&params);
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
                match self.ctx.system.path_identity(&subject, access) {
                    Ok(subject) => physical.push(subject),
                    Err(err) => {
                        return self.finish_dispatch(
                            name,
                            started,
                            approval_wait,
                            None,
                            ToolResult::error(format!(
                                "`{name}` denied by filesystem path guard: {err}"
                            )),
                            true,
                        );
                    }
                }
            }
            physical
        } else {
            subjects
        };
        let intents = tool.intents(&params);

        // 1. Authorization-policy floor (if configured): default-deny on any ungranted effect. A
        //    `Deny` short-circuits; an `ApprovalRequired` (e.g. a grant marked `requires_approval`,
        //    like the default `process.exec`) forces the approval gate below even if a permissive
        //    allow-rule would otherwise satisfy it — the policy is the floor, rules can't widen it.
        let mut policy_requires_approval = false;
        if let Some(policy) = &self.policy {
            // Snapshot once per dispatch: the cell may be swapped between turns (never mid-turn,
            // per the IdentityCell contract), and no lock is held across evaluation.
            let (caller, trust) = self.identity.get();
            for (action, resource) in effect_requests(&spec, &subjects) {
                let req = PolicyRequest {
                    caller: &caller,
                    trust: &trust,
                    action: &action,
                    resource: &resource,
                };
                match evaluate(policy, &req).decision {
                    Decision::Deny => {
                        return self.finish_dispatch(
                            name,
                            started,
                            approval_wait,
                            None,
                            ToolResult::error(format!(
                                "`{name}` denied by policy ({} on {:?})",
                                action.0, resource.kind
                            )),
                            true,
                        );
                    }
                    Decision::ApprovalRequired => policy_requires_approval = true,
                    Decision::Allow => {}
                }
            }
        }

        // 2. Permission rules (coder-style): deny wins; otherwise allow/ask for tool + subjects.
        let perm = self.perms.lock().unwrap().check(name, &subjects);
        if perm == PermDecision::Deny {
            return self.finish_dispatch(
                name,
                started,
                approval_wait,
                None,
                ToolResult::error(format!("`{name}` denied by permission rules")),
                true,
            );
        }

        // 3. Evidence + reactions: record this call (and a destructive marker when matched), then
        //    let the built-in escalation reaction decide whether approval must be forced.
        let mut observations = vec![Observation::new(
            "tool_call",
            Phase::Turn,
            json!({ "tool": name, "subjects": subjects }),
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
                ApprovalChoice::Deny => {
                    self.record_dispatch_event(
                        "approval.denied",
                        dispatch,
                        name,
                        started,
                        json!({}),
                    );
                    return self.finish_dispatch(
                        name,
                        started,
                        approval_wait,
                        None,
                        ToolResult::error(format!("`{name}` denied by user")),
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
        let result = match tool.execute(&self.ctx, params).await {
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
    use flux_system::Workspace;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

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
            Ok(ToolResult::ok(ctx.system.read_file(path).await?))
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
                .with_effects(vec![Effect::Write]),
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

    /// A read-effect tool gated only by the policy floor (permissive rules, auto-approve).
    struct ReadishTool;
    #[async_trait]
    impl Tool for ReadishTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("peek", "read", json!({"type": "object"}))
                .with_effects(vec![Effect::Read])
        }
        async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("read"))
        }
    }

    /// D-69 invariant: on a SHARED executor, `set_identity` swaps the policy subject between
    /// turns — a deny for caller B is not bypassed because caller A ran first (and A's grant is
    /// not sticky once B's identity is set). This is the per-request server mode's envelope
    /// guarantee, proven at the layer that enforces it.
    #[tokio::test]
    async fn set_identity_swaps_the_policy_subject_between_turns() {
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
        .with_policy(alice_only)
        .with_identity(caller, trust);

        let r = ex.dispatch("peek", json!({})).await;
        assert!(
            r.is_error && r.content.contains("denied by policy"),
            "bob is outside the grant set: {}",
            r.content
        );

        let (caller, trust) = ident("alice");
        ex.set_identity(caller, trust);
        let r = ex.dispatch("peek", json!({})).await;
        assert!(!r.is_error, "alice is granted reads: {}", r.content);

        let (caller, trust) = ident("bob");
        ex.set_identity(caller, trust);
        let r = ex.dispatch("peek", json!({})).await;
        assert!(
            r.is_error,
            "alice's grant must not stick to bob's turn: {}",
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
                .with_effects(vec![Effect::Write])
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
