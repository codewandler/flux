//! The [`App`]: the runtime host that turns a parsed [`Program`] into a running multi-agent system.
//!
//! An app owns three things — the program (agents/channels/triggers/journeys), a [`ToolRegistry`]
//! assembled from the builtins + the orchestration op-pack (+ cognition ops when a provider is wired),
//! and the in-process [`Bus`]. The [`Engine`] behind it is the worker the public surface delegates to
//! (and that the `spawn` op re-enters); it is held in an `Arc` so the orchestration ops can hold a
//! `Weak` back-reference without a cycle.
//!
//! A journey is executed by **reusing flux-flow's engine path**: a real [`Executor`] (the full
//! permission + approval envelope) drives `flux_flow::runtime::execute_flow` over the journey's
//! `DraftAst`, with a [`FlowStore`] for state and an [`AgentSink`] for output. Nothing about the
//! interpreter is reinvented here — the multi-agent layer is pure wiring over the existing engine.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use flux_agent::{AgentSpec, Permissions};
use flux_core::{Error, Result};
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_flow::registry::analyze_composites;
use flux_flow::state::FlowStore;
use flux_flow::AgentSink;
use flux_lang::ast::{SymbolName, Value as FluxValue, Visibility};
use flux_lang::program::{AgentDecl, Program};
use flux_provider::Provider;
use flux_runtime::{
    AllowApprover, Approver, DenyApprover, Executor, PermissionManager, Tool, ToolContext,
    ToolRegistry,
};
use flux_system::{System, Workspace};

use crate::bus::{Bus, Event};
use crate::ops::{self, JourneyHost};

/// How deep `spawn`-within-`spawn` may recurse before the engine refuses (cheap guard against a
/// journey that spawns itself unboundedly).
const MAX_SPAWN_DEPTH: u32 = 16;

/// The most events a single [`App::deliver`] will process across the initial event and any it cascades
/// into (via `emit`). Bounds an `emit`-loop in the one-shot path; the long-running [`App::run`] loop is
/// unbounded by design.
const MAX_CASCADE: u32 = 256;

/// The result of running one journey: which journey, its textual result (the flow's `return`/last view),
/// and how many ops it dispatched.
#[derive(Debug, Clone)]
pub struct JourneyRun {
    pub journey: String,
    pub result: String,
    pub steps: usize,
}

/// The runtime host for a multi-agent [`Program`]. Cheap to clone is *not* a goal — hold one `App` and
/// drive it; clone the [`Bus`] handle (via [`App::bus`]) if another task needs to emit.
pub struct App {
    engine: Arc<Engine>,
}

impl App {
    /// Build a host for `program`. When `provider` is `Some`, the model-backed cognition ops
    /// (`ai.*`, `synth`) are registered too, so journeys may plan/extract/judge; with `None` the host
    /// is hermetic (pure ops only — no network, no model).
    pub fn new(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
    ) -> Self {
        Self::with_options(program, provider, model, false)
    }

    /// Build a host, choosing the approval posture. `auto_approve = false` (the safe default) **denies**
    /// any op outside the pre-allowed orchestration + read-only set; `true` (the CLI's `--yes`) runs
    /// allow-all for trusted, pre-authored programs.
    pub fn with_options(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
    ) -> Self {
        Self::with_tools(program, provider, model, auto_approve, Vec::new())
    }

    /// Like [`with_options`](Self::with_options) but also registers `extra_tools` into the host
    /// registry — the seam the CLI uses to give journeys **and** the agent target (`trigger.agent`) the
    /// knowledge datasource retrieval ops (D-07) and the integration plugin tools (D-08), assembled in
    /// the async CLI layer so flux-app stays free of those deps.
    pub fn with_tools(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        App {
            engine: Engine::new(program, provider, model.into(), auto_approve, extra_tools),
        }
    }

    /// The program this host runs.
    pub fn program(&self) -> &Program {
        &self.engine.program
    }

    /// The assembled op registry (builtins + orchestration + optional cognition).
    pub fn registry(&self) -> &ToolRegistry {
        &self.engine.registry
    }

    /// A handle to the event bus (clone it to emit from another task).
    pub fn bus(&self) -> &Bus {
        &self.engine.bus
    }

    /// Build (or fetch the cached) [`FlowEngine`] for a declared agent — the seam the `a2a` channel
    /// uses to serve a program agent over HTTP/A2A. The engine shares this app's `EventStore`, so a
    /// session opened over HTTP and one woken by an agent-bound trigger live in the same log. Errors if
    /// the agent is undeclared or has no model provider.
    pub fn agent_engine(&self, name: &str) -> Result<Arc<FlowEngine>> {
        self.engine.agent_engine(name)
    }

    /// Look up a declared agent by name (e.g. for its A2A card metadata).
    pub fn agent_decl(&self, name: &str) -> Option<&AgentDecl> {
        self.engine.program.agents.iter().find(|a| a.name == name)
    }

    /// The program's sole declared agent, if there is exactly one. The `a2a` channel and the `--serve`
    /// flag bind to this when no agent is named explicitly; an ambiguous (multi-agent) or agent-less
    /// program must name its target.
    pub fn sole_agent(&self) -> Option<&AgentDecl> {
        match self.engine.program.agents.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// Inject one event and run every journey its label triggers **to completion**, returning each
    /// run's result. Events the journeys `emit` are processed too (bounded by [`MAX_CASCADE`]). This is
    /// the unit of work tests and the CLI channels drive; [`App::run`] is the long-running form.
    pub async fn deliver(
        &self,
        label: impl Into<String>,
        payload: Value,
    ) -> Result<Vec<JourneyRun>> {
        self.engine.deliver(label.into(), payload).await
    }

    /// Run as a long-lived supervisor: emit a single `startup` event, then consume the bus forever,
    /// running each event's triggered journeys. Returns only when the bus closes. Cascaded `emit`s are
    /// picked up by the same loop, so this is the natural form for a server-style program.
    pub async fn run(&self) -> Result<()> {
        let mut rx = self.engine.bus.subscribe();
        // Fire startup *after* subscribing so a `{on:"startup"}` trigger is never missed.
        self.engine.bus.emit("startup", json!({}));
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let mut sink = RecordingSink::default();
                    self.engine
                        .run_triggers(&ev.label, &ev.payload, &mut sink)
                        .await?;
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
        Ok(())
    }

    /// Test-only: how many messages the agent's bound session for `conversation` holds (`0` if none).
    /// Used to assert per-thread agent memory (same conversation reuses one session).
    #[cfg(test)]
    pub(crate) fn agent_session_len(&self, agent: &str, conversation: &str) -> usize {
        let map = self.engine.sessions.lock().expect("sessions map poisoned");
        match map.get(&(agent.to_string(), conversation.to_string())) {
            Some(sid) => self
                .engine
                .events
                .conversation(sid)
                .map(|m| m.len())
                .unwrap_or(0),
            None => 0,
        }
    }
}

/// The worker behind [`App`]. Owns the program, the registry, and the bus; resolves and runs journeys.
/// Held in an `Arc` so the `spawn` op can re-enter it through a `Weak<dyn JourneyHost>`.
pub(crate) struct Engine {
    pub(crate) program: Program,
    pub(crate) registry: ToolRegistry,
    pub(crate) bus: Bus,
    /// Active `spawn` recursion depth (guards against unbounded self-spawn).
    depth: AtomicU32,
    /// Monotonic counter giving each journey run a distinct session id.
    runs: AtomicU64,
    /// When true, journeys run under an allow-all approver (`--yes`); otherwise destructive ops
    /// outside the pre-allowed safe set are **denied** (the safe headless default).
    auto_approve: bool,
    /// The model provider (when wired); needed to assemble an agent-target engine lazily. An
    /// `agent`-bound trigger with no provider is a clear error.
    provider: Option<Arc<dyn Provider>>,
    /// The host default model (used when an `AgentDecl` declares none, and for new sessions).
    default_model: String,
    /// The append-only store backing agent-target **session memory** (a Slack thread → one session).
    events: Arc<EventStore>,
    /// Lazily-built engines for agents named by an `agent`-bound trigger, keyed by agent name.
    agents: Mutex<HashMap<String, Arc<FlowEngine>>>,
    /// `(agent, conversation)` → persistent session id (in-memory; a restart starts threads fresh).
    sessions: Mutex<HashMap<(String, String), String>>,
}

impl Engine {
    fn new(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: String,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
    ) -> Arc<Self> {
        let bus = Bus::new();
        let channels = Arc::new(program.channels.clone());
        // Agent-target turns persist per-thread conversation memory here; in-memory is fine for v1
        // (a restart starts threads fresh — flagged, pairs with D-02 later).
        let events = Arc::new(EventStore::in_memory().expect("flux-app: in-memory event store"));
        // `new_cyclic`: the `spawn` op needs a back-reference to the engine it re-enters, but the
        // engine owns the registry that owns the op — a `Weak` breaks the cycle.
        Arc::new_cyclic(|weak: &Weak<Engine>| {
            let mut registry = ToolRegistry::new();
            flux_tools::register_builtins(&mut registry);
            if let Some(provider) = provider.clone() {
                flux_cognition::CognitionPack::new(provider, model.clone()).register(&mut registry);
            }
            let host: Weak<dyn JourneyHost> = weak.clone();
            ops::register(&mut registry, bus.clone(), channels, host);
            // Extra tools assembled by the surface (datasource retrieval ops + integration plugin
            // tools) — available to journeys and to the `trigger.agent` target's registry.
            for tool in extra_tools {
                registry.register(tool);
            }
            Engine {
                program,
                registry,
                bus,
                depth: AtomicU32::new(0),
                runs: AtomicU64::new(0),
                auto_approve,
                provider,
                default_model: model,
                events,
                agents: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
            }
        })
    }

    /// Run every trigger whose `on` label equals `label`, collecting each journey run.
    async fn run_triggers(
        &self,
        label: &str,
        payload: &Value,
        sink: &mut dyn AgentSink,
    ) -> Result<Vec<JourneyRun>> {
        let mut runs = Vec::new();
        for trigger in self.program.triggers.iter().filter(|t| t.on == label) {
            // An `agent`-bound trigger wakes an agent turn (the model drives RAG + granted tools over
            // the thread's persistent session); otherwise it runs a journey (a fixed DAG), unchanged.
            let run = match trigger.agent.as_deref() {
                Some(agent) => self.run_agent(agent, label, payload).await?,
                None => self.run_journey(&trigger.run, payload, sink).await?,
            };
            runs.push(run);
        }
        Ok(runs)
    }

    /// One-shot delivery: process the initial event, then drain and process any events its journeys
    /// emitted, repeating until the bus is quiet or [`MAX_CASCADE`] events have been handled.
    async fn deliver(&self, label: String, payload: Value) -> Result<Vec<JourneyRun>> {
        // Subscribe first so emits *during* the journeys are captured.
        let mut rx = self.bus.subscribe();
        let mut sink = RecordingSink::default();
        let mut pending: VecDeque<Event> = VecDeque::new();
        pending.push_back(Event::new(label, payload));
        let mut results = Vec::new();
        let mut budget = MAX_CASCADE;

        while let Some(ev) = pending.pop_front() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            results.extend(self.run_triggers(&ev.label, &ev.payload, &mut sink).await?);
            // Drain events the journeys just emitted into the work queue.
            loop {
                match rx.try_recv() {
                    Ok(e) => pending.push_back(e),
                    Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
                    Err(TryRecvError::Lagged(_)) => continue,
                }
            }
        }
        Ok(results)
    }

    /// Execute one named journey to completion, reusing flux-flow's engine path (full envelope).
    async fn run_journey(
        &self,
        name: &str,
        payload: &Value,
        sink: &mut dyn AgentSink,
    ) -> Result<JourneyRun> {
        let ast = self
            .program
            .flow_named(name)
            .cloned()
            .ok_or_else(|| Error::Other(format!("unknown journey `{name}`")))?;

        // Depth guard: increment, ensure we decrement on every exit, then check.
        let prev = self.depth.fetch_add(1, Ordering::SeqCst);
        let _guard = DepthGuard(&self.depth);
        if prev >= MAX_SPAWN_DEPTH {
            return Err(Error::Other(format!(
                "spawn recursion exceeded max depth {MAX_SPAWN_DEPTH}"
            )));
        }

        let store = FlowStore::in_memory().map_err(other)?;
        let session_id = format!("{name}#{}", self.runs.fetch_add(1, Ordering::SeqCst));
        seed_payload(&store, &session_id, payload)?;
        let executor = build_executor(self.registry.clone(), self.auto_approve)?;
        analyze_composites(&self.program.ops, &self.registry)
            .map_err(|d| Error::Other(format!("composite ops: {}", join_diags(&d))))?;

        let outcome = if self.program.ops.is_empty() {
            flux_flow::runtime::execute_flow(&store, &executor, &session_id, &ast, sink).await
        } else {
            flux_flow::runtime::execute_flow_with_composites(
                &store,
                &executor,
                &session_id,
                &ast,
                &self.program.ops,
                sink,
            )
            .await
        }
        .map_err(other)?;

        Ok(JourneyRun {
            journey: name.to_string(),
            result: outcome.result,
            steps: outcome.steps,
        })
    }

    /// Get (build + cache) the [`FlowEngine`] for a declared agent. Built lazily on first use; an
    /// `agent`-bound trigger naming an undeclared agent, or one with no model provider, is a clear error.
    fn agent_engine(&self, name: &str) -> Result<Arc<FlowEngine>> {
        let mut cache = self.agents.lock().expect("agents cache poisoned");
        if let Some(engine) = cache.get(name) {
            return Ok(engine.clone());
        }
        let decl = self
            .program
            .agents
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| Error::Other(format!("trigger names unknown agent `{name}`")))?;
        let provider = self
            .provider
            .clone()
            .ok_or_else(|| Error::Other(format!("agent `{name}` needs a model provider")))?;
        let engine = Arc::new(build_agent_engine(
            decl,
            provider,
            self.registry.clone(),
            self.events.clone(),
            &self.default_model,
        )?);
        cache.insert(name.to_string(), engine.clone());
        Ok(engine)
    }

    /// Resolve the persistent session for `(agent, conversation)`: reuse the bound session (multi-turn
    /// thread memory) or mint one. A delivery with no conversation id runs in a fresh one-shot session.
    fn session_for(&self, agent: &str, conversation: Option<&str>) -> Result<String> {
        match conversation {
            Some(conv) => {
                let key = (agent.to_string(), conv.to_string());
                let mut map = self.sessions.lock().expect("sessions map poisoned");
                if let Some(sid) = map.get(&key) {
                    return Ok(sid.clone());
                }
                let sid = self
                    .events
                    .create_session(&self.default_model)
                    .map_err(other)?;
                map.insert(key, sid.clone());
                Ok(sid)
            }
            None => self
                .events
                .create_session(&self.default_model)
                .map_err(other),
        }
    }

    /// Run one agent turn for an `agent`-bound trigger: the model drives RAG + granted tools over the
    /// thread's persistent session, and the assistant's reply becomes the run result (the channel posts
    /// it). The conversation id (a Slack thread ts) binds repeated events to one session.
    async fn run_agent(&self, name: &str, label: &str, payload: &Value) -> Result<JourneyRun> {
        let engine = self.agent_engine(name)?;
        let conversation = payload
            .get("conversation")
            .or_else(|| payload.get("thread"))
            .and_then(|v| v.as_str());
        let session_id = self.session_for(name, conversation)?;
        // The turn's input: a real user message (a Slack mention's `text`) when present; otherwise
        // synthesize the event context so an event-driven agent (a `startup`/schedule trigger carries no
        // `text`) still wakes to a concrete turn naming the trigger that fired it (flux D-11).
        let input = match payload.get("text").and_then(|v| v.as_str()) {
            Some(t) if !t.trim().is_empty() => t.to_string(),
            _ => event_context(label, payload),
        };
        let mut sink = RecordingSink::default();
        engine
            .run_turn(&session_id, &input, &mut sink)
            .await
            .map_err(other)?;
        Ok(JourneyRun {
            journey: name.to_string(),
            result: sink.text,
            steps: sink.tools.len(),
        })
    }
}

#[async_trait]
impl JourneyHost for Engine {
    async fn run_journey_for_spawn(&self, name: &str, payload: Value) -> Result<String> {
        let mut sink = RecordingSink::default();
        Ok(self.run_journey(name, &payload, &mut sink).await?.result)
    }
}

/// Build the execution envelope for a journey: a guarded [`System`] rooted at the current working
/// directory, the shared op registry, and an approver chosen by `auto_approve`. Every op dispatches
/// through this `Executor`, so permission rules and effect gating apply exactly as in the interactive
/// engine.
///
/// **Safe headless default (`auto_approve = false`):** the orchestration verbs + read-only builtins are
/// pre-allowed (they run without prompting); anything else (`bash`, `write`, `git_*`, …) falls to a
/// [`DenyApprover`] and is **denied** — there is no human at a prompt, so destructive ops in an
/// untrusted program cannot execute. `auto_approve = true` (the CLI's `--yes`) swaps in an
/// [`AllowApprover`] for trusted, pre-authored programs.
fn build_executor(registry: ToolRegistry, auto_approve: bool) -> Result<Executor> {
    let root = std::env::current_dir().map_err(other)?;
    let workspace = Workspace::new(&root).map_err(other)?;
    let system = Arc::new(System::new(workspace));
    let ctx = ToolContext::new(system);
    let allow: Vec<String> = [
        "emit", "send", "ask", "spawn", "read", "glob", "grep", "search",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let perms = PermissionManager::from_rules(&allow, &[]);
    let approver: Arc<dyn Approver> = if auto_approve {
        Arc::new(AllowApprover)
    } else {
        Arc::new(DenyApprover)
    };
    Ok(Executor::new(registry, perms, approver, ctx))
}

/// Map a program-level [`AgentDecl`] to an [`AgentSpec`]. Its declared `tools` become **both** the visible
/// op subset *and* the pre-allow grants (`permissions.allow`), so under a [`DenyApprover`] only granted ops
/// run and everything else is denied — declared grants without a blanket `--yes`. The `description` (or a
/// `settings.system_prompt` string) seeds the persona; `model` falls back to the host default.
fn agent_spec_from_decl(decl: &AgentDecl, default_model: &str, cwd: PathBuf) -> AgentSpec {
    let system_prompt = decl
        .description
        .clone()
        .or_else(|| {
            decl.settings
                .get("system_prompt")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| AgentSpec::default().system_prompt);
    AgentSpec {
        model: decl
            .model
            .clone()
            .unwrap_or_else(|| default_model.to_string()),
        system_prompt,
        tools: Some(decl.tools.clone()),
        permissions: Permissions {
            allow: decl.tools.clone(),
            deny: Vec::new(),
        },
        cwd,
        ..AgentSpec::default()
    }
}

/// Assemble an agent-target [`FlowEngine`] from a declaration: a guarded [`System`] rooted at the cwd, the
/// host's op registry (subset to the agent's tools), the spec's grants, and a headless [`DenyApprover`] —
/// so the agent runs only its granted ops with no human at a prompt.
fn build_agent_engine(
    decl: &AgentDecl,
    provider: Arc<dyn Provider>,
    registry: ToolRegistry,
    events: Arc<EventStore>,
    default_model: &str,
) -> Result<FlowEngine> {
    let root = std::env::current_dir().map_err(other)?;
    let workspace = Workspace::new(&root).map_err(other)?;
    let system = Arc::new(System::new(workspace));
    let ctx = ToolContext::new(system);
    let spec = agent_spec_from_decl(decl, default_model, root);
    let approver: Arc<dyn Approver> = Arc::new(DenyApprover);
    // The agent loop's planner reads the turn's conversation via the FlowStore (`store.conversation()`),
    // which delegates to the FlowStore's *internal* event log. Back it with the SAME `events` store the
    // engine records the user message into — otherwise `in_memory()` mints a fresh, empty EventStore and
    // the planner sees no conversation, so the model only ever gets the system prompt (never the user's
    // message). This is what makes an `agent`-bound trigger actually answer the inbound mention.
    let flow = FlowStore::in_memory_with_events(events.clone()).map_err(other)?;
    spec.assemble(provider, registry, approver, ctx, events, flow)
        .map_err(other)
}

/// Synthesize a turn input for an `agent`-bound trigger whose event carries no user `text` (a `startup`
/// or a schedule tick, vs. a Slack mention). The agent's system prompt says what to do per event; this
/// hands it a concrete turn naming the trigger that woke it, plus any payload fields (e.g. a tick's
/// `at`), so an event-driven agent acts instead of waking to an empty prompt (flux D-11).
fn event_context(label: &str, payload: &Value) -> String {
    let mut s = format!("You were woken by the `{label}` trigger (event `{label}`).");
    if let Some(obj) = payload.as_object() {
        let fields: Vec<String> = obj
            .iter()
            .filter(|(k, _)| k.as_str() != "text")
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        if !fields.is_empty() {
            s.push_str(&format!(" Event data: {}.", fields.join(", ")));
        }
    }
    s.push_str(" Act according to your instructions for this event.");
    s
}

fn join_diags(diags: &[flux_lang::analyze::Diagnostic]) -> String {
    diags
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Seed an event's payload into the journey's session so the flow can read it: the whole payload binds
/// to `$input`, and each top-level field binds to its own symbol (so a journey body can interpolate
/// `{text}` or reference `$text` directly).
fn seed_payload(store: &FlowStore, session_id: &str, payload: &Value) -> Result<()> {
    bind_symbol(store, session_id, "input", payload)?;
    if let Some(obj) = payload.as_object() {
        for (key, value) in obj {
            bind_symbol(store, session_id, key, value)?;
        }
    }
    Ok(())
}

fn bind_symbol(store: &FlowStore, session_id: &str, name: &str, value: &Value) -> Result<()> {
    let flux_value = FluxValue::from_json(value);
    let value_id = store.put_value(session_id, &flux_value).map_err(other)?;
    store
        .bind(
            session_id,
            &SymbolName(name.to_string()),
            &value_id,
            None,
            &summarize(value),
            Visibility::Visible,
        )
        .map_err(other)?;
    Ok(())
}

/// A short human summary of a seeded value (the raw string for a string; compact JSON otherwise),
/// capped so a large payload doesn't bloat the session view.
fn summarize(value: &Value) -> String {
    let s = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if s.len() > 200 {
        format!("{}…", &s[..200])
    } else {
        s
    }
}

/// Map any foreign error onto [`flux_core::Error::Other`].
fn other(e: impl std::fmt::Display) -> Error {
    Error::Other(e.to_string())
}

/// Decrements the active spawn depth when a journey run unwinds (success, error, or early return).
struct DepthGuard<'a>(&'a AtomicU32);

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A minimal [`AgentSink`] that records streamed text and the op names dispatched. The journey's
/// canonical result is taken from the `FlowOutcome`, so this only needs to capture for inspection.
#[derive(Default)]
pub struct RecordingSink {
    pub text: String,
    pub tools: Vec<String>,
}

impl AgentSink for RecordingSink {
    fn text_delta(&mut self, text: &str) {
        self.text.push_str(text);
    }
    fn tool_call(&mut self, name: &str, _input: &Value) {
        self.tools.push(name.to_string());
    }
}

#[cfg(test)]
mod agent_target_tests {
    use super::*;
    use async_trait::async_trait;
    use flux_core::{Chunk, StopReason};
    use flux_lang::program::Module;
    use flux_provider::{ChunkStream, Request};

    /// A provider that answers every turn with the same prose (no plan) — enough to drive an agent turn
    /// to a final reply hermetically (no network, no real model).
    struct ReplyProvider {
        reply: String,
    }
    #[async_trait]
    impl Provider for ReplyProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let chunks = vec![
                Chunk::TextDelta(self.reply.clone()),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ];
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    fn program(src: &str) -> Program {
        match Module::parse_str(src).expect("parse program") {
            Module::Program(p) => p,
            Module::Flow(_) => panic!("expected a program, got a bare flow"),
        }
    }

    /// An app with one agent reachable via an `agent`-bound `slack` trigger.
    fn app_with_agent(reply: &str) -> App {
        let src = "\
agent assistant
  description \"be terse\"
  tools []

trigger t1
  on \"slack\"
  run _
  agent assistant
";
        let provider: Arc<dyn Provider> = Arc::new(ReplyProvider {
            reply: reply.to_string(),
        });
        App::with_options(program(src), Some(provider), "mock", false)
    }

    #[test]
    fn agent_spec_maps_tools_to_grants_and_persona() {
        let decl = AgentDecl {
            name: "a".into(),
            model: None,
            tools: vec!["read".into(), "now".into()],
            datasources: vec![],
            description: Some("be terse".into()),
            settings: Value::Null,
        };
        let spec = agent_spec_from_decl(&decl, "host-model", PathBuf::from("."));
        assert_eq!(spec.model, "host-model"); // falls back to the host default
        assert_eq!(spec.system_prompt, "be terse");
        // tools are the visible subset AND the pre-allow grants — under DenyApprover only these run.
        assert_eq!(
            spec.tools.as_deref(),
            Some(&["read".to_string(), "now".to_string()][..])
        );
        assert_eq!(
            spec.permissions.allow,
            vec!["read".to_string(), "now".to_string()]
        );
        assert!(spec.permissions.deny.is_empty());
    }

    #[tokio::test]
    async fn agent_trigger_runs_a_turn_and_returns_the_reply() {
        let app = app_with_agent("hi back");
        let runs = app
            .deliver("slack", json!({ "text": "hello", "conversation": "T1" }))
            .await
            .expect("deliver");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].journey, "assistant");
        assert!(
            runs[0].result.contains("hi back"),
            "agent reply should be the model's answer, got: {:?}",
            runs[0].result
        );
    }

    #[tokio::test]
    async fn same_conversation_reuses_one_session_distinct_ones_isolate() {
        let app = app_with_agent("ok");
        // Two mentions on the same thread accumulate in one session (multi-turn memory).
        app.deliver("slack", json!({ "text": "first", "conversation": "T1" }))
            .await
            .unwrap();
        let after_one = app.agent_session_len("assistant", "T1");
        app.deliver("slack", json!({ "text": "second", "conversation": "T1" }))
            .await
            .unwrap();
        let after_two = app.agent_session_len("assistant", "T1");
        assert!(
            after_one > 0,
            "the first turn should persist to the thread's session"
        );
        assert!(
            after_two > after_one,
            "the thread's session should grow across turns: {after_one} -> {after_two}"
        );
        // A different thread is a separate session, not the T1 one.
        app.deliver(
            "slack",
            json!({ "text": "elsewhere", "conversation": "T2" }),
        )
        .await
        .unwrap();
        assert!(app.agent_session_len("assistant", "T2") > 0);
        assert_eq!(
            app.agent_session_len("assistant", "T1"),
            after_two,
            "delivering to T2 must not touch T1's session"
        );
    }

    #[tokio::test]
    async fn trigger_without_agent_still_runs_its_journey() {
        // A plain journey trigger (no `agent`) runs the journey unchanged — the agentic path is additive.
        let src = "\
trigger t1
  on \"ping\"
  run pong

journey pong
  flow
    return \"pong!\"
";
        let app = App::with_options(program(src), None, "mock", false);
        let runs = app.deliver("ping", json!({})).await.expect("deliver");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].journey, "pong");
        assert_eq!(runs[0].result, "pong!");
    }
}
