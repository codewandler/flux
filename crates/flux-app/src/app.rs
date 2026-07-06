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

use flux_agent::{AgentSpec, Permissions, DEFAULT_COMPACT_THRESHOLD_CHARS};
use flux_core::{Error, Result, Usage};
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_flow::registry::analyze_composites;
use flux_flow::state::FlowStore;
use flux_flow::AgentSink;
use flux_lang::ast::{Node, SymbolName, Value as FluxValue, Visibility};
use flux_lang::program::{AgentDecl, Program};
use flux_lang::runtime::FlowOutcome;
use flux_orchestrate::{SubAgents, TaskTool};
use flux_provider::Provider;
use flux_runtime::{
    AllowApprover, Approver, DenyApprover, Executor, PermissionManager, Spawner, Tool, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_secret::Redactor;
use flux_system::{System, Workspace};

use crate::bus::{Bus, Event};
use crate::ops::{self, JourneyHost};
use crate::park::{self, ParkedAsk};

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
    /// The turn(s)' accumulated token usage (C-33), when the run drove at least one model call that
    /// reported it — `None` for a run that dispatched no model turn (a pure-op journey, or a journey
    /// whose cognition ops don't yet report usage). Summed across every `turn_end` the run's sink saw,
    /// so a journey/agent turn that makes more than one model call still attributes its full cost.
    pub usage: Option<Usage>,
    /// The canonical `provider/model` spec of the engine that drove this run (C-33) — an
    /// `agent`-bound trigger's actual engine (`flux_core::canonical_model_spec` over its provider +
    /// model), or the app's default model for a plain journey (a journey has no single "engine"; the
    /// app default is the honest stand-in, and it's only ever paired with real cost when `usage` is
    /// `Some`). A cost-display surface should ignore this when `usage` is `None`.
    pub model: String,
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
        Self::with_sub_agents(
            program,
            provider,
            model,
            auto_approve,
            extra_tools,
            None,
            Redactor::new(),
        )
    }

    /// Like [`with_tools`](Self::with_tools) but also wires a sub-agent [`SubAgents`] bundle: the
    /// `task` tool is registered into the host registry and every journey run installs the built
    /// spawner on its executor's [`ToolContext`], so a journey (or a composite op it calls, e.g.
    /// `strict_review`'s bounded reviewer fan-out — flux L-13) can delegate to a named role exactly as
    /// the CLI's `build_agent`/the SDK's `FlowClient::with_sub_agents` do — the same construction path
    /// (`SubAgents::into_spawner`), not a re-implementation.
    ///
    /// `redactor` is the host's shared secret redactor — pass the SAME one `resolve_secrets` seeded
    /// (clones share the value store), so program-declared secrets are scrubbed from every journey's
    /// and agent-target's tool output.
    pub fn with_sub_agents(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
        sub_agents: Option<SubAgents>,
        redactor: Redactor,
    ) -> Self {
        App {
            engine: Engine::new(
                program,
                provider,
                model.into(),
                auto_approve,
                extra_tools,
                sub_agents,
                redactor,
            ),
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
    pub async fn agent_engine(&self, name: &str) -> Result<Arc<FlowEngine>> {
        self.engine.agent_engine(name).await
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
    /// The sub-agent spawner (when [`App::with_sub_agents`] wired one) — installed on every journey
    /// run's executor so a `task` call (e.g. inside the `strict_review` composite op's reviewer
    /// fan-out) can delegate. `None` leaves journeys exactly as before (`task` errors with "no
    /// sub-agent spawner configured").
    spawner: Option<Arc<dyn Spawner>>,
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
    /// The guarded `System` every journey run's executor + (when wired) the sub-agent spawner share —
    /// built once here rather than a fresh instance per journey run, so a journey and the sub-agents
    /// it spawns always resolve paths against the identical workspace root.
    system: Arc<System>,
    /// The host's shared secret redactor — the one `resolve_secrets` seeded (clones share the value
    /// store). Installed on every journey-run executor's and agent-target engine's `ToolContext`, so
    /// program-declared secrets are scrubbed from tool output/logs everywhere.
    redactor: Redactor,
    /// Journeys parked on an `ask`, oldest first, each waiting for the correlated reply (A-11).
    /// [`Engine::run_triggers`] checks these before routing: a correlated inbound event resumes the
    /// oldest matching park instead of triggering journeys. See [`crate::park`] for the rule.
    parks: Mutex<Vec<ParkedAsk>>,
}

impl Engine {
    fn new(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: String,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
        sub_agents: Option<SubAgents>,
        redactor: Redactor,
    ) -> Arc<Self> {
        let bus = Bus::new();
        let channels = Arc::new(program.channels.clone());
        // Agent-target turns persist per-thread conversation memory here; in-memory is fine for v1
        // (a restart starts threads fresh — flagged, pairs with D-02 later).
        let events = Arc::new(EventStore::in_memory().expect("flux-app: in-memory event store"));
        // A guarded `System` rooted at the cwd, built ONCE and shared by every journey run's executor
        // and, when `sub_agents` is set, the spawner it builds — so a journey and the sub-agents it
        // spawns always resolve paths against the identical workspace root (never two independent
        // `System`s from separate `current_dir()` calls).
        let system = Arc::new(System::new(
            Workspace::from_env(std::env::current_dir().unwrap_or_else(|_| ".".into()))
                .expect("flux-app: workspace"),
        ));
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
            // Sub-agents (L-13): register `task` and build the spawner over the shared `system` — the
            // same `SubAgents::into_spawner` construction path the CLI's `build_agent` and the SDK's
            // `FlowClient::with_sub_agents` use, so a journey delegates through the identical envelope.
            // Children audit into the app's own event store by default (A-08), correlated per spawn.
            let spawner = sub_agents.map(|sa| {
                registry.register(Arc::new(TaskTool));
                sa.with_audit(events.clone()).into_spawner(system.clone())
            });
            Engine {
                program,
                registry,
                bus,
                depth: AtomicU32::new(0),
                runs: AtomicU64::new(0),
                auto_approve,
                spawner,
                provider,
                default_model: model,
                events,
                agents: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
                system,
                redactor,
                parks: Mutex::new(Vec::new()),
            }
        })
    }

    /// Run every trigger whose `on` label equals `label`, collecting each journey run.
    ///
    /// A pending ask-park takes precedence: an event that correlates with a parked journey (see
    /// [`crate::park`] for the rule) is **consumed** as that journey's reply — it resumes the park
    /// and does not also route through triggers (otherwise the reply line would start a fresh
    /// journey too). Uncorrelated events route normally and leave every park alone.
    async fn run_triggers(
        &self,
        label: &str,
        payload: &Value,
        sink: &mut dyn AgentSink,
    ) -> Result<Vec<JourneyRun>> {
        if let Some(run) = self.try_resume_ask(label, payload, sink).await? {
            return Ok(vec![run]);
        }
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
        let mut ast = self
            .program
            .flow_named(name)
            .cloned()
            .ok_or_else(|| Error::Other(format!("unknown journey `{name}`")))?;
        // Lower top-level `ask` calls onto the suspension seam (ask + await) — see `crate::park`.
        ast.body = park::rewrite_asks(std::mem::take(&mut ast.body));

        // Depth guard: increment, ensure we decrement on every exit, then check.
        let prev = self.depth.fetch_add(1, Ordering::SeqCst);
        let _guard = DepthGuard(&self.depth);
        if prev >= MAX_SPAWN_DEPTH {
            return Err(Error::Other(format!(
                "spawn recursion exceeded max depth {MAX_SPAWN_DEPTH}"
            )));
        }

        // `Arc`: an ask-park keeps the run's store alive (prefix symbols + the persisted
        // suspension) until the reply arrives.
        let store = Arc::new(FlowStore::in_memory().map_err(other)?);
        let session_id = format!("{name}#{}", self.runs.fetch_add(1, Ordering::SeqCst));
        seed_payload(&store, &session_id, payload)?;
        let executor = build_executor(
            self.registry.clone(),
            self.auto_approve,
            self.spawner.clone(),
            self.system.clone(),
            self.redactor.clone(),
        )?;
        // The journey's session id on the context, so a `task` call inside the journey correlates
        // its child's audit stream back to this run (A-08).
        executor.context().set_session(&session_id);
        analyze_composites(&self.program.ops, &self.registry)
            .map_err(|d| Error::Other(format!("composite ops: {}", join_diags(&d))))?;

        // Where the asked channel is read from if this run parks: the expects-reply sends recorded
        // from here on belong to this segment.
        let sent_before = self.bus.sent().len();
        // C-33: capture this run's own usage rather than accumulating into the caller's (possibly
        // shared/reused, and always type-erased) `sink` — see `UsageCapture`'s doc comment.
        let mut capture = UsageCapture {
            inner: sink,
            usage: None,
        };
        let outcome = if self.program.ops.is_empty() {
            flux_flow::runtime::execute_flow(&store, &executor, &session_id, &ast, &mut capture)
                .await
        } else {
            flux_flow::runtime::execute_flow_with_composites(
                &store,
                &executor,
                &session_id,
                &ast,
                &self.program.ops,
                &mut capture,
            )
            .await
        }
        .map_err(other)?;
        let usage = capture.usage;
        let model = self.default_model_spec();

        if let Some(parked) = self.park_if_asked(
            name,
            &session_id,
            &store,
            &ast.body,
            &outcome,
            sent_before,
            0,
            usage.clone(),
            model.clone(),
        )? {
            return Ok(parked);
        }

        Ok(JourneyRun {
            journey: name.to_string(),
            result: outcome.result,
            steps: outcome.steps,
            usage,
            model,
        })
    }

    /// The canonical `provider/model` spec of the app's default model (C-33) — the "driving engine
    /// spec" attributed to a plain journey run, which (unlike an `agent`-bound trigger's own
    /// [`FlowEngine`], used in [`Self::run_agent`]) has no per-op engine of its own.
    fn default_model_spec(&self) -> String {
        flux_core::canonical_model_spec(
            self.provider.as_ref().map(|p| p.name()),
            &self.default_model,
        )
    }

    /// Park the run when `outcome` suspended on an ask-lowered `await`: persist the resume point on
    /// the run's own [`FlowStore`] (the same suspension latch the interactive engine uses) and queue
    /// a [`ParkedAsk`] keyed by the asked channel. Returns the parked [`JourneyRun`] (empty result —
    /// the question is already on the channel), or `None` on a normal completion. A suspension from
    /// a hand-written `await` (a foreign `source`) is left untouched — flux-app has no resume
    /// surface for those (unchanged pre-A-11 behavior: the partial result is returned).
    #[allow(clippy::too_many_arguments)]
    fn park_if_asked(
        &self,
        journey: &str,
        session_id: &str,
        store: &Arc<FlowStore>,
        body: &[Node],
        outcome: &FlowOutcome,
        sent_before: usize,
        prior_steps: usize,
        usage: Option<Usage>,
        model: String,
    ) -> Result<Option<JourneyRun>> {
        let Some(susp) = &outcome.suspension else {
            return Ok(None);
        };
        if susp.source != park::ASK_REPLY_SOURCE {
            return Ok(None);
        }
        // The asked channel, from runtime truth: the lowered ask call executes immediately before
        // its `await`, so the segment's most recent expects-reply send is necessarily this ask
        // (a dynamically-computed channel name resolves correctly too).
        let sent = self.bus.sent();
        let Some(channel) = sent
            .get(sent_before..)
            .unwrap_or(&[])
            .iter()
            .rev()
            .find(|m| m.expects_reply)
            .map(|m| m.channel.clone())
        else {
            // Defensive: an ask-marked await with no recorded ask send — nothing to correlate on.
            return Ok(None);
        };
        store
            // Journeys execute their flow unnamed (see `run_journey`), so the park persists no
            // flow name — run and resume must derive the same (hash-only) checkpoint key.
            .save_suspension(session_id, None, body, susp.node, &susp.source)
            .map_err(other)?;
        let steps = prior_steps + outcome.steps;
        self.parks.lock().expect("parks poisoned").push(ParkedAsk {
            channel,
            journey: journey.to_string(),
            session_id: session_id.to_string(),
            store: store.clone(),
            steps,
        });
        Ok(Some(JourneyRun {
            journey: journey.to_string(),
            result: String::new(),
            steps,
            usage,
            model,
        }))
    }

    /// If `label`/`payload` is the reply a parked ask waits for, consume it: remove the oldest
    /// correlated park and resume that journey with the reply text. `None` means the event did not
    /// correlate and should route through triggers as usual.
    async fn try_resume_ask(
        &self,
        label: &str,
        payload: &Value,
        sink: &mut dyn AgentSink,
    ) -> Result<Option<JourneyRun>> {
        let park = {
            let mut parks = self.parks.lock().expect("parks poisoned");
            parks
                .iter()
                .position(|p| park::event_correlates(label, &self.program.channels, &p.channel))
                .map(|i| parks.remove(i))
        };
        let Some(park) = park else {
            return Ok(None);
        };
        let run = self.resume_parked(park, reply_text(payload), sink).await?;
        Ok(Some(run))
    }

    /// Resume a parked journey with `reply` bound as the suspended ask's result. Re-enters through
    /// the NORMAL engine path — `flux_flow::runtime::resume_flow*` over a fresh full-envelope
    /// executor and the park's own store — so permission + approval rules apply to the continuation
    /// exactly as to the original run (no side-channel execution of flow bodies). The continuation
    /// may itself `ask` again, in which case it parks again.
    async fn resume_parked(
        &self,
        park: ParkedAsk,
        reply: String,
        sink: &mut dyn AgentSink,
    ) -> Result<JourneyRun> {
        let ParkedAsk {
            journey,
            session_id,
            store,
            steps: prior_steps,
            ..
        } = park;
        let Some((flow_name, body, node, _source)) =
            store.take_suspension(&session_id).map_err(other)?
        else {
            return Err(Error::Other(format!(
                "parked ask for journey `{journey}` has no persisted suspension"
            )));
        };
        let executor = build_executor(
            self.registry.clone(),
            self.auto_approve,
            self.spawner.clone(),
            self.system.clone(),
            self.redactor.clone(),
        )?;
        executor.context().set_session(&session_id);
        analyze_composites(&self.program.ops, &self.registry)
            .map_err(|d| Error::Other(format!("composite ops: {}", join_diags(&d))))?;

        let sent_before = self.bus.sent().len();
        let input = FluxValue::String(reply);
        // C-33: same per-run usage capture as `run_journey` — see `UsageCapture`.
        let mut capture = UsageCapture {
            inner: sink,
            usage: None,
        };
        let outcome = if self.program.ops.is_empty() {
            flux_flow::runtime::resume_flow(
                &store,
                &executor,
                &session_id,
                &body,
                node,
                input,
                &mut capture,
            )
            .await
        } else {
            flux_flow::runtime::resume_flow_with_composites(
                &store,
                &executor,
                &session_id,
                flow_name.as_deref(),
                &body,
                node,
                input,
                &self.program.ops,
                &mut capture,
            )
            .await
        }
        .map_err(other)?;
        let usage = capture.usage;
        let model = self.default_model_spec();

        if let Some(parked) = self.park_if_asked(
            &journey,
            &session_id,
            &store,
            &body,
            &outcome,
            sent_before,
            prior_steps,
            usage.clone(),
            model.clone(),
        )? {
            return Ok(parked);
        }

        Ok(JourneyRun {
            journey,
            result: outcome.result,
            steps: prior_steps + outcome.steps,
            usage,
            model,
        })
    }

    /// Get (build + cache) the [`FlowEngine`] for a declared agent. Built lazily on first use; an
    /// `agent`-bound trigger naming an undeclared agent, or one with no model provider, is a clear error.
    async fn agent_engine(&self, name: &str) -> Result<Arc<FlowEngine>> {
        // Fast path: a cached engine. Scope the guard so it is dropped before the build await — engine
        // construction reads persona files through the guarded System (async IO), and a std `MutexGuard`
        // must never be held across an `.await`.
        if let Some(engine) = self.agents.lock().expect("agents cache poisoned").get(name) {
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
        let engine = Arc::new(
            build_agent_engine(
                decl,
                provider,
                self.registry.clone(),
                self.events.clone(),
                &self.default_model,
                self.redactor.clone(),
            )
            .await?,
        );
        // A concurrent caller may have built+inserted the same agent while we were off-lock; keep the
        // first-inserted instance so every thread shares one engine.
        let mut cache = self.agents.lock().expect("agents cache poisoned");
        Ok(cache.entry(name.to_string()).or_insert(engine).clone())
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
        let engine = self.agent_engine(name).await?;
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
        // C-33: the engine's own provider + model IS the driving engine spec for this run — unlike a
        // plain journey, an agent-bound trigger has exactly one engine, so there is no aggregation
        // question here.
        let model = flux_core::canonical_model_spec(Some(engine.provider.name()), &engine.model);
        Ok(JourneyRun {
            journey: name.to_string(),
            result: sink.text,
            steps: sink.tools.len(),
            usage: sink.usage,
            model,
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

/// Build the execution envelope for a journey: the shared `system` (built once by [`Engine::new`] and
/// passed in — never a second, independently-`current_dir()`-resolved instance), the shared op
/// registry, and an approver chosen by `auto_approve`. Every op dispatches through this `Executor`, so
/// permission rules and effect gating apply exactly as in the interactive engine.
///
/// **Safe headless default (`auto_approve = false`):** the orchestration verbs + read-only builtins are
/// pre-allowed (they run without prompting); anything else (`bash`, `write`, `git_*`, …) falls to a
/// [`DenyApprover`] and is **denied** — there is no human at a prompt, so destructive ops in an
/// untrusted program cannot execute. `auto_approve = true` (the CLI's `--yes`) swaps in an
/// [`AllowApprover`] for trusted, pre-authored programs.
///
/// `spawner`, when [`App::with_sub_agents`] wired one, is installed on the returned executor's
/// [`ToolContext`] so a `task` call inside the journey (or a composite op it calls) can delegate —
/// the same seam [`ToolContext::with_spawner`] provides everywhere else.
fn build_executor(
    registry: ToolRegistry,
    auto_approve: bool,
    spawner: Option<Arc<dyn Spawner>>,
    system: Arc<System>,
    redactor: Redactor,
) -> Result<Executor> {
    let mut ctx = ToolContext::new(system).with_redactor(redactor);
    if let Some(spawner) = spawner {
        ctx = ctx.with_spawner(spawner);
    }
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
/// run and everything else is denied — declared grants without a blanket `--yes`. The persona is the
/// `description` (or a `settings.system_prompt` string), followed by the contents of any
/// `settings.system_prompt_files` paths — read through the guarded, workspace-confined `system` so a
/// declarative bot can keep a long persona in `bot/PERSONA.md` instead of inlining it (flux D-11). A
/// non-string entry or an unreadable path is a clean error. `model` falls back to the host default.
/// Resolve a served/agentic agent's compaction threshold (A-22). A `run_agent` target binds its
/// conversation to ONE persistent session (`session_for`), so without a working threshold it
/// re-sends the whole growing transcript every turn — linear cost, then a hard provider
/// context-window error. Precedence: per-agent (`settings.compact_threshold_chars`) > env
/// (`FLUX_COMPACT_CHARS`, the same knob the CLI honours) > the sane non-zero
/// [`DEFAULT_COMPACT_THRESHOLD_CHARS`]. A per-agent `0` disables compaction explicitly.
fn compact_threshold_for_decl(decl: &AgentDecl) -> usize {
    decl.settings
        .get("compact_threshold_chars")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize)
        .or_else(|| {
            std::env::var("FLUX_COMPACT_CHARS")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(DEFAULT_COMPACT_THRESHOLD_CHARS)
}

async fn agent_spec_from_decl(
    decl: &AgentDecl,
    default_model: &str,
    cwd: PathBuf,
    system: &System,
) -> Result<AgentSpec> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(base) = decl.description.clone().or_else(|| {
        decl.settings
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }) {
        parts.push(base);
    }
    if let Some(files) = decl
        .settings
        .get("system_prompt_files")
        .and_then(|v| v.as_array())
    {
        for f in files {
            let path = f.as_str().ok_or_else(|| {
                Error::Other(format!(
                    "agent `{}`: settings.system_prompt_files entries must be strings",
                    decl.name
                ))
            })?;
            let text = system.read_file(path).await.map_err(|e| {
                Error::Other(format!(
                    "agent `{}`: read system_prompt_files `{path}`: {e}",
                    decl.name
                ))
            })?;
            parts.push(text);
        }
    }
    let system_prompt = if parts.is_empty() {
        AgentSpec::default().system_prompt
    } else {
        parts.join("\n\n")
    };
    // Inline knowledge blocks declared in `settings.context` (A-19) — injected into the system prompt as
    // `<knowledge-base>` sections. A non-list or malformed entry is a clean error.
    let context: Vec<flux_core::ContextBlock> = match decl.settings.get("context") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            Error::Other(format!(
                "agent `{}`: settings.context must be a list of {{id,title,body}} blocks: {e}",
                decl.name
            ))
        })?,
        None => Vec::new(),
    };
    Ok(AgentSpec {
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
        context,
        compact_threshold_chars: compact_threshold_for_decl(decl),
        ..AgentSpec::default()
    })
}

/// Assemble an agent-target [`FlowEngine`] from a declaration: a guarded [`System`] rooted at the cwd, the
/// host's op registry (subset to the agent's tools), the spec's grants, and a headless [`DenyApprover`] —
/// so the agent runs only its granted ops with no human at a prompt.
async fn build_agent_engine(
    decl: &AgentDecl,
    provider: Arc<dyn Provider>,
    registry: ToolRegistry,
    events: Arc<EventStore>,
    default_model: &str,
    redactor: Redactor,
) -> Result<FlowEngine> {
    let root = std::env::current_dir().map_err(other)?;
    let workspace = Workspace::from_env(&root).map_err(other)?;
    let system = Arc::new(System::new(workspace));
    // Build the spec (which may read persona files through the guarded `system`) before moving the
    // `system` into the tool context.
    let spec = agent_spec_from_decl(decl, default_model, root, &system).await?;
    let ctx = ToolContext::new(system).with_redactor(redactor);
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

/// The reply text an inbound event carries for a parked ask: the `text` field when present (the
/// shape every channel delivers — the stdin loop, Slack, webhooks), a bare string payload verbatim,
/// otherwise the payload's compact JSON.
fn reply_text(payload: &Value) -> String {
    match payload {
        Value::String(s) => s.clone(),
        v => v
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string()),
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
///
/// C-33: also accumulates every `turn_end`'s [`Usage`] (summed field-by-field — a run may drive
/// more than one model call, e.g. an `agent`-bound trigger's turn), so [`Engine::run_agent`] can
/// attribute the run's real cost onto its [`JourneyRun`].
#[derive(Default)]
pub struct RecordingSink {
    pub text: String,
    pub tools: Vec<String>,
    pub usage: Option<Usage>,
}

impl AgentSink for RecordingSink {
    fn text_delta(&mut self, text: &str) {
        self.text.push_str(text);
    }
    fn tool_call(&mut self, name: &str, _input: &Value) {
        self.tools.push(name.to_string());
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        accumulate_usage(&mut self.usage, usage);
    }
}

/// Fold `next` into `acc` field-by-field (C-33): `None` on either side leaves the other untouched,
/// `Some` on both sums every counter, including `reported_cost_usd` — a run may drive more than one
/// priced model call and the running total must stay additive, not last-write-wins.
fn accumulate_usage(acc: &mut Option<Usage>, next: Option<Usage>) {
    let Some(next) = next else { return };
    match acc {
        Some(acc) => {
            acc.input_tokens += next.input_tokens;
            acc.output_tokens += next.output_tokens;
            acc.cache_creation_input_tokens += next.cache_creation_input_tokens;
            acc.cache_read_input_tokens += next.cache_read_input_tokens;
            acc.reasoning_tokens += next.reasoning_tokens;
            acc.reported_cost_usd = match (acc.reported_cost_usd, next.reported_cost_usd) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, other) => other,
            };
        }
        None => *acc = Some(next),
    }
}

/// Wraps a caller-supplied [`AgentSink`] to attribute [`Usage`] to exactly the run this wrapper
/// spans (C-33), while forwarding every event through unchanged. `run_journey`/`resume_parked`
/// receive a `&mut dyn AgentSink` that may be shared/reused across several journey runs in one
/// [`Engine::deliver`] call — reading a concrete field off a trait object isn't possible, and
/// summing directly into the shared sink would misattribute one journey's cost to the next. A
/// fresh wrapper per run gives each [`JourneyRun`] its own accurate total without changing what the
/// outer sink observes.
struct UsageCapture<'a> {
    inner: &'a mut dyn AgentSink,
    usage: Option<Usage>,
}

impl AgentSink for UsageCapture<'_> {
    fn text_delta(&mut self, text: &str) {
        self.inner.text_delta(text);
    }
    fn thinking_delta(&mut self, text: &str) {
        self.inner.thinking_delta(text);
    }
    fn planning(&mut self, active: bool) {
        self.inner.planning(active);
    }
    fn tool_call(&mut self, name: &str, input: &Value) {
        self.inner.tool_call(name, input);
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        self.inner.tool_result(name, result);
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.inner.observation(o);
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        self.inner.turn_end(usage.clone());
        accumulate_usage(&mut self.usage, usage);
    }
}

#[cfg(test)]
mod agent_target_tests {
    use super::*;
    use async_trait::async_trait;
    use flux_core::{Chunk, Role, StopReason};
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

    /// Like [`ReplyProvider`] but also reports real token usage (a `Chunk::Usage` before `Done`) —
    /// drives an agent turn whose `turn_end` usage is `Some`, so C-33's `JourneyRun::usage`/`::model`
    /// wiring can be exercised hermetically (no network, no real model).
    struct ReplyWithUsageProvider {
        reply: String,
    }
    #[async_trait]
    impl Provider for ReplyWithUsageProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let chunks = vec![
                Chunk::TextDelta(self.reply.clone()),
                Chunk::Usage(Usage {
                    input_tokens: 100,
                    output_tokens: 40,
                    ..Default::default()
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ];
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// A provider that echoes the latest user message straight back, so a test can observe the exact
    /// turn input the engine fed the model.
    struct EchoProvider;
    #[async_trait]
    impl Provider for EchoProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let echo = req
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::User))
                .map(|m| m.text())
                .unwrap_or_default();
            let chunks = vec![
                Chunk::TextDelta(echo),
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

    /// The C-13 wiring: the redactor `resolve_secrets` seeded is the SAME one every journey-run
    /// executor redacts with — a tool result leaking a resolved secret comes back scrubbed.
    #[tokio::test]
    async fn journey_executor_scrubs_resolved_secrets_from_tool_output() {
        // A fixture named `search` (inside build_executor's pre-allowed safe set) that leaks a secret.
        struct LeakyTool;
        #[async_trait]
        impl Tool for LeakyTool {
            fn spec(&self) -> flux_spec::ToolSpec {
                flux_spec::ToolSpec::read_only("search", "leaks", json!({"type": "object"}))
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _params: Value,
            ) -> Result<flux_runtime::ToolResult> {
                Ok(flux_runtime::ToolResult::ok(
                    "found: xoxb-app-secret-987".to_string(),
                ))
            }
        }
        let redactor = Redactor::new();
        redactor.add_secret("xoxb-app-secret-987"); // what resolve_secrets does at load
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(LeakyTool));
        let system = Arc::new(System::new(Workspace::new(".").unwrap()));
        let executor = build_executor(registry, false, None, system, redactor).unwrap();
        let r = executor.dispatch("search", json!({})).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            !r.content.contains("xoxb-app-secret-987"),
            "the resolved secret must be scrubbed from tool output: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn agent_spec_maps_tools_to_grants_and_persona() {
        let decl = AgentDecl {
            name: "a".into(),
            model: None,
            tools: vec!["read".into(), "now".into()],
            datasources: vec![],
            description: Some("be terse".into()),
            settings: Value::Null,
        };
        let system = System::new(Workspace::new(".").unwrap());
        let spec = agent_spec_from_decl(&decl, "host-model", PathBuf::from("."), &system)
            .await
            .unwrap();
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

    /// A-22: a served/agentic agent target gets a NON-ZERO compaction threshold by default (so its
    /// persistent-session conversation is bounded), and `settings.compact_threshold_chars` overrides
    /// it per-agent — including an explicit `0` to disable compaction. Before the fix every non-CLI
    /// construction used `AgentSpec::default()`'s `0`, so `maybe_compact` was a no-op and the
    /// transcript grew until the provider context window blew.
    #[tokio::test]
    async fn agent_spec_has_nonzero_compaction_default_and_per_agent_override() {
        let system = System::new(Workspace::new(".").unwrap());
        let base = AgentDecl {
            name: "a".into(),
            model: None,
            tools: vec![],
            datasources: vec![],
            description: Some("be terse".into()),
            settings: Value::Null,
        };

        // No per-agent setting → a sane non-zero default (the served agent compacts).
        let spec = agent_spec_from_decl(&base, "m", PathBuf::from("."), &system)
            .await
            .unwrap();
        assert!(
            spec.compact_threshold_chars > 0,
            "a served agent must compact by default, got {}",
            spec.compact_threshold_chars
        );

        // Per-agent override wins.
        let tuned = AgentDecl {
            settings: json!({ "compact_threshold_chars": 9999 }),
            ..base.clone()
        };
        let spec = agent_spec_from_decl(&tuned, "m", PathBuf::from("."), &system)
            .await
            .unwrap();
        assert_eq!(spec.compact_threshold_chars, 9999);

        // …including disabling compaction explicitly.
        let disabled = AgentDecl {
            settings: json!({ "compact_threshold_chars": 0 }),
            ..base.clone()
        };
        let spec = agent_spec_from_decl(&disabled, "m", PathBuf::from("."), &system)
            .await
            .unwrap();
        assert_eq!(spec.compact_threshold_chars, 0);
    }

    #[tokio::test]
    async fn agent_spec_injects_inline_context_blocks() {
        // A-19: `settings.context` blocks are injected into the effective system prompt as
        // <knowledge-base> sections (grounded-knowledge epic).
        let decl = AgentDecl {
            name: "a".into(),
            model: None,
            tools: vec![],
            datasources: vec![],
            description: Some("be terse".into()),
            settings: json!({
                "context": [
                    { "id": "hours", "title": "Opening hours", "body": "Mon–Fri 09:00–18:00 CET." }
                ]
            }),
        };
        let system = System::new(Workspace::new(".").unwrap());
        let spec = agent_spec_from_decl(&decl, "m", PathBuf::from("."), &system)
            .await
            .unwrap();
        assert_eq!(spec.context.len(), 1);
        let prompt = spec.effective_system_prompt();
        assert!(prompt.contains("be terse"), "persona kept: {prompt}");
        assert!(
            prompt.contains("<knowledge-base id=\"hours\" title=\"Opening hours\">"),
            "context injected: {prompt}"
        );
        assert!(prompt.contains("Mon–Fri 09:00–18:00 CET."));

        // A malformed context (not a list of blocks) is a clean, attributed error.
        let bad = AgentDecl {
            name: "b".into(),
            settings: json!({ "context": "nope" }),
            ..decl.clone()
        };
        assert!(
            agent_spec_from_decl(&bad, "m", PathBuf::from("."), &system)
                .await
                .is_err(),
            "a non-list context is an error"
        );
    }

    #[tokio::test]
    async fn agent_spec_appends_system_prompt_files() {
        // A declarative bot keeps its long persona in a file rather than inlining it (flux D-11):
        // `settings.system_prompt_files` paths are read (workspace-confined) and concatenated after the
        // base persona.
        let dir = std::env::temp_dir().join(format!("flux-persona-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("persona.md"), "PERSONA-MARKER: talk like a pirate").unwrap();

        let decl = AgentDecl {
            name: "a".into(),
            model: None,
            tools: vec![],
            datasources: vec![],
            description: Some("be terse".into()),
            settings: json!({ "system_prompt_files": ["persona.md"] }),
        };
        let system = System::new(Workspace::new(&dir).unwrap());
        let spec = agent_spec_from_decl(&decl, "m", dir.clone(), &system)
            .await
            .unwrap();
        assert!(
            spec.system_prompt.contains("be terse"),
            "the base description is kept: {}",
            spec.system_prompt
        );
        assert!(
            spec.system_prompt.contains("PERSONA-MARKER"),
            "the persona file is appended: {}",
            spec.system_prompt
        );

        // A missing file is a clean, attributed error — not a silently-empty persona.
        let bad = AgentDecl {
            name: "b".into(),
            settings: json!({ "system_prompt_files": ["nope.md"] }),
            ..decl.clone()
        };
        assert!(
            agent_spec_from_decl(&bad, "m", dir.clone(), &system)
                .await
                .is_err(),
            "an unreadable system_prompt_files path is an error"
        );

        std::fs::remove_dir_all(&dir).ok();
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

    /// C-33: an `agent`-bound trigger's turn carries its real usage and the engine's own canonical
    /// model spec onto the returned [`JourneyRun`] — the seam an operator-console surface (the
    /// `flux app run` CLI channel, `flux-channels::host::serve`) needs to render a cost annotation.
    /// The story's named failing-first test for the app-run/journey/agent-target surface.
    #[tokio::test]
    async fn agent_trigger_run_carries_usage_and_model_spec() {
        let src = "\
agent assistant
  description \"be terse\"
  tools []

trigger t1
  on \"slack\"
  run _
  agent assistant
";
        let provider: Arc<dyn Provider> = Arc::new(ReplyWithUsageProvider {
            reply: "hi back".to_string(),
        });
        let app = App::with_options(program(src), Some(provider), "mock", false);
        let runs = app
            .deliver("slack", json!({ "text": "hello", "conversation": "T1" }))
            .await
            .expect("deliver");
        assert_eq!(runs.len(), 1);
        let usage = runs[0]
            .usage
            .as_ref()
            .expect("an agent turn that reports usage must carry it onto the JourneyRun");
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 40);
        assert_eq!(
            runs[0].model, "mock",
            "the driving engine's own canonical model spec, not some other default"
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

    /// C-33: a plain journey that dispatches no model call carries no usage — but still reports the
    /// host's default model spec (the "driving engine spec" honest stand-in for a journey, which has
    /// no per-op engine of its own) so a cost-display surface has something to show *if* usage were
    /// ever present, without inventing a fake dollar figure now.
    #[tokio::test]
    async fn journey_run_carries_no_usage_but_reports_the_host_default_model() {
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
        assert!(
            runs[0].usage.is_none(),
            "a pure-op journey drives no model call, so there's no usage to report"
        );
        assert_eq!(runs[0].model, "mock");
    }

    #[tokio::test]
    async fn scheduled_agent_turn_receives_event_context() {
        // An event carrying no user `text` (a `startup` / schedule tick, vs. a Slack mention) must still
        // wake the agent with a concrete turn that names the firing trigger and carries the payload (e.g.
        // the tick's `at`), so a monitor can branch startup-vs-tick and read the time (flux D-11). The
        // echo provider surfaces the exact turn input as the reply.
        let src = "\
agent monitor
  description \"watch\"
  tools []

trigger tick
  on \"schedule\"
  run _
  agent monitor
";
        let provider: Arc<dyn Provider> = Arc::new(EchoProvider);
        let app = App::with_options(program(src), Some(provider), "mock", false);
        let runs = app
            .deliver("schedule", json!({ "at": "2026-06-30T12:00:00Z" }))
            .await
            .expect("deliver");
        assert_eq!(runs.len(), 1);
        let reply = &runs[0].result;
        assert!(
            reply.contains("schedule"),
            "the turn names the firing trigger/event: {reply}"
        );
        assert!(
            reply.contains("2026-06-30T12:00:00Z"),
            "the turn carries the schedule `at`: {reply}"
        );
    }
}
