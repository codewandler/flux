//! Flux's single turn engine. An authored Flux-Lang program owns the outer control loop while typed
//! model stages use provider-native operation calls for intent, evidence, action proposals, repair,
//! and presentation. Models never author executable Flux.
//!
//! The engine reuses [`crate::AgentSink`] so a surface (CLI/TUI) can drive it with the same sink.
//! Every op still executes through `Executor::dispatch` — there is no new bypass surface.

use std::sync::Arc;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use flux_core::{Chunk, ContentBlock, Error, Message, Result, Usage};
use flux_events::EventStore;
use flux_provider::{Effort, Provider, Request};
use flux_runtime::{
    scope_runtime_turn, Executor, RuntimeTurnContext, SpawnTaskSupervisor, TurnIdentity,
    SPAWN_CLEANUP_GRACE,
};

use crate::ast::{DraftAst, Node, NodeId};
use crate::composites::DynamicComposites;
use crate::model::StageOptions;
use crate::registry::OpRegistry;
use crate::runtime::{
    execute_flow_traced, execute_flow_with_composites, resume_flow_with_composites,
};
use crate::state::FlowStore;
use crate::AgentSink;
use flux_lang::runtime::FlowOutcome;

/// Default bound for the authored decision/batch repeat in the shipped agent loop.
pub const DEFAULT_AGENT_LOOP_ITERATIONS: usize = 50;

/// Maximum configurable bound for an authored agent loop.
///
/// The shipped loop lowers its repeat into a durable top-level state machine by cloning the loop
/// body once per iteration. Keeping this at twenty times the normal default permits unusually long
/// turns without allowing configuration input to drive unbounded startup work.
pub const MAX_AGENT_LOOP_ITERATIONS: usize = 1_000;

/// A shipped agent-loop preset. Presets are ordinary Flux-Lang programs selected explicitly by an
/// agent definition; the host does not probe the workspace for an implicit override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BuiltinAgentLoop {
    /// Intent routing, native-schema exploration, explicit action-batch approval/execution, and
    /// grounded presentation.
    #[default]
    Adaptive,
}

/// The outer control program for an agent turn.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentLoopSpec {
    /// One of Flux's shipped, versioned loop presets.
    Builtin(BuiltinAgentLoop),
    /// A caller-supplied, already parsed Flux-Lang program.
    Flux(DraftAst),
}

impl Default for AgentLoopSpec {
    fn default() -> Self {
        Self::Builtin(BuiltinAgentLoop::Adaptive)
    }
}

impl AgentLoopSpec {
    /// Parse an explicit selector: `adaptive` chooses the shipped preset; every other value is
    /// parsed as Flux-Lang source. File IO belongs to the calling surface and is intentionally not
    /// hidden inside this constructor.
    pub fn parse(selector_or_source: &str) -> Result<Self> {
        if selector_or_source.trim().eq_ignore_ascii_case("adaptive") {
            return Ok(Self::default());
        }
        flux_lang::parse::parse(selector_or_source)
            .map(Self::Flux)
            .map_err(|error| Error::Other(format!("invalid explicit agent loop: {error}")))
    }
}

/// C-87: the hard cap on distinct sessions the per-session `sticky_groups` map retains on a
/// long-lived shared engine. Generous — realistic interleaving stays well under it, so the
/// cross-session cache-stability invariant holds — while still turning unbounded growth into a bound.
const MAX_STICKY_SESSIONS: usize = 1024;

/// flux-flow's turn engine: a provider, the tool executor (safety envelope), the unified event store
/// (conversation + run trace + turn telemetry), and flux-flow's own value/symbol/suspension store.
pub struct FlowEngine {
    /// Shared with the loop host so model stages and `maybe_compact` use one provider instance.
    pub provider: Arc<dyn Provider>,
    /// Shared with the loop host so all stage and action dispatches use one safety envelope.
    pub executor: Arc<Executor>,
    pub events: Arc<EventStore>,
    /// Shared (`Arc`) with the loop host: inner runs bind symbols / trace into the same session store.
    pub flow: Arc<FlowStore>,
    /// The agent loop itself, written in Flux-Lang. The bootstrap runs this each turn.
    pub agent_loop: DraftAst,
    /// The installed authored-loop host; `set_turn` points it at the current session + sink.
    pub loop_host: Arc<crate::loop_host::EngineLoopHost>,
    /// Dynamic composite ops loaded from global/project stores or registered by this agent.
    pub composites: Arc<DynamicComposites>,
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub max_iterations: usize,
    /// Adaptive-thinking request policy for every model call made by this agent.
    pub thinking: bool,
    /// Provider-mapped reasoning effort for every model call made by this agent.
    pub effort: Option<Effort>,
    /// Skills explicitly enabled for this agent. Their bodies are injected into each turn's system
    /// prompt; discovery metadata/triggers never activate a skill implicitly.
    pub skills: Vec<flux_skill::Skill>,
    /// When the persisted session exceeds this many (serialized) chars, older turns are summarized
    /// into one synthetic message before the next request. `0` disables compaction.
    pub compact_threshold_chars: usize,
    /// Evidence-gated tool groups. Each turn the workspace is probed for signals (`detect_signals`)
    /// and only ops whose group is surfaced are advertised in the op catalog. **Empty disables
    /// gating** (every op advertised, as before surfacing existed).
    pub groups: Vec<flux_evidence::ToolGroup>,
    /// Workspace root, re-probed each turn for the surfacing signals above.
    pub cwd: std::path::PathBuf,
    /// Session-ambient signals injected by the host surface (D-115): facts the per-turn workspace
    /// walk can't see — e.g. the CLI's "the endpoints store is non-empty", computed once from its
    /// startup-loaded registry. Appended to every turn's detected signals for group surfacing;
    /// sticky-monotonic surfacing makes session-static values sufficient. Empty by default.
    ambient_signals: Vec<String>,
    /// Monotonic union of every group that has surfaced on this ENGINE. `resolve_active_groups` is
    /// stateless (it reflects only the current turn's signals), so a marker file appearing then
    /// disappearing would rewrite segment A's op catalog and miss the provider prompt cache on the
    /// whole `tools+A+phase+B` prefix (A-03). Accumulating here makes the advertised catalog grow
    /// monotonically — once a group surfaces it stays — so the cached prefix only ever stabilizes.
    /// Scope note: an engine shared across sessions (the a2a server) accumulates across them; that
    /// widens advertisement only (never grants), and the signals derive from the same host/cwd anyway.
    /// The approval/policy envelope still gates every op. Unused when `groups` is empty (gating off ⇒
    /// all ops advertised, already stable).
    /// Monotonic surfaced groups scoped by session. A shared server/A2A engine must preserve cache
    /// stability within one conversation without leaking an integration catalog into another.
    sticky_groups:
        std::sync::Mutex<std::collections::HashMap<String, std::collections::HashSet<String>>>,
    /// How many in-memory evidence observations have been flushed to the event store so far — the
    /// per-turn watermark [`flush_observations`](Self::flush_observations) advances (C-14). The
    /// executor's log is append-only and shared across this engine's turns, so a plain high-water
    /// mark attributes each tail to the turn that just ended.
    evidence_flushed: std::sync::atomic::AtomicUsize,
    /// One active public turn per engine. Nested authored operations stay inside the already-held
    /// lifecycle and call the runtime directly, so they never recursively acquire this gate.
    turn_gate: tokio::sync::Mutex<()>,
}

enum TurnProgram<'a> {
    Adaptive,
    Authored(&'a DraftAst),
    Resume {
        flow_name: Option<String>,
        body: Vec<Node>,
        node: NodeId,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TurnProgramKind {
    Adaptive,
    Authored,
    Resume,
}

impl TurnProgram<'_> {
    fn kind(&self) -> TurnProgramKind {
        match self {
            Self::Adaptive => TurnProgramKind::Adaptive,
            Self::Authored(_) => TurnProgramKind::Authored,
            Self::Resume { .. } => TurnProgramKind::Resume,
        }
    }

    fn label<'a>(&'a self, user_input: Option<&'a str>) -> &'a str {
        match self {
            Self::Authored(flow) => flow.name.as_deref().unwrap_or("(flow start)"),
            Self::Adaptive | Self::Resume { .. } => user_input.unwrap_or_default(),
        }
    }
}

struct SuspensionWrite {
    flow_name: Option<String>,
    body: Vec<Node>,
    node: NodeId,
    source: String,
}

struct TurnTerminal {
    outcome: &'static str,
    steps: u32,
    answer: String,
    cancelled: bool,
    consume_checkpoint: bool,
    suspension: Option<SuspensionWrite>,
}

struct TurnLifecycle {
    accounting: TurnAccounting,
    channel: Arc<std::sync::Mutex<dyn AgentSink>>,
    receiver: tokio::sync::mpsc::UnboundedReceiver<crate::loop_host::SinkEvent>,
    runtime: RuntimeTurnContext,
    spawn_supervisor: Arc<SpawnTaskSupervisor>,
}

struct TurnAccounting {
    turn_id: i64,
    iteration_base: usize,
    subagent_base: usize,
}

impl FlowEngine {
    /// Assemble an engine: wrap the store/provider in `Arc`, install the adaptive
    /// [`EngineLoopHost`], and load the built-in `agent-loop.flux`. This is the one place the
    /// executor⇄host cycle is tied — a plain struct literal cannot express it. Custom loops are
    /// selected explicitly through [`Self::assemble_with_loop`].
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        provider: Arc<dyn Provider>,
        executor: Executor,
        events: Arc<EventStore>,
        flow: FlowStore,
        model: String,
        system_prompt: String,
        max_tokens: u32,
        max_iterations: usize,
        skills: Vec<flux_skill::Skill>,
        compact_threshold_chars: usize,
        groups: Vec<flux_evidence::ToolGroup>,
        cwd: std::path::PathBuf,
    ) -> Result<Self> {
        Self::assemble_with_loop(
            provider,
            executor,
            events,
            flow,
            model,
            system_prompt,
            max_tokens,
            max_iterations,
            skills,
            compact_threshold_chars,
            groups,
            cwd,
            AgentLoopSpec::default(),
        )
    }

    /// Assemble an engine with an explicit authored outer loop.
    #[allow(clippy::too_many_arguments)]
    pub fn assemble_with_loop(
        provider: Arc<dyn Provider>,
        executor: Executor,
        events: Arc<EventStore>,
        flow: FlowStore,
        model: String,
        system_prompt: String,
        max_tokens: u32,
        max_iterations: usize,
        skills: Vec<flux_skill::Skill>,
        compact_threshold_chars: usize,
        groups: Vec<flux_evidence::ToolGroup>,
        cwd: std::path::PathBuf,
        agent_loop: AgentLoopSpec,
    ) -> Result<Self> {
        let flow = Arc::new(flow);
        let composites = Arc::new(DynamicComposites::load(
            executor.context().system().as_ref(),
        )?);
        let opts = StageOptions {
            max_tokens,
            ..Default::default()
        };
        // A throwaway initial session/sink; `set_turn` points the host at the real ones each turn.
        let init_sink: Arc<std::sync::Mutex<dyn AgentSink>> =
            Arc::new(std::sync::Mutex::new(NullSink));
        let (executor, loop_host) = crate::loop_host::EngineLoopHost::install(
            executor,
            provider.clone(),
            model.clone(),
            Some(system_prompt.clone()),
            flow.clone(),
            composites.clone(),
            String::new(),
            init_sink,
            opts,
        );
        loop_host.set_groups(groups.clone());
        // The loop machinery is engine control flow, not a user action. Proposed effects are still
        // captured, approved as a batch, and dispatched individually through the executor.
        executor.allow(&[
            "detect_intent",
            "explore",
            "approve_batch",
            "execute_batch",
            "present_results",
            "ai_segment",
            "observe",
            "evidence",
            "metrics",
        ]);
        composites.validate_base(executor.registry())?;
        let agent_loop = load_agent_loop_with_iterations(agent_loop, max_iterations)?;
        validate_agent_loop(
            &agent_loop,
            executor.registry(),
            composites.active_for_session(""),
        )?;
        Ok(FlowEngine {
            provider,
            executor,
            events,
            flow,
            agent_loop,
            loop_host,
            composites,
            model,
            system_prompt,
            max_tokens,
            max_iterations,
            thinking: false,
            effort: None,
            skills,
            compact_threshold_chars,
            groups,
            cwd,
            ambient_signals: Vec::new(),
            sticky_groups: std::sync::Mutex::new(std::collections::HashMap::new()),
            evidence_flushed: std::sync::atomic::AtomicUsize::new(0),
            turn_gate: tokio::sync::Mutex::new(()),
        })
    }

    /// Inject session-ambient group-surfacing signals (D-115): host-known facts the per-turn
    /// workspace walk can't see (e.g. the CLI injects `endpoint` when its startup-loaded
    /// endpoints store is non-empty). They join every turn's probed signals; surfacing is
    /// sticky-monotonic, so values computed once at startup are enough.
    pub fn with_ambient_signals(mut self, signals: Vec<String>) -> Self {
        self.ambient_signals = signals;
        self
    }

    /// Opt into model-invoked progressive skill disclosure (D-188): `catalog` is every discovered
    /// skill eligible for on-demand loading (already filtered to exclude
    /// `disable-model-invocation: true` skills by the caller). A non-empty catalog is what makes the
    /// engine surface a compact name+description listing in the system prompt and advertise
    /// `skill.load`; an empty catalog (the default — nobody calls this) leaves both absent, so the
    /// manual-only default path stays byte-identical. Stored on the long-lived loop host (not this
    /// engine) so the `skill.load` op — dispatched through `ToolContext`, not through `self` — can
    /// read the same catalog.
    pub fn with_model_invoked_skills(self, catalog: Vec<flux_skill::Skill>) -> Self {
        self.loop_host.set_skill_catalog(catalog);
        self
    }

    /// Apply one reasoning policy to the full agent call graph: intent, exploration, repair,
    /// presentation, authored model stages, and compaction calls.
    pub fn with_reasoning(mut self, thinking: bool, effort: Option<Effort>) -> Self {
        self.thinking = thinking;
        self.effort = effort;
        self.loop_host.set_reasoning(thinking, effort);
        self
    }

    /// Update the reasoning effort for every model call this agent makes, mid-session. Takes effect
    /// from the next turn; effort is ephemeral session state (not persisted like the model name).
    pub fn set_effort(&mut self, effort: Option<Effort>) {
        self.effort = effort;
        self.loop_host.set_reasoning(self.thinking, effort);
    }

    /// Atomically switch the live model and the session's durable model attribution. Persistence
    /// happens first; if it fails, the in-memory engine remains unchanged.
    pub fn switch_model_for_session(
        &mut self,
        session_id: &str,
        provider: Arc<dyn Provider>,
        model: String,
    ) -> Result<()> {
        self.events.set_model(session_id, &model)?;
        self.loop_host.set_model(provider.clone(), model.clone());
        self.provider = provider;
        self.model = model;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_turn_lifecycle(
        &self,
        session_id: &str,
        label: &str,
        user_message: Option<&str>,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
        identity: &TurnIdentity,
        scope_override: Option<Arc<crate::cassette::CassetteScope>>,
    ) -> Result<TurnLifecycle> {
        let skill_input = user_message.unwrap_or_default();
        // The cache boundary precedes every execution flavor, including a persisted continuation.
        self.executor.begin_cache_turn();
        if let Some(message) = user_message {
            self.events
                .record_message(session_id, &Message::user_text(message))?;
        }
        let turn_id = self
            .events
            .begin_turn(
                session_id,
                label,
                &flux_core::canonical_model_spec(Some(self.provider.name()), &self.model),
            )
            .unwrap_or(-1);
        self.executor.observe(flux_evidence::Observation::new(
            "turn.identity",
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "caller": identity.caller().principal.id.as_str(),
                "source": identity.caller().source.as_str(),
                "trust": identity.trust(),
            }),
        ));
        let iteration_base = self.evidence_kind_count("turn.iteration");
        let subagent_base = self.evidence_kind_count("subagent.usage");
        let base_system = self.base_system_with_skills(session_id, skill_input, sink);
        let advertised = self.surfaced_for_turn(session_id, skill_input, sink);
        let (sender, receiver) =
            tokio::sync::mpsc::unbounded_channel::<crate::loop_host::SinkEvent>();
        let channel: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
            crate::loop_host::ChannelSink::new(sender),
        ));
        let activity = self.loop_host.set_turn(
            session_id.to_string(),
            Some(base_system),
            channel.clone(),
            Some(advertised),
            Some((self.events.clone(), turn_id)),
        );
        let spawn_supervisor = Arc::new(SpawnTaskSupervisor::with_cancel(cancel.child_token()));
        let runtime = RuntimeTurnContext::new()
            .with_cancel(cancel.clone())
            .with_session(session_id)
            .with_spawn_activity_sink(activity)
            .with_spawn_supervisor(spawn_supervisor.clone())
            .with_identity(identity.clone());
        // D-175: `run_turn_pinned` supplies the WHOLE cassette scope for this turn — explicitly
        // WINNING over `FLUX_CASSETTE=0`. The kill switch governs only whether an ordinary turn
        // defaults to `Record` capture; it says nothing about a caller that deliberately pinned a
        // `Frozen`/`Resume`/`Replay` scope (Tune/Resurrect/Test Kit dispatch through it, kill switch
        // or not — there would be nothing to serve from otherwise). This install is the ONLY code
        // this story swaps; every other line of the turn lifecycle is unchanged.
        match scope_override {
            Some(scope) => self.flow.set_cassette(Some(scope)),
            None if crate::cassette::enabled() => {
                self.flow
                    .set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Record(
                        crate::cassette::RecordScope::new(self.events.clone(), session_id),
                    ))));
            }
            None => self.flow.set_cassette(None),
        }
        Ok(TurnLifecycle {
            accounting: TurnAccounting {
                turn_id,
                iteration_base,
                subagent_base,
            },
            channel,
            receiver,
            runtime,
            spawn_supervisor,
        })
    }

    async fn race_turn<F>(
        lifecycle: TurnLifecycle,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
        future: F,
    ) -> (Option<Result<FlowOutcome>>, TurnAccounting)
    where
        F: std::future::Future<Output = Result<FlowOutcome>>,
    {
        let TurnLifecycle {
            accounting,
            channel: _,
            mut receiver,
            runtime,
            spawn_supervisor,
        } = lifecycle;
        let reveal = show_loop();
        let outcome = scope_runtime_turn(runtime, async {
            tokio::pin!(future);
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break None,
                    event = receiver.recv() => {
                        if let Some(event) = event {
                            drain_event(event, sink, reveal);
                        }
                    }
                    result = &mut future => break Some(result),
                }
            }
        })
        .await;
        // `task` runs its spawner in a turn-owned Tokio task. Dropping an operation branch detaches
        // its JoinHandle, so every terminal path retains the turn until children have observed the
        // supervisor token and finalized; abort is only the bounded backstop.
        spawn_supervisor.shutdown(SPAWN_CLEANUP_GRACE).await;
        while let Ok(event) = receiver.try_recv() {
            drain_event(event, sink, reveal);
        }
        (outcome, accounting)
    }

    fn finish_turn_lifecycle(
        &self,
        session_id: &str,
        sink: &mut dyn AgentSink,
        accounting: &TurnAccounting,
        mut terminal: TurnTerminal,
    ) -> Result<()> {
        let persistence = if let Some(suspension) = terminal.suspension.take() {
            self.flow.save_suspension(
                session_id,
                suspension.flow_name.as_deref(),
                &suspension.body,
                suspension.node,
                &suspension.source,
            )
        } else if terminal.consume_checkpoint {
            self.flow.clear_suspension(session_id)
        } else {
            Ok(())
        };
        if let Err(error) = persistence {
            terminal.outcome = "error";
            terminal.cancelled = false;
            terminal.answer = format!(
                "The turn finished, but its continuation checkpoint could not be persisted — {error}"
            );
        }
        sink.text_delta(&terminal.answer);
        let usage =
            self.record_resume_usage(session_id, accounting.turn_id, accounting.subagent_base);
        let _ = self.events.end_turn(
            session_id,
            accounting.turn_id,
            terminal.outcome,
            terminal.steps,
            &terminal.answer,
            usage.clone(),
        );
        self.finish_turn(
            session_id,
            accounting.turn_id,
            sink,
            &terminal.answer,
            terminal.cancelled,
            usage,
        )
    }

    /// Run one user turn to completion, uninterruptible.
    pub async fn run_turn(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
    ) -> Result<()> {
        self.run_turn_cancellable(session_id, user_input, sink, &CancellationToken::new())
            .await
    }

    /// Run one user turn, abortable via `cancel`: the token races the whole agent-loop future in the
    /// `select!` below, so a Ctrl-C mid-op returns at once and drops the in-flight op (aborting its IO)
    /// rather than waiting for it to finish. Every termination path persists exactly one assistant
    /// message, so the session stays a valid `user → assistant` alternation.
    pub async fn run_turn_cancellable(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let _turn = self.turn_gate.lock().await;
        let identity = self.executor.identity().snapshot();
        self.run_turn_locked(session_id, user_input, sink, cancel, identity, None)
            .await
    }

    /// Run one user turn against a PINNED cassette scope instead of the default per-turn `Record`
    /// capture — the one primitive Tune (D-176) and Resurrect (D-178) both build on. Mirrors
    /// [`Self::run_turn`]'s body exactly (turn-gate lock, identity snapshot, a fresh
    /// [`CancellationToken`]) and swaps ONLY the cassette install in `begin_turn_lifecycle` — there is
    /// no parallel turn path (no-fallbacks rule): this still runs through the SAME `run_turn_locked` →
    /// `run_turn_lifecycle` → `begin_turn_lifecycle` chain every other turn entry point uses.
    ///
    /// `scope` explicitly WINS over `FLUX_CASSETTE=0`: the kill switch governs only whether an
    /// ordinary turn defaults to `Record` capture, never an explicitly pinned scope — a caller who
    /// pins `Frozen`/`Resume`/`Replay` needs it installed regardless, or there is nothing to serve
    /// dispatches from.
    pub async fn run_turn_pinned(
        &self,
        session_id: &str,
        user_input: &str,
        scope: Arc<crate::cassette::CassetteScope>,
        sink: &mut dyn AgentSink,
    ) -> Result<()> {
        let _turn = self.turn_gate.lock().await;
        let identity = self.executor.identity().snapshot();
        self.run_turn_locked(
            session_id,
            user_input,
            sink,
            &CancellationToken::new(),
            identity,
            Some(scope),
        )
        .await
    }

    /// Run one user turn under an explicit immutable caller identity. The identity is installed
    /// only after this engine acquires its single-active-turn gate and remains lexical to the turn.
    pub async fn run_turn_as(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
        identity: TurnIdentity,
    ) -> Result<()> {
        self.run_turn_cancellable_as(
            session_id,
            user_input,
            sink,
            &CancellationToken::new(),
            identity,
        )
        .await
    }

    /// Cancellable counterpart to [`Self::run_turn_as`].
    pub async fn run_turn_cancellable_as(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
        identity: TurnIdentity,
    ) -> Result<()> {
        let _turn = self.turn_gate.lock().await;
        self.run_turn_locked(session_id, user_input, sink, cancel, identity, None)
            .await
    }

    /// Run one user turn after the engine-level single-active-turn gate has been acquired.
    /// `scope_override` is `Some` only from [`Self::run_turn_pinned`]; every other caller passes
    /// `None` and gets the default per-turn `Record`/kill-switch behavior unchanged.
    async fn run_turn_locked(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
        identity: TurnIdentity,
        scope_override: Option<Arc<crate::cassette::CassetteScope>>,
    ) -> Result<()> {
        let program = match self.flow.load_suspension(session_id)? {
            Some((flow_name, body, node, _source)) => TurnProgram::Resume {
                flow_name,
                body,
                node,
            },
            None => TurnProgram::Adaptive,
        };
        self.run_turn_lifecycle(
            session_id,
            Some(user_input),
            program,
            sink,
            cancel,
            identity,
            scope_override,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_turn_lifecycle(
        &self,
        session_id: &str,
        user_input: Option<&str>,
        program: TurnProgram<'_>,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
        identity: TurnIdentity,
        scope_override: Option<Arc<crate::cassette::CassetteScope>>,
    ) -> Result<()> {
        let lifecycle = self.begin_turn_lifecycle(
            session_id,
            program.label(user_input),
            user_input,
            sink,
            cancel,
            &identity,
            scope_override,
        )?;
        let future = self.execute_turn_program(
            session_id,
            user_input.unwrap_or_default(),
            &program,
            lifecycle.channel.clone(),
            cancel,
        );
        let (outcome, accounting) = Self::race_turn(lifecycle, sink, cancel, future).await;
        let terminal = self.turn_terminal(&program, outcome, accounting.iteration_base);
        self.finish_turn_lifecycle(session_id, sink, &accounting, terminal)
    }

    async fn execute_turn_program(
        &self,
        session_id: &str,
        user_input: &str,
        program: &TurnProgram<'_>,
        channel: Arc<std::sync::Mutex<dyn AgentSink>>,
        cancel: &CancellationToken,
    ) -> Result<FlowOutcome> {
        let mut output = crate::loop_host::SharedSink::new(channel);
        match program {
            TurnProgram::Adaptive => {
                let (compaction, usage) = self
                    .compaction_attempt(session_id, &mut output, cancel)
                    .await;
                if let Some(usage) = usage {
                    self.loop_host
                        .record_external_call(self.provider.name(), &self.model, usage);
                }
                if let Err(error) = compaction {
                    return Err(Error::Other(format!("compaction: {}", model_error(&error))));
                }
                execute_flow_traced(
                    &self.flow,
                    &self.executor,
                    session_id,
                    &self.agent_loop,
                    &mut output,
                    trace_loop(),
                )
                .await
                .map_err(|error| Error::Other(error.to_string()))
            }
            TurnProgram::Authored(flow) => {
                self.composites
                    .ensure_session_loaded(&self.flow, session_id)?;
                let composites = self.composites.active_for_session(session_id);
                execute_flow_with_composites(
                    &self.flow,
                    &self.executor,
                    session_id,
                    flow,
                    &composites,
                    &mut output,
                )
                .await
                .map_err(|error| Error::Other(error.to_string()))
            }
            TurnProgram::Resume {
                flow_name,
                body,
                node,
            } => {
                self.composites
                    .ensure_session_loaded(&self.flow, session_id)?;
                let composites = self.composites.active_for_session(session_id);
                resume_flow_with_composites(
                    &self.flow,
                    &self.executor,
                    session_id,
                    flow_name.as_deref(),
                    body,
                    *node,
                    flux_lang::ast::Value::String(user_input.to_string()),
                    &composites,
                    &mut output,
                )
                .await
                .map_err(|error| Error::Other(error.to_string()))
            }
        }
    }

    fn turn_terminal(
        &self,
        program: &TurnProgram<'_>,
        outcome: Option<Result<FlowOutcome>>,
        iteration_base: usize,
    ) -> TurnTerminal {
        let kind = program.kind();
        let Some(outcome) = outcome else {
            return TurnTerminal {
                outcome: "cancelled",
                steps: 0,
                answer: "(turn cancelled)".into(),
                cancelled: true,
                consume_checkpoint: false,
                suspension: None,
            };
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                let error = error.to_string();
                let answer = match kind {
                    TurnProgramKind::Adaptive if error.starts_with("compaction: ") => format!(
                        "I couldn't compact the conversation before continuing — {}",
                        error.trim_start_matches("compaction: ")
                    ),
                    TurnProgramKind::Adaptive => {
                        format!("I couldn't complete the turn — {error}")
                    }
                    TurnProgramKind::Authored => {
                        format!("The flow failed to start — {error}")
                    }
                    TurnProgramKind::Resume => {
                        format!("The resumed flow failed — {error}")
                    }
                };
                return TurnTerminal {
                    outcome: "error",
                    steps: 0,
                    answer,
                    cancelled: false,
                    consume_checkpoint: false,
                    suspension: None,
                };
            }
        };

        if let Some(suspension) = &outcome.suspension {
            let (flow_name, body) = match program {
                TurnProgram::Adaptive => {
                    (self.agent_loop.name.clone(), self.agent_loop.body.clone())
                }
                TurnProgram::Authored(flow) => (flow.name.clone(), flow.body.clone()),
                TurnProgram::Resume {
                    flow_name, body, ..
                } => (flow_name.clone(), body.clone()),
            };
            let steps = if kind == TurnProgramKind::Adaptive {
                self.evidence_kind_count("turn.iteration")
                    .saturating_sub(iteration_base) as u32
            } else {
                outcome.steps as u32
            };
            return TurnTerminal {
                outcome: "suspended",
                steps,
                answer: suspension_prompt(&outcome),
                cancelled: false,
                consume_checkpoint: false,
                suspension: Some(SuspensionWrite {
                    flow_name,
                    body,
                    node: suspension.node,
                    source: suspension.source.clone(),
                }),
            };
        }

        match kind {
            TurnProgramKind::Adaptive => {
                let answer = outcome.result.trim().to_string();
                let (answer, status) = if answer.is_empty() {
                    (
                        format!(
                            "Reached the maximum of {} adaptive iterations for this turn; stopping.",
                            self.max_iterations
                        ),
                        "max_iter",
                    )
                } else {
                    (answer, "ok")
                };
                TurnTerminal {
                    outcome: status,
                    steps: self
                        .evidence_kind_count("turn.iteration")
                        .saturating_sub(iteration_base) as u32,
                    answer,
                    cancelled: false,
                    consume_checkpoint: false,
                    suspension: None,
                }
            }
            TurnProgramKind::Authored => TurnTerminal {
                outcome: "completed",
                steps: outcome.steps as u32,
                answer: if outcome.result.trim().is_empty() {
                    format!("Flow completed ({} step(s)).", outcome.steps)
                } else {
                    outcome.result.trim().to_string()
                },
                cancelled: false,
                consume_checkpoint: false,
                suspension: None,
            },
            TurnProgramKind::Resume => TurnTerminal {
                outcome: "resumed",
                steps: outcome.steps as u32,
                answer: if outcome.result.trim().is_empty() {
                    format!("Resumed and completed ({} step(s)).", outcome.steps)
                } else {
                    outcome.result.trim().to_string()
                },
                cancelled: false,
                consume_checkpoint: true,
                suspension: None,
            },
        }
    }

    fn surfaced_for_turn(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
    ) -> std::collections::HashSet<String> {
        // C-87: bound the per-session sticky-group map so a long-lived engine shared across
        // conversations (the A2A server) can't accumulate one entry per session forever. Unlike the
        // composite caches this has no durable reload path — the accumulated groups ARE the
        // cache-stability state — so a session's entry must survive OTHER sessions' turns (an invariant
        // the shared-engine tests pin). We therefore keep a generous cap and only evict OTHER sessions'
        // entries once it is exceeded, never the active one. Serialised by `turn_gate`.
        {
            let mut sticky = self.sticky_groups.lock().unwrap();
            if sticky.len() > MAX_STICKY_SESSIONS {
                let overflow = sticky.len() - MAX_STICKY_SESSIONS;
                let victims: Vec<String> = sticky
                    .keys()
                    .filter(|k| k.as_str() != session_id)
                    .take(overflow)
                    .cloned()
                    .collect();
                for victim in victims {
                    sticky.remove(&victim);
                }
            }
        }
        // C-100: probe the executor context's ACTIVE root, not the assembly-time `cwd` — after a
        // worktree transition (`git_worktree_enter`), evidence signals present only in the
        // worktree (e.g. a worktree-local Cargo.toml) must surface their groups. Assembly-time
        // config/skills/roles loading deliberately stays fixed to `self.cwd` (entering a worktree
        // changes the working directory, not the agent's authority).
        let active_system = self.executor.context().system();
        let (advertised, surfaced) = surfaced_op_names(
            self.executor.registry(),
            &self.groups,
            active_system.workspace().root(),
            &self.sticky_groups,
            session_id,
            &self.ambient_signals,
            user_input,
        );
        if let Some(surfaced) = surfaced.as_ref() {
            self.record_active_groups(surfaced, sink);
        }
        self.narrow_by_skill_catalog(self.narrow_by_skill_allowed_tools(advertised))
    }

    /// Suppress `skill.load` from the advertised set unless the opt-in model-invoked skill catalog
    /// (D-188) is non-empty for this engine. `skill.load` is unconditionally registered (so
    /// `Executor::dispatch` can find and clearly reject a stray call), but must never appear in the
    /// model-facing catalog on the default-off path — this is what keeps
    /// `skills_are_disabled_until_named_explicitly` byte-identical: no catalog is set, so
    /// `skill_catalog()` is empty, so this always removes the op.
    fn narrow_by_skill_catalog(
        &self,
        mut advertised: std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String> {
        if self.loop_host.skill_catalog().is_empty() {
            advertised.remove("skill.load");
        }
        advertised
    }

    /// Narrow this turn's advertised ops to any active skill's Claude `allowed-tools` (D-189):
    /// intersect with the union of every active skill's translated allowlist
    /// (`flux_skill::Skill::allowed_ops`). A skill with no `allowed-tools` imposes no constraint;
    /// when NO active skill declares one, `advertised` passes through unchanged. This sits on top
    /// of whatever policy/group gating already produced — the same narrowing-only spirit as a
    /// role's `tools:` allowlist — and can only shrink the surfaced set, never grow it.
    fn narrow_by_skill_allowed_tools(
        &self,
        advertised: std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String> {
        let mut allowlisted = self
            .skills
            .iter()
            .filter(|s| !s.allowed_ops.is_empty())
            .peekable();
        if allowlisted.peek().is_none() {
            return advertised;
        }
        let allowed: std::collections::HashSet<&str> = allowlisted
            .flat_map(|s| s.allowed_ops.iter().map(String::as_str))
            .collect();
        advertised
            .into_iter()
            .filter(|op| allowed.contains(op.as_str()))
            .collect()
    }

    /// Record (audit + surface) which evidence-gated groups are active this turn — and which
    /// workspace signals justified them (C-14 provenance) — so the user can see what the workspace
    /// surfaced and why. Mirrors the skill-activation observation pattern.
    fn record_active_groups(&self, surfaced: &SurfacedGroups, sink: &mut dyn AgentSink) {
        let mut names: Vec<&str> = surfaced.active.iter().map(String::as_str).collect();
        names.sort_unstable();
        let mut signals: Vec<&str> = surfaced.signals.iter().map(String::as_str).collect();
        signals.sort_unstable();
        let obs = flux_evidence::Observation::new(
            "groups.active",
            flux_evidence::Phase::Turn,
            serde_json::json!({ "groups": names, "signals": signals }),
        );
        self.executor.observe(obs.clone());
        sink.observation(&obs);
    }

    /// The agent identity + project context + explicitly enabled skills — the common base for every
    /// model-backed stage. When the opt-in model-invoked catalog (D-188) is non-empty, also injects
    /// the full body of any skill loaded via `skill.load` earlier in this session (one consistent
    /// semantics with manual `--skill` activation — see `docs/designs/claude-interop.md`) and a
    /// compact name+description listing of the rest, so the model knows what else it can load.
    fn base_system_with_skills(
        &self,
        session_id: &str,
        _user_input: &str,
        sink: &mut dyn AgentSink,
    ) -> String {
        let mut base_system = self.system_prompt.clone();
        let mut injected: std::collections::HashSet<String> = std::collections::HashSet::new();
        // C-121: the assembly-time project context above describes the ORIGINAL workspace; while a
        // worktree transition is active, tell the model per turn where its operations actually
        // land (the `cwd` op stays the live ground truth).
        if let Some(session) = self
            .executor
            .context()
            .workspace_context()
            .worktree_session()
        {
            base_system.push_str(&format!(
                "\n\n<workspace-note>\nThis context is currently transitioned into a temporary \
                 git worktree: all file, process, and toolchain operations run under {} on \
                 branch {}. Any project context above describing the working directory, git \
                 branch, or git status refers to the ORIGINAL workspace, not this worktree. Use \
                 the cwd op for the live working directory. Run git_worktree_leave to merge the \
                 committed work back and return.\n</workspace-note>",
                session.checkout.display(),
                session.branch,
            ));
        }
        for skill in &self.skills {
            self.inject_skill_tag(&mut base_system, skill, sink);
            injected.insert(skill.name.clone());
        }
        let catalog = self.loop_host.skill_catalog();
        if !catalog.is_empty() {
            let loaded = self.loop_host.loaded_skill_names(session_id);
            for skill in catalog
                .iter()
                .filter(|s| loaded.contains(&s.name) && !injected.contains(&s.name))
            {
                self.inject_skill_tag(&mut base_system, skill, sink);
            }
            base_system.push_str(&render_skill_catalog(&catalog));
        }
        base_system
    }

    /// Inject one skill's full body as a `<skill>` block and emit the `skill.activated`
    /// observation both `self.skills` (manual) and a `skill.load`-loaded catalog entry share.
    fn inject_skill_tag(
        &self,
        base_system: &mut String,
        skill: &flux_skill::Skill,
        sink: &mut dyn AgentSink,
    ) {
        let path_attr = skill_disclosed_path(skill)
            .map(|p| format!(" path=\"{}\"", p.display()))
            .unwrap_or_default();
        base_system.push_str(&format!(
            "\n\n<skill name=\"{}\"{path_attr}>\n{}\n</skill>",
            skill.name, skill.body
        ));
        let obs = flux_evidence::Observation::new(
            "skill.activated",
            flux_evidence::Phase::Turn,
            serde_json::json!({ "skill": skill.name }),
        );
        self.executor.observe(obs.clone());
        sink.observation(&obs);
    }

    /// Persist the single assistant message for this turn (keeping the `user → assistant` session
    /// shape) and end the turn. `cancelled` records the audit observation. `usage` is this turn's
    /// token tally (all model-stage calls summed), surfaced to the sink for the turn-end annotation.
    /// Both termination paths (completion + cancel) come through here, which is what makes the
    /// evidence flush below turn-complete.
    fn finish_turn(
        &self,
        session_id: &str,
        turn_id: i64,
        sink: &mut dyn AgentSink,
        answer: &str,
        cancelled: bool,
        usage: Option<Usage>,
    ) -> Result<()> {
        if cancelled {
            let obs = flux_evidence::Observation::new(
                "turn.cancelled",
                flux_evidence::Phase::Turn,
                serde_json::json!({}),
            );
            self.executor.observe(obs.clone());
            sink.observation(&obs);
        }
        // Durable evidence (C-14): flush everything the in-memory log gained since the last flush
        // to the event store, batched per turn (a crash mid-turn loses at most that turn's batch —
        // turn-granular audit is the goal, not crash forensics). The first flush starts at
        // watermark 0, so startup observations land too.
        self.flush_observations(session_id, turn_id);
        self.composites.clear_turn(session_id);
        self.flow.set_cassette(None);
        self.events.record_message(
            session_id,
            &Message::assistant(vec![ContentBlock::Text {
                text: answer.to_string(),
            }]),
        )?;
        sink.turn_end(usage);
        Ok(())
    }

    /// Persist the in-memory evidence log's tail (`[watermark..]`) as `EventKind::Observation`
    /// events on the session stream, then advance the watermark. Non-fatal — audit writes never
    /// fail a turn. The in-memory log stays the live `/evidence` read model; this is its durable
    /// mirror (`projection::observations`).
    ///
    /// C-22: the tail is scrubbed through the SAME [`Redactor`](flux_secret::Redactor) the executor
    /// uses on tool results (seeded from `resolve_secrets` per C-13) BEFORE it reaches the store —
    /// the `tool_call` observation carries raw per-token permission subjects (a `Bearer`/secret in a
    /// `bash` arg), built and pushed before dispatch redacts the model-facing result. Redacting once
    /// here, at the single flush seam, covers every observation kind without touching the live
    /// in-memory log (which is process-local and already gated behind `/evidence`).
    fn flush_observations(&self, session_id: &str, turn_id: i64) {
        let redactor = &self.executor.context().redactor;
        // C-87: snapshot ONLY the unflushed tail under the evidence lock — not a deep clone of the
        // whole (never-trimmed) log — and release the lock before the per-observation DB writes below,
        // so this never holds a std mutex across store I/O.
        let start = self
            .evidence_flushed
            .load(std::sync::atomic::Ordering::SeqCst);
        let (tail, start) = {
            let log = self.executor.context().evidence.lock().unwrap();
            let all = log.all();
            let start = start.min(all.len());
            (all[start..].to_vec(), start)
        };
        // C-24: advance the watermark only past observations whose write returned `Ok` — a transient
        // `record_observation` failure (WAL `BUSY`, disk-full) leaves the unwritten tail behind the
        // watermark to be retried next flush, instead of being lost forever behind an
        // unconditionally-advanced mark.
        let written = flush_tail(&tail, |obs| {
            let redacted = redact_observation(redactor, obs);
            self.events
                .record_observation(session_id, turn_id, &redacted)
        });
        self.evidence_flushed
            .store(start + written, std::sync::atomic::Ordering::SeqCst);
    }

    /// C-87: the number of recorded observations of a given kind, read directly under the shared
    /// evidence lock (the log field is public [`Arc<Mutex<EvidenceLog>>`]) instead of via
    /// `Executor::evidence()`, which deep-clones the entire never-trimmed log on every call. Used for
    /// the per-turn `turn.iteration` / `subagent.usage` snapshot bases, several times per turn.
    fn evidence_kind_count(&self, kind: &str) -> usize {
        self.executor
            .context()
            .evidence
            .lock()
            .unwrap()
            .by_kind(kind)
            .count()
    }

    /// This turn's token tally, as an `Option` for the sink: `None` when nothing was billed (e.g. an
    /// offline `-m mock` turn) so a surface needn't render a misleading all-zero annotation. Includes
    /// any sub-agents this turn spawned (`subagent_calls`, from [`Self::subagent_calls_since`]) — a
    /// `task` call's tokens are real spend the parent turn incurred, so its total must reflect them
    /// (C-06 sub-agent rollup).
    fn turn_usage(&self, subagent_calls: &[(String, Usage)]) -> Option<Usage> {
        let mut usage = self.loop_host.turn_usage();
        for (_, call) in subagent_calls {
            usage.sum_independent(call);
        }
        (usage.total() > 0).then_some(usage)
    }

    /// Every `subagent.usage` observation recorded since `base` (this turn's sub-agents, per the
    /// snapshot-then-diff scoping [`run_turn_cancellable`] already uses for `turn.iteration`), parsed
    /// into `(model, usage)` pairs. A malformed/missing field is skipped rather than panicking — this
    /// reads a cross-crate string-keyed contract (`flux-orchestrate`'s `TaskTool`), not a typed one.
    fn subagent_calls_since(&self, base: usize) -> Vec<(String, Usage)> {
        // C-87: collect owned `(model, usage)` pairs directly under the evidence lock — the collected
        // tuples own their data, so nothing borrows past the guard — instead of deep-cloning the whole
        // log via `Executor::evidence()`.
        self.executor
            .context()
            .evidence
            .lock()
            .unwrap()
            .by_kind("subagent.usage")
            .skip(base)
            .filter_map(|o| {
                let model = o.data.get("model")?.as_str()?.to_string();
                let usage: Usage = serde_json::from_value(o.data.get("usage")?.clone()).ok()?;
                Some((model, usage))
            })
            .collect()
    }

    /// Append one `EventKind::CallUsage` per model-stage call this turn made (`self.loop_host.turn_calls`)
    /// PLUS one per sub-agent this turn spawned (`subagent_calls`), each stamped with the model that
    /// produced it — the active stage model at the time, or the sub-agent's own resolved model —
    /// so a mid-turn `/model` switch AND a `task` delegation both attribute tokens/cost correctly
    /// (C-06). Called just before `end_turn` on every termination path (cancelled or completed) so the
    /// per-call attribution records and the turn-total `TurnEnded.usage` land together. Non-fatal like
    /// every other telemetry write here: a DB hiccup must never fail the turn — `record_call_usage`
    /// itself already no-ops on a failed (`-1`) `turn_id`.
    fn record_call_usage_events(
        &self,
        session_id: &str,
        turn_id: i64,
        subagent_calls: &[(String, Usage)],
    ) {
        // Zero-usage calls (a `mock`/free provider, or one that genuinely reported nothing) are
        // skipped — mirrors `TurnEnded.usage` staying `None` for a token-less turn, so a log doesn't
        // fill with placeholder zero entries for every offline/no-cost call.
        for (model, usage) in self.loop_host.turn_calls() {
            if usage.total() == 0 {
                continue;
            }
            let _ = self
                .events
                .record_call_usage(session_id, turn_id, &model, usage);
        }
        for (model, usage) in subagent_calls {
            if usage.total() == 0 {
                continue;
            }
            let _ = self
                .events
                .record_call_usage(session_id, turn_id, model, usage.clone());
        }
    }

    /// Start an authored flow as the session's conversation driver (D-131). Executes the flow
    /// **fresh** to its first top-level `await`, persists the suspension so every later `run_turn`
    /// routes through the shared suspension-first turn runner, and surfaces the
    /// flow's own **authored prompt** (its last emitted view) as the assistant turn — no adaptive stage is
    /// invoked for this deterministic skeleton. A flow that completes without any `await` surfaces
    /// its result as a single completed turn.
    ///
    /// Turn 1 is flow-authored, not user-authored: the flow speaks first, so no user message is
    /// recorded (the session log opens with the authored prompt). Runs over the shared
    /// `Arc<Executor>`, so the authorization → approval → guarded-IO envelope applies exactly as on
    /// the adaptive path — a `RiskApprover` gates a flow-driven op identically to a captured one.
    pub async fn start_flow_turn(
        &self,
        session_id: &str,
        flow: &DraftAst,
        sink: &mut dyn AgentSink,
    ) -> Result<()> {
        self.start_flow_turn_cancellable(session_id, flow, sink, &CancellationToken::new())
            .await
    }

    /// Start an authored flow under the engine's single-active-turn gate and cancellation context.
    pub async fn start_flow_turn_cancellable(
        &self,
        session_id: &str,
        flow: &DraftAst,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let _turn = self.turn_gate.lock().await;
        let identity = self.executor.identity().snapshot();
        self.start_flow_turn_locked(session_id, flow, sink, cancel, identity)
            .await
    }

    /// Start an authored flow under an explicit immutable caller identity.
    pub async fn start_flow_turn_as(
        &self,
        session_id: &str,
        flow: &DraftAst,
        sink: &mut dyn AgentSink,
        identity: TurnIdentity,
    ) -> Result<()> {
        self.start_flow_turn_cancellable_as(
            session_id,
            flow,
            sink,
            &CancellationToken::new(),
            identity,
        )
        .await
    }

    /// Cancellable counterpart to [`Self::start_flow_turn_as`].
    pub async fn start_flow_turn_cancellable_as(
        &self,
        session_id: &str,
        flow: &DraftAst,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
        identity: TurnIdentity,
    ) -> Result<()> {
        let _turn = self.turn_gate.lock().await;
        self.start_flow_turn_locked(session_id, flow, sink, cancel, identity)
            .await
    }

    async fn start_flow_turn_locked(
        &self,
        session_id: &str,
        flow: &DraftAst,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
        identity: TurnIdentity,
    ) -> Result<()> {
        self.run_turn_lifecycle(
            session_id,
            None,
            TurnProgram::Authored(flow),
            sink,
            cancel,
            identity,
            None,
        )
        .await
    }

    /// Record a turn's spend and return the turn total (C-26). The shared lifecycle arms (and
    /// resets) the loop host before every adaptive, authored, or resumed turn, so this folds BOTH
    /// the loop host's model-stage calls and the `task` sub-agent
    /// spend recorded since `subagent_base`, emitting one `CallUsage` per call and returning the
    /// turn aggregate. `None` when nothing billed, mirroring [`Self::turn_usage`].
    fn record_resume_usage(
        &self,
        session_id: &str,
        turn_id: i64,
        subagent_base: usize,
    ) -> Option<Usage> {
        let subagent_calls = self.subagent_calls_since(subagent_base);
        self.record_call_usage_events(session_id, turn_id, &subagent_calls);
        self.turn_usage(&subagent_calls)
    }

    /// If the session has grown past `compact_threshold_chars`, summarize everything but the most
    /// recent messages into one synthetic message and rewrite the session log. A no-op when
    /// compaction is disabled or the session is small. Snaps the boundary back so a `tool_result` is
    /// never orphaned from the `tool_use` that compaction would summarize away.
    pub async fn maybe_compact(
        &self,
        session_id: &str,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        self.compaction_attempt(session_id, sink, cancel).await.0
    }

    /// The compaction result plus usage for its optional provider call. Usage is outside the
    /// `Result`: a stream can report tokens before failing, and those
    /// tokens remain billable. `None` means no summary request was needed; `Some(default())` means a
    /// request was attempted but the provider reported no usage.
    async fn compaction_attempt(
        &self,
        session_id: &str,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
    ) -> (Result<()>, Option<Usage>) {
        if self.compact_threshold_chars == 0 {
            return (Ok(()), None);
        }
        let messages = match self.events.conversation(session_id) {
            Ok(messages) => messages,
            Err(error) => return (Err(error), None),
        };
        if messages.len() < 4 {
            return (Ok(()), None);
        }
        let total: usize = messages
            .iter()
            .map(|m| serde_json::to_string(m).map(|s| s.len()).unwrap_or(0))
            .sum();
        if total <= self.compact_threshold_chars {
            return (Ok(()), None);
        }

        let keep = 2.min(messages.len());
        let mut split = messages.len() - keep;
        while split > 0 && has_tool_result(&messages[split]) {
            split -= 1;
        }
        if split == 0 {
            return (Ok(()), None); // can't summarize without splitting a tool_use/tool_result pair
        }
        let (old, recent) = messages.split_at(split);

        let mut transcript = String::new();
        for m in old {
            let t = m.text();
            if !t.trim().is_empty() {
                transcript.push_str(t.trim());
                transcript.push('\n');
            }
        }
        let prompt = format!(
            "Summarize the earlier conversation into a compact set of durable facts, decisions, and \
             open threads. Preserve file paths, names, and numbers. Be terse.\n\n{transcript}"
        );
        let req = Request::new(self.model.clone(), prompt)
            .with_max_tokens(1024)
            .with_thinking(self.thinking);
        let req = match self.effort {
            Some(effort) => req.with_effort(effort),
            None => req,
        };
        let mut usage = Usage::default();
        let mut stream = match self.provider.stream(req).await {
            Ok(stream) => stream,
            Err(error) => return (Err(error), Some(usage)),
        };
        let mut summary = String::new();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return (Ok(()), Some(usage)),
                chunk = stream.next() => {
                    let Some(chunk) = chunk else { break };
                    match chunk {
                        Ok(Chunk::TextDelta(text)) => summary.push_str(&text),
                        // Provider usage chunks are cumulative within one call: last wins.
                        Ok(Chunk::Usage(call_usage)) => usage = call_usage,
                        Ok(_) => {}
                        Err(error) => return (Err(error), Some(usage)),
                    }
                }
            }
        }
        if summary.trim().is_empty() {
            return (Ok(()), Some(usage));
        }

        let mut new_msgs = vec![Message::user_text(format!(
            "[summary of earlier conversation]\n{}",
            summary.trim()
        ))];
        new_msgs.extend(recent.iter().cloned());
        let to = new_msgs.len();
        if let Err(error) = self.events.record_compaction(session_id, &new_msgs) {
            return (Err(error), Some(usage));
        }

        let obs = flux_evidence::Observation::new(
            "context.compacted",
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "from_messages": messages.len(),
                "to_messages": to,
                "approx_chars_before": total,
            }),
        );
        self.executor.observe(obs.clone());
        sink.observation(&obs);
        (Ok(()), Some(usage))
    }
}

/// Persist a `tail` of observations via `record`, returning how many were durably written (C-24).
/// The caller advances the flush watermark by exactly this count, so a failed write leaves its
/// observation (and everything after it) behind the watermark for the next flush to retry.
fn flush_tail(
    tail: &[flux_evidence::Observation],
    mut record: impl FnMut(&flux_evidence::Observation) -> Result<()>,
) -> usize {
    let mut written = 0;
    for obs in tail {
        // Stop at the FIRST failed write: everything from here stays behind the watermark and is
        // retried next flush. Advancing past a failed write is exactly the lost-observation bug.
        if record(obs).is_err() {
            break;
        }
        written += 1;
    }
    written
}

/// Return a redacted copy of `obs` — its `data` scrubbed of any registered/credential-shaped
/// secret (C-22). Only the JSON's string leaves are rewritten; keys and structure are preserved so
/// the persisted observation still folds through `projection::observations` unchanged in shape.
pub(crate) fn redact_observation(
    redactor: &flux_secret::Redactor,
    obs: &flux_evidence::Observation,
) -> flux_evidence::Observation {
    let mut out = obs.clone();
    redact_json_strings(redactor, &mut out.data);
    out
}

/// Recursively rewrite every string leaf of `value` through the redactor (in place).
fn redact_json_strings(redactor: &flux_secret::Redactor, value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => *s = redactor.redact(s),
        serde_json::Value::Array(items) => {
            for item in items {
                redact_json_strings(redactor, item);
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                redact_json_strings(redactor, v);
            }
        }
        _ => {}
    }
}

/// The location disclosed to the model for a skill's supporting files (D-190). A `SKILL.md`-backed
/// skill discloses its **directory** — `references/*.md`, scripts, and templates all resolve
/// relative to it — while a flat `.md` skill discloses the file itself (it has no sibling files to
/// anchor). `None` when the skill carries no `source` (e.g. constructed in-memory via the SDK):
/// there is no real location to disclose, and injecting a fabricated one would mislead a `read`.
///
/// This is disclosure only — it grants no read access. The path is read (or not) through the
/// normal `read` op, which still runs the standard authorization/approval flow; a project-local
/// skill's supporting files are reachable exactly because they already sit inside the workspace
/// jail, not because of anything this function does.
fn skill_disclosed_path(skill: &flux_skill::Skill) -> Option<std::path::PathBuf> {
    let source = skill.source.as_ref()?;
    if source.file_name().map(|n| n == "SKILL.md").unwrap_or(false) {
        source.parent().map(std::path::Path::to_path_buf)
    } else {
        Some(source.clone())
    }
}

/// D-188: render the compact `<available-skills>` catalog block — name, optional disclosed path
/// (D-190), and description only. Bodies are deliberately absent; the model pulls one into context
/// with `skill.load(name)` when it decides a skill applies. Listed unconditionally (including
/// already-loaded ones) — re-listing costs a line, and `skill.load` is idempotent, so there's no
/// correctness reason to track and exclude them here.
fn render_skill_catalog(catalog: &[flux_skill::Skill]) -> String {
    let mut listing = String::from(
        "\n\n<available-skills>\nOther skills discovered in this workspace. Only their name and \
         description are shown — call `skill.load` with a skill's exact `name` to pull its full \
         instructions into context before following it.\n",
    );
    for skill in catalog {
        let path_attr = skill_disclosed_path(skill)
            .map(|p| format!(" ({})", p.display()))
            .unwrap_or_default();
        listing.push_str(&format!(
            "- {}{path_attr}: {}\n",
            skill.name, skill.description
        ));
    }
    listing.push_str("</available-skills>");
    listing
}

/// True if a message carries a tool_result block (a `user` message answering tool calls).
fn has_tool_result(msg: &Message) -> bool {
    msg.content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
}

/// Where the active agent loop came from — what [`agent_loop_source`] resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopSource {
    /// The compiled-in default (`assets/agent-loop.flux`).
    Builtin,
}

/// The advertised op-name set for a turn, plus the active group set when gating is on (`None` when
/// the manifest is empty = gating disabled). The ONE computation both catalog paths share — the
/// engine's preview registries and the loop host's model stages — so they can't drift.
///
/// Gating disabled still excludes the never-surfaced adaptive machinery (`reflect` group). With a
/// manifest, `advertised_op_names` already excludes it.
pub(crate) fn surfaced_op_names(
    reg: &flux_runtime::ToolRegistry,
    groups: &[flux_evidence::ToolGroup],
    cwd: &std::path::Path,
    sticky: &std::sync::Mutex<std::collections::HashMap<String, std::collections::HashSet<String>>>,
    session_id: &str,
    ambient: &[String],
    user_input: &str,
) -> (std::collections::HashSet<String>, Option<SurfacedGroups>) {
    if groups.is_empty() {
        let advertised = reg
            .specs()
            .iter()
            .filter(|s| s.group.as_deref() != Some(flux_runtime::REFLECT_GROUP))
            .map(|s| s.name.clone())
            .collect();
        return (advertised, None);
    }
    let mut signals = flux_runtime::detect_signals(cwd);
    // Session-ambient signals (D-115): host-known facts the workspace walk can't see — e.g. the
    // CLI's "the endpoints store is non-empty", computed once from its startup-loaded registry.
    // They join the probed signals and gate groups identically.
    signals.extend(
        ambient
            .iter()
            .map(|s| flux_evidence::Observation::signal(s)),
    );
    signals.extend(flux_evidence::turn_intent_observations(groups, user_input));
    let active = flux_evidence::resolve_active_groups(groups, &signals);
    // Monotonic surfacing (A-03 cache stability): fold this turn's active groups into the session's
    // session-local sticky union and advertise from the ACCUMULATED set. `resolve_active_groups` is
    // stateless, so a
    // marker file appearing then disappearing would otherwise rewrite segment A's op catalog and miss
    // the cached `tools+A+phase+B` prefix; accumulating means the catalog only ever grows and the
    // prefix restabilizes. Advertising is not granting — the approval/policy envelope still gates ops.
    let accumulated = {
        let mut sessions = sticky.lock().unwrap_or_else(|e| e.into_inner());
        let session = sessions.entry(session_id.to_string()).or_default();
        session.extend(active);
        session.clone()
    };
    let advertised = flux_runtime::advertised_op_names(&reg.specs(), groups, &accumulated);
    // Keep the signal NAMES alongside the resolved groups — the `groups.active` observation
    // records both, so the audit trail says not just which groups surfaced but WHY (C-14).
    let signal_names = signals
        .iter()
        .filter_map(|o| o.data.get("signal").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect();
    (
        advertised,
        Some(SurfacedGroups {
            // The CUMULATIVE set, so the `groups.active` audit matches what's actually advertised
            // this turn; `signals` above still reflects what fired THIS turn (the provenance).
            active: accumulated,
            signals: signal_names,
        }),
    )
}

/// The resolved evidence-gated groups for a turn plus the workspace signals that justified them —
/// the signal→group provenance the `groups.active` observation records (C-14).
pub(crate) struct SurfacedGroups {
    pub active: std::collections::HashSet<String>,
    pub signals: Vec<String>,
}

/// The compiled-in default agent loop, as readable Flux-Lang text, surfaced by `flux loop show`.
pub fn builtin_agent_loop() -> &'static str {
    include_str!("../assets/agent-loop.flux")
}

/// Return the built-in loop source. Kept as the `flux loop show` inspection seam; workspace files no
/// longer override an agent implicitly.
pub fn agent_loop_source(cwd: &std::path::Path) -> (LoopSource, String) {
    let _ = cwd;
    (LoopSource::Builtin, builtin_agent_loop().to_string())
}

/// Parse the built-in adaptive agent loop. Custom loops enter through [`AgentLoopSpec::Flux`], not a
/// magic filesystem probe.
pub fn load_agent_loop(cwd: &std::path::Path) -> Result<DraftAst> {
    let (_, src) = agent_loop_source(cwd);
    // The loop is written in readable Flux-Lang text (it round-trips through `format`/`parse`), so parse
    // it through the language surface rather than the JSON wire form.
    flux_lang::parse::parse(&src)
        .map_err(|e| flux_core::Error::Other(format!("agent-loop.flux: invalid flow: {e}")))
}

/// Load the selected agent loop and replace its execute-pass `repeat … until
/// $done` budget with the engine setting. Flux-Lang repeat bounds are literals by design, so this is
/// configuration of the parsed program, not a second Rust turn loop. Custom loops without that
/// conventional execute pass remain byte-for-byte as authored.
fn load_agent_loop_with_iterations(spec: AgentLoopSpec, max_iterations: usize) -> Result<DraftAst> {
    if max_iterations == 0 {
        return Err(flux_core::Error::Other(
            "max_iterations must be greater than zero".into(),
        ));
    }
    if max_iterations > MAX_AGENT_LOOP_ITERATIONS {
        return Err(flux_core::Error::Other(format!(
            "max_iterations {max_iterations} exceeds the maximum of \
             {MAX_AGENT_LOOP_ITERATIONS} (the built-in agent loop expands once per iteration)"
        )));
    }
    // Bounded to `MAX_AGENT_LOOP_ITERATIONS` above, so narrowing to Flux-Lang's `u32` repeat bound
    // is always lossless.
    let max = max_iterations as u32;
    let builtin = matches!(spec, AgentLoopSpec::Builtin(BuiltinAgentLoop::Adaptive));
    let mut ast = match spec {
        AgentLoopSpec::Builtin(BuiltinAgentLoop::Adaptive) => {
            flux_lang::parse::parse(builtin_agent_loop()).map_err(|error| {
                flux_core::Error::Other(format!("built-in adaptive loop is invalid: {error}"))
            })?
        }
        AgentLoopSpec::Flux(ast) => ast,
    };
    for node in &mut ast.body {
        if let crate::ast::Node::Repeat {
            max: repeat_max,
            until: Some(until),
            ..
        } = node
        {
            if matches!(until.as_ref(), crate::ast::Node::Var { name } if name.0 == "done") {
                *repeat_max = max;
            }
        }
    }
    if builtin {
        ast = lift_builtin_repeat_awaits(ast)?;
    }
    Ok(ast)
}

/// Compile the built-in loop's bounded `repeat` with a direct-child `await` into an equivalent
/// finite top-level state machine. Flux-Lang's durable cursor is a top-level index; lifting the
/// bounded copies keeps that cursor stable across process restart without adding another parking
/// mechanism or teaching the runtime to serialize an async interpreter stack.
fn lift_builtin_repeat_awaits(mut ast: DraftAst) -> Result<DraftAst> {
    let mut lowered = Vec::new();
    let mut repeat_index = 0usize;
    for node in std::mem::take(&mut ast.body) {
        let crate::ast::Node::Repeat {
            max,
            until,
            body,
            collect,
        } = node
        else {
            lowered.push(node);
            continue;
        };
        if !body
            .iter()
            .any(|node| matches!(node, crate::ast::Node::Await { .. }))
        {
            lowered.push(crate::ast::Node::Repeat {
                max,
                until,
                body,
                collect,
            });
            continue;
        }
        if collect.is_some() {
            return Err(Error::Other(
                "the built-in repeatable-decision loop cannot collect iteration values".into(),
            ));
        }
        let until = until.ok_or_else(|| {
            Error::Other("the built-in repeatable-decision loop must have an `until` guard".into())
        })?;
        if !matches!(
            until.as_ref(),
            crate::ast::Node::Var { .. } | crate::ast::Node::Lit { .. }
        ) {
            return Err(Error::Other(
                "the built-in repeatable-decision loop requires a literal or variable `until` guard"
                    .into(),
            ));
        }
        let active = crate::ast::SymbolName(format!("__adaptive_repeat_{repeat_index}_active"));
        lowered.push(crate::ast::Node::Bind {
            name: active.clone(),
            value: Box::new(crate::ast::Node::Lit {
                value: serde_json::Value::Bool(true),
            }),
            ty: None,
            effect: None,
        });
        for _ in 0..max {
            for statement in &body {
                match statement {
                    crate::ast::Node::Await {
                        binding,
                        source,
                        as_type,
                        condition,
                    } => {
                        let condition = match condition.as_deref() {
                            None => crate::ast::Node::Var {
                                name: active.clone(),
                            },
                            Some(
                                condition @ (crate::ast::Node::Var { .. }
                                | crate::ast::Node::Lit { .. }),
                            ) => crate::ast::Node::Expr {
                                formula: "active && requested".into(),
                                vars: [
                                    (
                                        "active".into(),
                                        Box::new(crate::ast::Node::Var {
                                            name: active.clone(),
                                        }),
                                    ),
                                    ("requested".into(), Box::new(condition.clone())),
                                ]
                                .into_iter()
                                .collect(),
                            },
                            Some(_) => {
                                return Err(Error::Other(
                                    "the built-in repeatable decision requires a literal or variable await condition"
                                        .into(),
                                ));
                            }
                        };
                        lowered.push(crate::ast::Node::Await {
                            binding: binding.clone(),
                            source: source.clone(),
                            as_type: as_type.clone(),
                            condition: Some(Box::new(condition)),
                        });
                    }
                    statement => lowered.push(crate::ast::Node::When {
                        cond: Box::new(crate::ast::Node::Var {
                            name: active.clone(),
                        }),
                        then: vec![statement.clone()],
                        otherwise: Vec::new(),
                    }),
                }
            }
            lowered.push(crate::ast::Node::Bind {
                name: active.clone(),
                value: Box::new(crate::ast::Node::Expr {
                    formula: "active && !stop".into(),
                    vars: [
                        (
                            "active".into(),
                            Box::new(crate::ast::Node::Var {
                                name: active.clone(),
                            }),
                        ),
                        ("stop".into(), Box::new(until.as_ref().clone())),
                    ]
                    .into_iter()
                    .collect(),
                }),
                ty: None,
                effect: None,
            });
        }
        repeat_index += 1;
    }
    ast.body = lowered;
    Ok(ast)
}

fn validate_agent_loop(
    ast: &DraftAst,
    tools: &flux_runtime::ToolRegistry,
    composites: Vec<flux_lang::program::CompositeOpDecl>,
) -> Result<()> {
    let catalog = OpRegistry::new(tools).with_owned_composites(composites);
    flux_lang::analyze::analyze_flow(ast, &catalog, &std::collections::HashSet::new()).map_err(
        |diagnostics| {
            Error::Other(format!(
                "agent loop failed validation: {}",
                diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.message)
                    .collect::<Vec<_>>()
                    .join("; ")
            ))
        },
    )
}

/// The loop-machinery ops a turn dispatches to *drive* the loop (not to do the user's work). Their
/// tool-call/result events are filtered out of the user-facing sink so the surface shows the actual
/// gathered and executed operations, not the plumbing.
const MACHINERY_OPS: &[&str] = &[
    "detect_intent",
    "explore",
    "approve_batch",
    "execute_batch",
    "present_results",
    "ai_segment",
    "observe",
    "evidence",
    "metrics",
    "grade",
];

/// Whether an op is internal agent-loop machinery rather than user-requested work. Surfaces use
/// this when reconstructing durable traces so their historical view matches the live sink filter.
pub fn is_loop_machinery_op(name: &str) -> bool {
    MACHINERY_OPS.contains(&name)
}

/// Whether the loop-machinery ops are revealed on the surface — the CLI `--show-loop`, exported as
/// `FLUX_SHOW_LOOP` so the engine reads it without new plumbing. When set, the user watches the loop
/// move through its typed stages instead of only seeing the gathered and executed operations.
pub fn show_loop() -> bool {
    flux_system::env_truthy("FLUX_SHOW_LOOP")
}

/// Whether the OUTER agent loop's structure is traced (A-39) — the CLI `--trace-loop`, exported as
/// `FLUX_TRACE_LOOP` so the engine reads it without new plumbing. When set, [`execute_flow_traced`]
/// emits one live `loop.round`/`loop.node` observation per outer-loop round/structural node
/// executed. Scoped to the outer loop only; authored `flow run` and resume paths never see it.
pub fn trace_loop() -> bool {
    flux_system::env_truthy("FLUX_TRACE_LOOP")
}

/// Drain one captured sink event onto the real sink. By default the loop-machinery tool calls/results
/// are dropped (the surface shows real work, not plumbing); `reveal` keeps them so `--show-loop` can
/// stream the loop's own iterations.
fn drain_event(ev: crate::loop_host::SinkEvent, sink: &mut dyn AgentSink, reveal: bool) {
    use crate::loop_host::SinkEvent;
    let machinery = match &ev {
        SinkEvent::ToolCall(name, _)
        | SinkEvent::ToolTiming(name, _)
        | SinkEvent::ToolResult(name, _) => is_loop_machinery_op(name),
        _ => false,
    };
    if reveal || !machinery {
        ev.apply(sink);
    }
}

/// A sink that discards everything — the engine's initial loop-host sink, replaced by `set_turn` before
/// the first real turn.
struct NullSink;
impl AgentSink for NullSink {}

/// Render a model-stage failure for the user. A provider API error carries the raw JSON response body;
/// unwrap it to `error.message` so a credit/billing/auth/rate-limit failure reads as a plain sentence
/// instead of a JSON dump. Every other error uses its own `Display`.
pub fn model_error(e: &flux_core::Error) -> String {
    match e {
        flux_core::Error::Api { status, message } => {
            let detail = serde_json::from_str::<serde_json::Value>(message)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|err| err.get("message"))
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| message.clone());
            format!("the model provider returned an error (HTTP {status}): {detail}")
        }
        // A-33: a decode-class error that escaped a stage's own bounded retry loop — the same friendly framing as
        // the API-error case above, instead of the raw "provider stream decode error: …" Display.
        flux_core::Error::StreamDecode(detail) => format!(
            "the model provider's response broke mid-stream and could not be decoded: {detail}"
        ),
        _ => e.to_string(),
    }
}

/// The text to surface when a flow-driven session suspends on a top-level `await` (D-131): the
/// flow's own last-emitted view (`outcome.result`) — its **authored prompt** — falling back to the
/// generic hint only when the author emitted nothing before the `await`. Shared by the fresh
/// [`FlowEngine::start_flow_turn`] and the resume path so both surface the same authored text.
/// `pub(crate)`: the Resurrect driver (`resurrect.rs`, D-178) reuses this so a crash-resumed
/// suspension surfaces the identical authored prompt a live suspension would.
pub(crate) fn suspension_prompt(outcome: &FlowOutcome) -> String {
    let prompt = outcome.result.trim();
    if prompt.is_empty() {
        "(awaiting your input — reply to continue the flow)".to_string()
    } else {
        prompt.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use flux_core::{StopReason, ToolResultContent};
    use flux_provider::ChunkStream;
    use flux_runtime::{
        AllowApprover, PermissionManager, SkillLoader, Tool, ToolContext, ToolRegistry, ToolResult,
    };
    use flux_spec::{Effect, Idempotency, Risk, ToolSpec};
    use flux_system::{System, Workspace};
    use serde_json::{json, Value};

    static TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    struct ScriptedProvider {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
        requests: Arc<Mutex<Vec<Request>>>,
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn name(&self) -> &str {
            "scripted"
        }

        async fn stream(&self, request: Request) -> Result<ChunkStream> {
            self.requests.lock().unwrap().push(request);
            let chunks = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    struct PendingProvider;

    #[async_trait]
    impl Provider for PendingProvider {
        fn name(&self) -> &str {
            "pending"
        }

        async fn stream(&self, _request: Request) -> Result<ChunkStream> {
            Ok(Box::pin(futures::stream::pending()))
        }
    }

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "echo",
                "Return the supplied text.",
                json!({
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"],
                    "additionalProperties": false
                }),
            )
        }

        async fn execute(&self, _ctx: &ToolContext, input: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                input
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ))
        }
    }

    struct CountingWriteTool(Arc<AtomicU64>);

    #[async_trait]
    impl Tool for CountingWriteTool {
        fn spec(&self) -> ToolSpec {
            let mut spec = ToolSpec::read_only(
                "change",
                "Change the fixture exactly once.",
                json!({
                    "type": "object",
                    "properties": {
                        "value": {"type": "string"},
                        "fail": {"type": "boolean"}
                    },
                    "required": ["value"],
                    "additionalProperties": false
                }),
            )
            .with_effects(vec![Effect::Write, Effect::Filesystem])
            .with_access(vec![flux_spec::AccessKind::Filesystem])
            .with_risk(Risk::Medium);
            spec.idempotency = Idempotency::NonIdempotent;
            spec
        }

        fn permission_subjects(&self, _input: &Value) -> Vec<String> {
            vec!["fixture".into()]
        }

        async fn execute(&self, _ctx: &ToolContext, input: Value) -> Result<ToolResult> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if input.get("fail").and_then(Value::as_bool) == Some(true) {
                return Err(Error::Other("fixture change failed".into()));
            }
            Ok(ToolResult::ok("fixture changed"))
        }
    }

    struct FileProbeTool(Arc<AtomicUsize>);

    #[async_trait]
    impl Tool for FileProbeTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "file_probe",
                "Read the lifecycle cache fixture.",
                json!({"type": "object", "additionalProperties": false}),
            )
            .with_access(vec![flux_spec::AccessKind::Filesystem])
        }

        fn permission_subjects(&self, _input: &Value) -> Vec<String> {
            vec!["value.txt".into()]
        }

        async fn execute(&self, ctx: &ToolContext, _input: Value) -> Result<ToolResult> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult::ok(ctx.system().read_file("value.txt").await?))
        }
    }

    struct NestedAuthoredTool;

    #[async_trait]
    impl Tool for NestedAuthoredTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "nested_authored",
                "Run one authored flow through the active loop host.",
                json!({"type": "object", "additionalProperties": false}),
            )
        }

        async fn execute(&self, ctx: &ToolContext, _input: Value) -> Result<ToolResult> {
            let host = ctx
                .loop_host
                .as_ref()
                .ok_or_else(|| Error::Other("test loop host is missing".into()))?;
            let ast = DraftAst {
                body: vec![Node::Return {
                    value: Box::new(Node::Lit {
                        value: json!("nested result"),
                    }),
                }],
                ..Default::default()
            };
            let outcome = host.run_authored_flow(serde_json::to_value(ast)?).await?;
            Ok(ToolResult::ok(
                outcome
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ))
        }
    }

    struct TurnBarrier {
        entered: AtomicUsize,
        active: AtomicUsize,
        max_active: AtomicUsize,
        notify: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
    }

    impl Default for TurnBarrier {
        fn default() -> Self {
            Self {
                entered: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                notify: tokio::sync::Notify::new(),
                release: tokio::sync::Semaphore::new(0),
            }
        }
    }

    impl TurnBarrier {
        async fn wait_for_entered(&self, expected: usize) {
            loop {
                let notified = self.notify.notified();
                if self.entered.load(Ordering::SeqCst) >= expected {
                    return;
                }
                notified.await;
            }
        }

        fn release(&self, permits: usize) {
            self.release.add_permits(permits);
        }
    }

    struct ActiveCall<'a>(&'a AtomicUsize);

    impl Drop for ActiveCall<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct BlockingTurnTool(Arc<TurnBarrier>);

    #[async_trait]
    impl Tool for BlockingTurnTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "blocking_turn",
                "Hold a turn open until the test releases it.",
                json!({"type": "object", "additionalProperties": false}),
            )
        }

        async fn execute(&self, ctx: &ToolContext, _input: Value) -> Result<ToolResult> {
            let session_id = ctx.session_id().unwrap_or_default();
            let active = self.0.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.0.max_active.fetch_max(active, Ordering::SeqCst);
            let _active = ActiveCall(&self.0.active);
            let call_number = self.0.entered.fetch_add(1, Ordering::SeqCst) + 1;
            ctx.evidence
                .lock()
                .unwrap()
                .record(flux_evidence::Observation::new(
                    "test.turn_context",
                    flux_evidence::Phase::Turn,
                    json!({"session_id": session_id}),
                ));
            ctx.evidence
                .lock()
                .unwrap()
                .record(flux_evidence::Observation::new(
                    "subagent.usage",
                    flux_evidence::Phase::Turn,
                    json!({
                        "model": format!("child-{session_id}"),
                        "usage": Usage {
                            input_tokens: call_number as u64,
                            output_tokens: (call_number * 10) as u64,
                            ..Usage::default()
                        }
                    }),
                ));
            self.0.notify.notify_waiters();
            let permit = self
                .0
                .release
                .acquire()
                .await
                .map_err(|error| Error::Other(error.to_string()))?;
            permit.forget();
            Ok(ToolResult::ok(session_id))
        }
    }

    struct IdentityBlockingTool(Arc<TurnBarrier>);

    #[async_trait]
    impl Tool for IdentityBlockingTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "identity_blocking",
                "Hold a turn open and report its immutable caller identity.",
                json!({"type": "object", "additionalProperties": false}),
            )
        }

        async fn execute(&self, ctx: &ToolContext, _input: Value) -> Result<ToolResult> {
            let session_id = ctx.session_id().unwrap_or_default();
            let caller = ctx
                .turn_identity()
                .ok_or_else(|| Error::Other("test turn identity is missing".into()))?
                .caller()
                .principal
                .id
                .clone();
            let active = self.0.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.0.max_active.fetch_max(active, Ordering::SeqCst);
            let _active = ActiveCall(&self.0.active);
            self.0.entered.fetch_add(1, Ordering::SeqCst);
            ctx.evidence
                .lock()
                .unwrap()
                .record(flux_evidence::Observation::new(
                    "test.identity_context",
                    flux_evidence::Phase::Turn,
                    json!({"session_id": session_id.as_str(), "caller": caller.as_str()}),
                ));
            self.0.notify.notify_waiters();
            let permit = self
                .0
                .release
                .acquire()
                .await
                .map_err(|error| Error::Other(error.to_string()))?;
            permit.forget();
            Ok(ToolResult::ok(format!("{session_id}:{caller}")))
        }
    }

    #[derive(Default)]
    struct CollectSink {
        text: String,
        tools: Vec<String>,
        observations: Vec<flux_evidence::Observation>,
        usages: Vec<Option<Usage>>,
        ended: usize,
    }

    impl AgentSink for CollectSink {
        fn text_delta(&mut self, text: &str) {
            self.text.push_str(text);
        }

        fn tool_call(&mut self, name: &str, _input: &Value) {
            self.tools.push(name.to_string());
        }

        fn observation(&mut self, observation: &flux_evidence::Observation) {
            self.observations.push(observation.clone());
        }

        fn turn_end(&mut self, usage: Option<Usage>) {
            self.usages.push(usage);
            self.ended += 1;
        }
    }

    fn native_call(id: &str, name: &str, input: Value) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input,
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    fn native_calls(calls: Vec<(&str, &str, Value)>) -> Vec<Chunk> {
        let mut chunks = calls
            .into_iter()
            .map(|(id, name, input)| {
                Chunk::Block(ContentBlock::ToolUse {
                    id: id.into(),
                    name: name.into(),
                    input,
                })
            })
            .collect::<Vec<_>>();
        chunks.push(Chunk::Done {
            stop_reason: Some(StopReason::ToolUse),
        });
        chunks
    }

    fn prose(text: &str) -> Vec<Chunk> {
        vec![
            Chunk::TextDelta(text.into()),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ]
    }

    fn assemble_test_engine(
        provider: Arc<dyn Provider>,
        loop_spec: AgentLoopSpec,
    ) -> (Result<FlowEngine>, Arc<EventStore>) {
        let sequence = TEST_ROOT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "flux-adaptive-engine-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["echo".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap()))),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = FlowEngine::assemble_with_loop(
            provider,
            executor,
            events.clone(),
            flow,
            "test-model".into(),
            "Use only observed evidence.".into(),
            2_048,
            5,
            Vec::new(),
            0,
            Vec::new(),
            root.clone(),
            loop_spec,
        );
        (engine, events)
    }

    fn scripted_engine(
        responses: Vec<Vec<Chunk>>,
        loop_spec: AgentLoopSpec,
    ) -> (FlowEngine, Arc<EventStore>, Arc<Mutex<Vec<Request>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider {
            responses: Mutex::new(responses.into()),
            requests: requests.clone(),
        });
        let (engine, events) = assemble_test_engine(provider, loop_spec);
        (engine.unwrap(), events, requests)
    }

    type ScriptedWriteEngine = (
        FlowEngine,
        Arc<EventStore>,
        Arc<Mutex<Vec<Request>>>,
        Arc<AtomicU64>,
    );

    fn scripted_write_engine(responses: Vec<Vec<Chunk>>) -> ScriptedWriteEngine {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider {
            responses: Mutex::new(responses.into()),
            requests: requests.clone(),
        });
        let writes = Arc::new(AtomicU64::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CountingWriteTool(writes.clone())));
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let sequence = TEST_ROOT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "flux-adaptive-write-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["change".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap()))),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = FlowEngine::assemble_with_loop(
            provider,
            executor,
            events.clone(),
            flow,
            "test-model".into(),
            "Use only observed evidence.".into(),
            2_048,
            8,
            Vec::new(),
            0,
            Vec::new(),
            root,
            AgentLoopSpec::default(),
        )
        .unwrap();
        (engine, events, requests, writes)
    }

    fn tool_engine(
        tool: Arc<dyn Tool>,
        agent_loop: DraftAst,
    ) -> (FlowEngine, Arc<EventStore>, std::path::PathBuf) {
        let sequence = TEST_ROOT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "flux-turn-lifecycle-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let name = tool.spec().name;
        let mut registry = ToolRegistry::new();
        registry.register(tool);
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(std::slice::from_ref(&name), &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap()))),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = FlowEngine::assemble_with_loop(
            Arc::new(flux_provider::NullProvider),
            executor,
            events.clone(),
            flow,
            "test-model".into(),
            "Test turn isolation.".into(),
            2_048,
            5,
            Vec::new(),
            0,
            Vec::new(),
            root.clone(),
            AgentLoopSpec::Flux(agent_loop),
        )
        .unwrap();
        (engine, events, root)
    }

    fn call_node(op: &str) -> flux_lang::ast::Node {
        flux_lang::ast::Node::Call {
            op: op.into(),
            args: Vec::new(),
        }
    }

    fn blocking_agent_loop() -> DraftAst {
        DraftAst {
            body: vec![flux_lang::ast::Node::Return {
                value: Box::new(call_node("blocking_turn")),
            }],
            ..Default::default()
        }
    }

    fn identity_blocking_agent_loop() -> DraftAst {
        DraftAst {
            body: vec![flux_lang::ast::Node::Return {
                value: Box::new(call_node("identity_blocking")),
            }],
            ..Default::default()
        }
    }

    fn idle_agent_loop() -> DraftAst {
        DraftAst {
            body: vec![flux_lang::ast::Node::Return {
                value: Box::new(flux_lang::ast::Node::Lit {
                    value: json!("idle"),
                }),
            }],
            ..Default::default()
        }
    }

    fn nested_authored_agent_loop() -> DraftAst {
        DraftAst {
            body: vec![Node::Return {
                value: Box::new(call_node("nested_authored")),
            }],
            ..Default::default()
        }
    }

    fn resumable_read_flow() -> DraftAst {
        use flux_lang::ast::{Node, SymbolName};
        DraftAst {
            name: Some("cache_resume".into()),
            body: vec![
                Node::Bind {
                    name: SymbolName("before".into()),
                    value: Box::new(call_node("file_probe")),
                    ty: None,
                    effect: None,
                },
                Node::Await {
                    binding: Some(SymbolName("reply".into())),
                    source: "user_input".into(),
                    as_type: None,
                    condition: None,
                },
                Node::Bind {
                    name: SymbolName("after".into()),
                    value: Box::new(call_node("file_probe")),
                    ty: None,
                    effect: None,
                },
                Node::Return {
                    value: Box::new(Node::Var {
                        name: SymbolName("after".into()),
                    }),
                },
            ],
            ..Default::default()
        }
    }

    fn blocking_resume_flow() -> DraftAst {
        use flux_lang::ast::{Node, SymbolName};
        DraftAst {
            name: Some("cancel_resume".into()),
            body: vec![
                Node::Await {
                    binding: Some(SymbolName("reply".into())),
                    source: "user_input".into(),
                    as_type: None,
                    condition: None,
                },
                Node::Return {
                    value: Box::new(call_node("blocking_turn")),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn builtin_loop_is_authored_flux_without_the_retired_compiler_ops() {
        let source = builtin_agent_loop();
        let ast = flux_lang::parse::parse(source).expect("built-in loop parses");
        let formatted = flux_lang::format::format(&ast);
        flux_lang::parse::parse(&formatted).expect("built-in loop round-trips");

        for retired in ["emit_plan", "run_plan", "staged_plan"] {
            assert!(!source.contains(retired), "found retired op `{retired}`");
        }
        for stage in [
            "detect_intent",
            "explore",
            "approve_batch",
            "execute_batch",
            "present_results",
        ] {
            assert!(source.contains(stage), "missing adaptive stage `{stage}`");
        }
        assert!(
            source.contains(&format!("repeat {DEFAULT_AGENT_LOOP_ITERATIONS}")),
            "the inspectable built-in source must show the default outer-loop bound \
             ({DEFAULT_AGENT_LOOP_ITERATIONS}); regenerate assets/agent-loop.flux if this fails"
        );
    }

    #[test]
    fn outer_loop_iteration_bounds_reject_zero_and_values_above_the_practical_cap() {
        let zero = load_agent_loop_with_iterations(AgentLoopSpec::default(), 0)
            .unwrap_err()
            .to_string();
        assert!(zero.contains("must be greater than zero"), "{zero}");

        load_agent_loop_with_iterations(AgentLoopSpec::default(), MAX_AGENT_LOOP_ITERATIONS)
            .expect("the practical maximum remains usable");

        let above_max = load_agent_loop_with_iterations(
            AgentLoopSpec::default(),
            MAX_AGENT_LOOP_ITERATIONS + 1,
        );
        match above_max {
            Err(error) => {
                let error = error.to_string();
                assert!(
                    error.contains(&format!("maximum of {MAX_AGENT_LOOP_ITERATIONS}")),
                    "{error}"
                );
            }
            Ok(_) => panic!("a bound above the practical maximum must fail before AST expansion"),
        }
    }

    #[tokio::test]
    async fn ai_segment_honors_an_authored_50_round_budget() {
        let mut responses = (0..49)
            .map(|round| {
                native_call(
                    &format!("echo-{round}"),
                    "echo",
                    json!({"text": format!("evidence-{round}")}),
                )
            })
            .collect::<Vec<_>>();
        responses.push(prose("All 49 observations were gathered."));
        let loop_spec = AgentLoopSpec::Flux(
            flux_lang::parse::parse(
                r#"flow segment_test -> string
  $segment = ai_segment({ goal: "Gather every observation", tools: ["echo"], max_rounds: 50 })
  return $segment.result
"#,
            )
            .expect("test loop parses"),
        );
        let (engine, events, requests) = scripted_engine(responses, loop_spec);
        let session = events.create_session("scripted/test-model").unwrap();
        let mut sink = CollectSink::default();

        engine
            .run_turn(&session, "Run the segment", &mut sink)
            .await
            .unwrap();

        assert_eq!(sink.text, "All 49 observations were gathered.");
        assert_eq!(requests.lock().unwrap().len(), 50);
    }

    #[tokio::test]
    async fn default_turn_routes_intent_then_uses_exact_native_tool_schema() {
        let (engine, events, requests) = scripted_engine(
            vec![
                native_call(
                    "intent-1",
                    "declare_intent",
                    json!({
                        "intent": "echo a grounded value",
                        "capability_families": ["core"]
                    }),
                ),
                native_call("echo-1", "echo", json!({"text": "evidence"})),
                prose("The observed value is evidence."),
            ],
            AgentLoopSpec::default(),
        );
        let session = events.create_session("scripted/test-model").unwrap();
        let mut sink = CollectSink::default();

        engine
            .run_turn(&session, "Echo a grounded value", &mut sink)
            .await
            .unwrap();

        assert_eq!(sink.text, "The observed value is evidence.");
        assert_eq!(sink.tools, vec!["echo"]);
        assert_eq!(sink.ended, 1);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[0]
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["declare_intent"]
        );
        assert_eq!(
            requests[1]
                .tools
                .iter()
                .find(|tool| tool.name == "echo")
                .expect("selected native op")
                .input_schema,
            json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            })
        );
        assert!(requests
            .iter()
            .flat_map(|request| &request.tools)
            .all(|tool| tool.name != "emit_plan"));
        let feedback = requests[2]
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(content),
                _ => None,
            })
            .flatten()
            .filter_map(|content| match content {
                ToolResultContent::Text { text } => Some(text.as_str()),
                ToolResultContent::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(feedback.contains("evidence"));
        assert_eq!(events.conversation(&session).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn authored_decision_suspends_and_resumes_the_same_native_ledger() {
        let (engine, events, requests) = scripted_engine(
            vec![
                native_call(
                    "intent-1",
                    "declare_intent",
                    json!({
                        "intent": "choose a fixture",
                        "capability_families": []
                    }),
                ),
                native_call(
                    "decision-1",
                    "request_decision",
                    json!({
                        "prompt": "Which fixture should I use?",
                        "options": ["alpha", "beta"]
                    }),
                ),
                prose("I will use alpha."),
            ],
            AgentLoopSpec::default(),
        );
        let session = events.create_session("scripted/test-model").unwrap();
        let mut sink = CollectSink::default();

        engine
            .run_turn(&session, "Choose a fixture", &mut sink)
            .await
            .unwrap();
        assert!(sink.text.contains("Which fixture should I use?"));
        assert!(sink.text.contains("1. alpha"));

        engine.run_turn(&session, "alpha", &mut sink).await.unwrap();
        assert!(sink.text.ends_with("I will use alpha."));
        assert_eq!(sink.ended, 2);
        assert_eq!(events.conversation(&session).unwrap().len(), 4);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3, "resume must not re-run intent detection");
        let resumed_context = requests[2]
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(content),
                _ => None,
            })
            .flatten()
            .filter_map(|content| match content {
                ToolResultContent::Text { text } => Some(text.as_str()),
                ToolResultContent::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(resumed_context.contains("alpha"));
    }

    #[tokio::test]
    async fn every_adaptive_decision_suspends_and_resumes_the_same_native_ledger() {
        let (engine, events, requests) = scripted_engine(
            vec![
                native_call(
                    "intent-1",
                    "declare_intent",
                    json!({
                        "intent": "choose two fixtures",
                        "capability_families": []
                    }),
                ),
                native_call(
                    "decision-1",
                    "request_decision",
                    json!({
                        "prompt": "Which primary fixture should I use?",
                        "options": ["alpha", "beta"]
                    }),
                ),
                native_call(
                    "decision-2",
                    "request_decision",
                    json!({
                        "prompt": "Which fallback fixture should I use?",
                        "options": ["gamma", "delta"]
                    }),
                ),
                prose("I will use alpha with gamma as the fallback."),
            ],
            AgentLoopSpec::default(),
        );
        let session = events.create_session("scripted/test-model").unwrap();
        let mut sink = CollectSink::default();

        engine
            .run_turn(&session, "Choose two fixtures", &mut sink)
            .await
            .unwrap();
        assert!(sink.text.contains("Which primary fixture should I use?"));

        engine.run_turn(&session, "alpha", &mut sink).await.unwrap();
        assert!(sink.text.contains("Which fallback fixture should I use?"));

        engine.run_turn(&session, "gamma", &mut sink).await.unwrap();
        assert!(sink
            .text
            .ends_with("I will use alpha with gamma as the fallback."));
        assert_eq!(sink.ended, 3);
        assert_eq!(events.conversation(&session).unwrap().len(), 6);
        assert_eq!(
            requests.lock().unwrap().len(),
            4,
            "each resume must continue the native ledger without rerunning intent"
        );
    }

    #[tokio::test]
    async fn decision_after_execution_resumes_without_replaying_the_completed_action() {
        let responses = vec![
            native_call(
                "intent-1",
                "declare_intent",
                json!({
                    "intent": "change a fixture and choose the follow-up",
                    "capability_families": ["workspace.write"]
                }),
            ),
            native_call("change-1", "change", json!({"value": "updated"})),
            native_call(
                "finalize-1",
                "finalize_plan",
                json!({"instructions": "Report the completed change after resolving the follow-up."}),
            ),
            native_call(
                "decision-after-execution",
                "request_decision",
                json!({
                    "prompt": "Which follow-up should I report?",
                    "options": ["summary", "details"]
                }),
            ),
            prose("The fixture changed once; here is the summary."),
        ];
        let (engine, events, requests, writes) = scripted_write_engine(responses);
        let session = events.create_session("scripted/test-model").unwrap();
        let mut sink = CollectSink::default();

        engine
            .run_turn(&session, "Change the fixture", &mut sink)
            .await
            .unwrap();
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "sink text={:?}, tools={:?}, conversation={:?}, requests={}",
            sink.text,
            sink.tools,
            events.conversation(&session).unwrap(),
            requests.lock().unwrap().len(),
        );
        assert!(sink.text.contains("Which follow-up should I report?"));
        assert!(
            sink.text.contains("1. summary"),
            "sink text={:?}",
            sink.text
        );
        let approved = sink
            .observations
            .iter()
            .find(|observation| observation.kind == "approval.approved")
            .expect("approval outcome is observable");
        assert!(approved.data["wait_us"].is_number());
        let executed = sink
            .observations
            .iter()
            .find(|observation| observation.kind == "action_batch.executed")
            .expect("batch execution is observable");
        assert!(executed.data["duration_us"].is_number());
        assert!(sink
            .observations
            .iter()
            .any(|observation| observation.kind == "model.call"
                && observation.data["stage"] == "explore"));

        engine
            .run_turn(&session, "summary", &mut sink)
            .await
            .unwrap();
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "resuming a post-execution decision must not replay the consumed batch"
        );
        assert!(sink
            .text
            .ends_with("The fixture changed once; here is the summary."));
        assert_eq!(
            requests.lock().unwrap().len(),
            5,
            "resume must continue after the execution report without rerunning intent or action capture"
        );
    }

    #[tokio::test]
    async fn partial_failure_skips_later_actions_and_is_not_replayed_after_decision_resume() {
        let responses = vec![
            native_call(
                "intent-1",
                "declare_intent",
                json!({
                    "intent": "attempt two fixture changes and choose the report",
                    "capability_families": ["workspace.write"]
                }),
            ),
            native_calls(vec![
                (
                    "change-fails",
                    "change",
                    json!({"value": "first", "fail": true}),
                ),
                ("change-skipped", "change", json!({"value": "second"})),
            ]),
            native_call(
                "finalize-1",
                "finalize_plan",
                json!({"instructions": "Report the partial failure after resolving presentation."}),
            ),
            native_call(
                "decision-after-failure",
                "request_decision",
                json!({
                    "prompt": "How should I present the partial failure?",
                    "options": ["briefly", "with details"]
                }),
            ),
            prose("The first change failed, so the second was skipped."),
        ];
        let (engine, events, requests, writes) = scripted_write_engine(responses);
        let session = events.create_session("scripted/test-model").unwrap();
        let mut sink = CollectSink::default();

        engine
            .run_turn(&session, "Attempt both fixture changes", &mut sink)
            .await
            .unwrap();
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "the second action must be skipped after the first action fails"
        );
        assert!(sink.text.contains("1. briefly"));

        engine
            .run_turn(&session, "briefly", &mut sink)
            .await
            .unwrap();
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "resuming after a partial failure must not replay either the failed or skipped action"
        );
        assert!(sink
            .text
            .ends_with("The first change failed, so the second was skipped."));
        assert_eq!(requests.lock().unwrap().len(), 5);
    }

    #[test]
    fn surfaced_groups_do_not_leak_between_sessions_on_a_shared_engine() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        let groups = vec![flux_evidence::ToolGroup {
            name: "plugin.slack".into(),
            description: "Company chat operations.".into(),
            tools: vec!["echo".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: flux_evidence::KIND_TURN_INTENT.into(),
                signal: Some("slack".into()),
            }],
        }];
        let root = std::env::temp_dir();
        let sticky = std::sync::Mutex::new(std::collections::HashMap::new());

        let (first, _) = surfaced_op_names(
            &registry,
            &groups,
            &root,
            &sticky,
            "session-a",
            &[],
            "use slack",
        );
        assert!(first.contains("echo"));

        let (second, _) = surfaced_op_names(
            &registry,
            &groups,
            &root,
            &sticky,
            "session-b",
            &[],
            "say hello",
        );
        assert!(
            !second.contains("echo"),
            "a different session must not inherit another session's surfaced integration"
        );
    }

    /// D-189: a skill with `allowed-tools` narrows the turn's surfaced ops to the union of every
    /// active skill's translated allowlist — an out-of-allowlist op is not offered, even though
    /// policy/group gating alone would have surfaced it.
    #[test]
    fn skill_allowed_tools_narrows_the_advertised_op_set() {
        let (engine, _events) =
            assemble_test_engine(Arc::new(PendingProvider), AgentLoopSpec::default());
        let mut engine = engine.unwrap();
        engine.skills = vec![flux_skill::Skill {
            name: "reviewer".into(),
            description: String::new(),
            triggers: Vec::new(),
            body: "body".into(),
            format: flux_skill::SkillFormat::AgentSkills,
            source: None,
            allowed_ops: vec!["read".to_string()],
            model: None,
            disable_model_invocation: false,
            argument_hint: String::new(),
            agent_triggerable: false,
        }];

        let advertised: std::collections::HashSet<String> = ["echo", "read", "bash"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let narrowed = engine.narrow_by_skill_allowed_tools(advertised);

        assert_eq!(
            narrowed,
            std::collections::HashSet::from(["read".to_string()]),
            "only the allowlisted op survives narrowing"
        );
    }

    /// No active skill declares `allowed-tools` → the advertised set passes through unchanged
    /// (narrowing never engages without an explicit allowlist).
    #[test]
    fn no_skill_allowlist_leaves_the_advertised_op_set_unchanged() {
        let (engine, _events) =
            assemble_test_engine(Arc::new(PendingProvider), AgentLoopSpec::default());
        let mut engine = engine.unwrap();
        engine.skills = vec![flux_skill::Skill {
            name: "plain".into(),
            description: String::new(),
            triggers: Vec::new(),
            body: "body".into(),
            format: flux_skill::SkillFormat::AgentSkills,
            source: None,
            allowed_ops: Vec::new(),
            model: None,
            disable_model_invocation: false,
            argument_hint: String::new(),
            agent_triggerable: false,
        }];

        let advertised: std::collections::HashSet<String> = ["echo", "read", "bash"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let narrowed = engine.narrow_by_skill_allowed_tools(advertised.clone());

        assert_eq!(narrowed, advertised);
    }

    /// D-188 default-off invariant: an engine that never calls `with_model_invoked_skills` has an
    /// empty catalog, so `skill.load` is narrowed back out of the advertised set even though
    /// nothing else would exclude it, and the system prompt gets no `<available-skills>` block —
    /// the manual-only default stays byte-identical.
    #[test]
    fn no_model_invoked_catalog_hides_skill_load_and_the_catalog_block() {
        let (engine, _events) =
            assemble_test_engine(Arc::new(PendingProvider), AgentLoopSpec::default());
        let engine = engine.unwrap();

        let advertised: std::collections::HashSet<String> = ["echo", "skill.load"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let narrowed = engine.narrow_by_skill_catalog(advertised);
        assert_eq!(
            narrowed,
            std::collections::HashSet::from(["echo".to_string()]),
            "skill.load must not be advertised when no catalog is installed"
        );

        let mut sink = CollectSink::default();
        let rendered = engine.base_system_with_skills("session", "hello", &mut sink);
        assert!(
            !rendered.contains("<available-skills>"),
            "no catalog block should appear by default: {rendered}"
        );
    }

    /// D-188 surfacing: a non-empty opt-in catalog (1) keeps `skill.load` in the advertised set and
    /// (2) renders a compact `<available-skills>` listing with every catalog skill's name and
    /// description — but never its body, so the token cost stays proportional to the summary, not
    /// every skill's full content.
    #[test]
    fn model_invoked_catalog_surfaces_names_and_descriptions_without_bodies() {
        let (engine, _events) =
            assemble_test_engine(Arc::new(PendingProvider), AgentLoopSpec::default());
        let engine = engine
            .unwrap()
            .with_model_invoked_skills(vec![skill_fixture(
                "pdf-extract",
                "Extract text and tables from PDF files",
                "SECRET FULL BODY NOT YET REQUESTED",
                None,
            )]);

        let advertised: std::collections::HashSet<String> =
            ["echo"].iter().map(|s| s.to_string()).collect();
        let narrowed = engine.narrow_by_skill_catalog(advertised);
        assert!(
            !narrowed.contains("skill.load"),
            "narrow_by_skill_catalog only ever removes; skill.load wasn't in the starting set"
        );

        let mut sink = CollectSink::default();
        let rendered = engine.base_system_with_skills("session", "hello", &mut sink);
        assert!(
            rendered.contains("<available-skills>"),
            "expected the catalog block: {rendered}"
        );
        assert!(
            rendered.contains("pdf-extract")
                && rendered.contains("Extract text and tables from PDF files"),
            "expected name+description in the catalog: {rendered}"
        );
        assert!(
            !rendered.contains("SECRET FULL BODY NOT YET REQUESTED"),
            "the body must not be injected until skill.load is called: {rendered}"
        );
    }

    /// D-188 on-demand load + persistence: loading a catalog skill for a session (the
    /// `SkillLoader` capability `skill.load` delegates to) makes its full body appear in that
    /// session's system prompt on a LATER call — the same one-consistent-semantics treatment as an
    /// explicitly `--skill`-activated skill — while a different session sees no body at all.
    #[tokio::test]
    async fn loaded_skill_persists_its_body_for_the_loading_session_only() {
        let (engine, _events) =
            assemble_test_engine(Arc::new(PendingProvider), AgentLoopSpec::default());
        let engine = engine
            .unwrap()
            .with_model_invoked_skills(vec![skill_fixture(
                "pdf-extract",
                "Extract text and tables from PDF files",
                "full pdf-extract instructions",
                None,
            )]);

        let outcome = engine
            .loop_host
            .load_skill("session-a", "pdf-extract")
            .await
            .expect("known catalog skill loads");
        assert_eq!(outcome.body, "full pdf-extract instructions");

        let mut sink = CollectSink::default();
        let loaded_session = engine.base_system_with_skills("session-a", "hello", &mut sink);
        assert!(
            loaded_session.contains("full pdf-extract instructions"),
            "the loaded skill's body must be injected on a later call in the same session: \
             {loaded_session}"
        );

        let other_session = engine.base_system_with_skills("session-b", "hello", &mut sink);
        assert!(
            !other_session.contains("full pdf-extract instructions"),
            "loading is per-session, not global: {other_session}"
        );
    }

    /// D-188 unknown-skill error: `skill.load` on a name outside the catalog (including one that
    /// exists but declared `disable-model-invocation: true`, since the caller never puts those in
    /// the catalog) fails clearly instead of silently no-op'ing.
    #[tokio::test]
    async fn loading_an_unknown_skill_fails_clearly() {
        let (engine, _events) =
            assemble_test_engine(Arc::new(PendingProvider), AgentLoopSpec::default());
        let engine = engine.unwrap();
        let error = engine
            .loop_host
            .load_skill("session-a", "nope")
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("nope"),
            "expected the unknown name in the error: {error}"
        );
    }

    /// C-100: the per-turn surfacing probe follows the executor context's ACTIVE root, not the
    /// engine's assembly-time `cwd`. Assemble over a root without a `rust` marker, transition the
    /// context's `WorkspaceContext` into a worktree that has a `Cargo.toml`, and the gated group
    /// must surface on the next turn probe.
    #[test]
    fn per_turn_surfacing_probe_follows_the_transitioned_root() {
        let sequence = TEST_ROOT.fetch_add(1, Ordering::SeqCst);
        let origin = std::env::temp_dir().join(format!(
            "flux-c100-origin-{}-{sequence}",
            std::process::id()
        ));
        let worktree = std::env::temp_dir().join(format!(
            "flux-c100-worktree-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        // The signal exists ONLY in the worktree.
        std::fs::write(worktree.join("Cargo.toml"), "[package]\nname = \"probe\"\n").unwrap();

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let groups = vec![flux_evidence::ToolGroup {
            name: "toolchain.rust".into(),
            description: "Rust toolchain operations.".into(),
            tools: vec!["echo".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: flux_evidence::KIND_SIGNAL.into(),
                signal: Some("rust".into()),
            }],
        }];
        let system = Arc::new(System::new(Workspace::new(&origin).unwrap()));
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["echo".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system.clone()),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider {
            responses: Mutex::new(Vec::<Vec<Chunk>>::new().into()),
            requests: Arc::new(Mutex::new(Vec::new())),
        });
        let engine = FlowEngine::assemble_with_loop(
            provider,
            executor,
            events,
            flow,
            "test-model".into(),
            "Use only observed evidence.".into(),
            2_048,
            5,
            Vec::new(),
            0,
            groups,
            origin.clone(),
            AgentLoopSpec::default(),
        )
        .unwrap();

        let mut sink = CollectSink::default();
        let before = engine.surfaced_for_turn("session-wt", "hello", &mut sink);
        assert!(
            !before.contains("echo"),
            "the rust-gated group must not surface from the origin root (no Cargo.toml)"
        );

        // Transition the executor context into the worktree (as `git_worktree_enter` does).
        let active = engine.executor.context().system();
        let rerooted = Arc::new(active.rerooted(&worktree).unwrap());
        engine
            .executor
            .context()
            .workspace_context()
            .enter_worktree(
                flux_runtime::WorktreeSession {
                    original: active,
                    base_commit: "deadbeef".into(),
                    branch: "flux/worktree/test".into(),
                    checkout: worktree.clone(),
                    parent_dir: worktree.clone(),
                    phase: flux_runtime::WorktreePhase::Active,
                },
                rerooted,
            )
            .unwrap();

        let after = engine.surfaced_for_turn("session-wt", "hello", &mut sink);
        assert!(
            after.contains("echo"),
            "after the worktree transition the per-turn probe must detect the worktree-local Cargo.toml"
        );
        assert_eq!(
            engine.cwd, origin,
            "the assembly-time cwd stays fixed — only the surfacing probe follows the active root"
        );
    }

    /// C-121: while a worktree session is active, every turn's base system carries a
    /// `<workspace-note>` naming the transitioned root (the assembly-time project context still
    /// describes the original workspace); the note disappears after leave.
    #[test]
    fn base_system_carries_a_worktree_note_only_while_transitioned() {
        let sequence = TEST_ROOT.fetch_add(1, Ordering::SeqCst);
        let origin = std::env::temp_dir().join(format!(
            "flux-c121-origin-{}-{sequence}",
            std::process::id()
        ));
        let worktree = std::env::temp_dir().join(format!(
            "flux-c121-worktree-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let system = Arc::new(System::new(Workspace::new(&origin).unwrap()));
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["echo".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system.clone()),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider {
            responses: Mutex::new(Vec::<Vec<Chunk>>::new().into()),
            requests: Arc::new(Mutex::new(Vec::new())),
        });
        let engine = FlowEngine::assemble_with_loop(
            provider,
            executor,
            events,
            flow,
            "test-model".into(),
            "Use only observed evidence.".into(),
            2_048,
            5,
            Vec::new(),
            0,
            Vec::new(),
            origin.clone(),
            AgentLoopSpec::default(),
        )
        .unwrap();

        let mut sink = CollectSink::default();
        let before = engine.base_system_with_skills("session-c118", "hello", &mut sink);
        assert!(
            !before.contains("<workspace-note>"),
            "no note without an active worktree session"
        );

        let active = engine.executor.context().system();
        let rerooted = Arc::new(active.rerooted(&worktree).unwrap());
        engine
            .executor
            .context()
            .workspace_context()
            .enter_worktree(
                flux_runtime::WorktreeSession {
                    original: active,
                    base_commit: "deadbeef".into(),
                    branch: "flux/worktree/note-test".into(),
                    checkout: worktree.clone(),
                    parent_dir: worktree.clone(),
                    phase: flux_runtime::WorktreePhase::Active,
                },
                rerooted,
            )
            .unwrap();

        let during = engine.base_system_with_skills("session-c118", "hello", &mut sink);
        assert!(during.contains("<workspace-note>"));
        assert!(
            during.contains(&worktree.display().to_string()),
            "the note names the transitioned root"
        );
        assert!(during.contains("flux/worktree/note-test"));

        engine
            .executor
            .context()
            .workspace_context()
            .leave_worktree()
            .unwrap();
        let after = engine.base_system_with_skills("session-c118", "hello", &mut sink);
        assert!(
            !after.contains("<workspace-note>"),
            "the note disappears after leave"
        );
    }

    #[tokio::test]
    async fn explicit_authored_loop_runs_without_a_model_call() {
        let loop_spec = AgentLoopSpec::parse("flow custom -> string\n  return \"custom loop\"")
            .expect("custom loop parses");
        let (engine, events, requests) = scripted_engine(Vec::new(), loop_spec);
        let session = events.create_session("scripted/test-model").unwrap();
        let mut sink = CollectSink::default();

        engine.run_turn(&session, "hello", &mut sink).await.unwrap();

        assert_eq!(sink.text, "custom loop");
        assert!(requests.lock().unwrap().is_empty());
        assert_eq!(events.conversation(&session).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn resumed_turn_starts_a_fresh_deterministic_read_cache_generation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (engine, events, root) =
            tool_engine(Arc::new(FileProbeTool(calls.clone())), idle_agent_loop());
        std::fs::write(root.join("value.txt"), "before").unwrap();
        let session = events.create_session("test-model").unwrap();
        let flow = resumable_read_flow();
        let mut started = CollectSink::default();

        engine
            .start_flow_turn(&session, &flow, &mut started)
            .await
            .unwrap();
        assert!(engine.flow.has_suspension(&session).unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        std::fs::write(root.join("value.txt"), "after").unwrap();
        let mut resumed = CollectSink::default();
        engine
            .run_turn(&session, "continue", &mut resumed)
            .await
            .unwrap();

        assert_eq!(resumed.text, "after");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the post-await read must execute in the resumed turn, not reuse the prior turn cache"
        );
    }

    #[tokio::test]
    async fn pre_cancelled_resume_is_terminal_and_keeps_its_checkpoint() {
        let barrier = Arc::new(TurnBarrier::default());
        let (engine, events, _root) = tool_engine(
            Arc::new(BlockingTurnTool(barrier.clone())),
            idle_agent_loop(),
        );
        let session = events.create_session("test-model").unwrap();
        let mut started = CollectSink::default();
        engine
            .start_flow_turn(&session, &blocking_resume_flow(), &mut started)
            .await
            .unwrap();

        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut resumed = CollectSink::default();
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            engine.run_turn_cancellable(&session, "answer", &mut resumed, &cancel),
        )
        .await
        .expect("a pre-cancelled resume must return promptly")
        .unwrap();

        assert_eq!(barrier.entered.load(Ordering::SeqCst), 0);
        assert!(engine.flow.has_suspension(&session).unwrap());
        assert_eq!(resumed.text, "(turn cancelled)");
        assert_eq!(resumed.ended, 1);
        let conversation = events.conversation(&session).unwrap();
        assert_eq!(conversation.len(), 3);
        assert_eq!(conversation[1].text(), "answer");
        assert_eq!(conversation[2].text(), "(turn cancelled)");
        assert_eq!(
            events.turns(&session).unwrap().last().unwrap().outcome,
            "cancelled"
        );
    }

    #[tokio::test]
    async fn cancelling_an_in_flight_resume_keeps_its_checkpoint() {
        let barrier = Arc::new(TurnBarrier::default());
        let (engine, events, _root) = tool_engine(
            Arc::new(BlockingTurnTool(barrier.clone())),
            idle_agent_loop(),
        );
        let session = events.create_session("test-model").unwrap();
        let mut started = CollectSink::default();
        engine
            .start_flow_turn(&session, &blocking_resume_flow(), &mut started)
            .await
            .unwrap();

        let cancel = CancellationToken::new();
        let mut resumed = CollectSink::default();
        let run = engine.run_turn_cancellable(&session, "answer", &mut resumed, &cancel);
        let controller = async {
            barrier.wait_for_entered(1).await;
            cancel.cancel();
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let (result, ()) = tokio::join!(run, controller);
            result
        })
        .await
        .expect("an in-flight resumed operation must stop on cancellation");
        result.unwrap();

        assert_eq!(barrier.active.load(Ordering::SeqCst), 0);
        assert!(engine.flow.has_suspension(&session).unwrap());
        assert_eq!(resumed.text, "(turn cancelled)");
        assert_eq!(resumed.ended, 1);
        let conversation = events.conversation(&session).unwrap();
        assert_eq!(conversation.len(), 3);
        assert_eq!(conversation[1].text(), "answer");
        assert_eq!(conversation[2].text(), "(turn cancelled)");
        assert_eq!(
            events.turns(&session).unwrap().last().unwrap().outcome,
            "cancelled"
        );
    }

    fn persisted_turn_contexts(events: &EventStore, session_id: &str) -> Vec<String> {
        events
            .observations(session_id)
            .unwrap()
            .into_iter()
            .filter(|observation| observation.kind == "test.turn_context")
            .filter_map(|observation| observation.data["session_id"].as_str().map(str::to_string))
            .collect()
    }

    fn persisted_callers(events: &EventStore, session_id: &str, kind: &str) -> Vec<String> {
        events
            .observations(session_id)
            .unwrap()
            .into_iter()
            .filter(|observation| observation.kind == kind)
            .filter_map(|observation| observation.data["caller"].as_str().map(str::to_string))
            .collect()
    }

    #[tokio::test]
    async fn one_raw_engine_serializes_turns_without_cross_wiring_sinks_or_audit() {
        let barrier = Arc::new(TurnBarrier::default());
        let (mut engine, events, _root) = tool_engine(
            Arc::new(BlockingTurnTool(barrier.clone())),
            blocking_agent_loop(),
        );
        engine.groups = vec![
            flux_evidence::ToolGroup {
                name: "turn.first".into(),
                description: "First-turn-only test catalog.".into(),
                tools: vec!["blocking_turn".into()],
                surface_when: vec![flux_evidence::SignalMatch {
                    kind: flux_evidence::KIND_TURN_INTENT.into(),
                    signal: Some("first".into()),
                }],
            },
            flux_evidence::ToolGroup {
                name: "turn.second".into(),
                description: "Second-turn-only test catalog.".into(),
                tools: vec!["second_only".into()],
                surface_when: vec![flux_evidence::SignalMatch {
                    kind: flux_evidence::KIND_TURN_INTENT.into(),
                    signal: Some("second".into()),
                }],
            },
        ];
        let engine = Arc::new(engine);
        let authority_before = engine.executor.approval_context();
        let first_session = events.create_session("test-model").unwrap();
        let second_session = events.create_session("test-model").unwrap();

        let first = tokio::spawn({
            let engine = engine.clone();
            let session = first_session.clone();
            async move {
                let mut sink = CollectSink::default();
                let result = engine.run_turn(&session, "first", &mut sink).await;
                (result, sink)
            }
        });
        barrier.wait_for_entered(1).await;
        let second = tokio::spawn({
            let engine = engine.clone();
            let session = second_session.clone();
            async move {
                let mut sink = CollectSink::default();
                let result = engine.run_turn(&session, "second", &mut sink).await;
                (result, sink)
            }
        });

        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                barrier.wait_for_entered(2)
            )
            .await
            .is_err(),
            "the second call must wait at the engine boundary"
        );
        barrier.release(1);
        let (first_result, first_sink) = first.await.unwrap();
        first_result.unwrap();
        barrier.wait_for_entered(2).await;
        barrier.release(1);
        let (second_result, second_sink) = second.await.unwrap();
        second_result.unwrap();

        assert_eq!(barrier.max_active.load(Ordering::SeqCst), 1);
        assert_eq!(first_sink.text, first_session);
        assert_eq!(second_sink.text, second_session);
        assert_eq!(first_sink.tools, ["blocking_turn"]);
        assert_eq!(second_sink.tools, ["blocking_turn"]);
        assert_eq!(first_sink.usages.len(), 1);
        assert_eq!(second_sink.usages.len(), 1);
        assert_eq!(first_sink.usages[0].as_ref().unwrap().input_tokens, 1);
        assert_eq!(first_sink.usages[0].as_ref().unwrap().output_tokens, 10);
        assert_eq!(second_sink.usages[0].as_ref().unwrap().input_tokens, 2);
        assert_eq!(second_sink.usages[0].as_ref().unwrap().output_tokens, 20);
        assert_eq!(first_sink.ended, 1);
        assert_eq!(second_sink.ended, 1);
        let active_groups = |sink: &CollectSink| {
            sink.observations
                .iter()
                .find(|observation| observation.kind == "groups.active")
                .and_then(|observation| observation.data["groups"].as_array())
                .cloned()
                .unwrap_or_default()
        };
        assert_eq!(active_groups(&first_sink), vec![json!("turn.first")]);
        assert_eq!(active_groups(&second_sink), vec![json!("turn.second")]);
        let (first_advertised, _) = surfaced_op_names(
            engine.executor.registry(),
            &engine.groups,
            &engine.cwd,
            &engine.sticky_groups,
            &first_session,
            &[],
            "",
        );
        let (second_advertised, _) = surfaced_op_names(
            engine.executor.registry(),
            &engine.groups,
            &engine.cwd,
            &engine.sticky_groups,
            &second_session,
            &[],
            "",
        );
        assert!(first_advertised.contains("blocking_turn"));
        assert!(!second_advertised.contains("blocking_turn"));
        assert_eq!(engine.executor.approval_context(), authority_before);
        let authority: Value = serde_json::from_str(&authority_before).unwrap();
        assert_eq!(authority["caller"]["principal"]["id"], "local");
        assert_eq!(
            events
                .conversation(&first_session)
                .unwrap()
                .iter()
                .map(Message::text)
                .collect::<Vec<_>>(),
            vec!["first", first_session.as_str()]
        );
        assert_eq!(
            events
                .conversation(&second_session)
                .unwrap()
                .iter()
                .map(Message::text)
                .collect::<Vec<_>>(),
            vec!["second", second_session.as_str()]
        );
        assert_eq!(
            persisted_turn_contexts(&events, &first_session),
            vec![first_session]
        );
        assert_eq!(
            persisted_turn_contexts(&events, &second_session),
            vec![second_session]
        );
    }

    #[tokio::test]
    async fn concurrent_raw_turns_keep_distinct_lexical_identities_in_sink_dispatch_and_audit() {
        let barrier = Arc::new(TurnBarrier::default());
        let (engine, events, _root) = tool_engine(
            Arc::new(IdentityBlockingTool(barrier.clone())),
            identity_blocking_agent_loop(),
        );
        let identity = |id: &str| {
            let (mut caller, trust) = engine.executor.identity().get();
            caller.principal.id = id.to_string();
            caller.principal.name = id.to_string();
            caller.source = "raw-engine-test".into();
            TurnIdentity::new(caller, trust)
        };
        let alice = identity("alice");
        let bob = identity("bob");
        let engine = Arc::new(engine);
        let first_session = events.create_session("test-model").unwrap();
        let second_session = events.create_session("test-model").unwrap();

        let first = tokio::spawn({
            let engine = engine.clone();
            let session = first_session.clone();
            async move {
                let mut sink = CollectSink::default();
                let result = engine
                    .run_turn_as(&session, "first", &mut sink, alice)
                    .await;
                (result, sink)
            }
        });
        barrier.wait_for_entered(1).await;
        let second = tokio::spawn({
            let engine = engine.clone();
            let session = second_session.clone();
            async move {
                let mut sink = CollectSink::default();
                let result = engine.run_turn_as(&session, "second", &mut sink, bob).await;
                (result, sink)
            }
        });

        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                barrier.wait_for_entered(2),
            )
            .await
            .is_err(),
            "bob's turn must wait without retargeting alice's active context"
        );
        barrier.release(1);
        let (first_result, first_sink) = first.await.unwrap();
        first_result.unwrap();
        barrier.wait_for_entered(2).await;
        barrier.release(1);
        let (second_result, second_sink) = second.await.unwrap();
        second_result.unwrap();

        assert_eq!(barrier.max_active.load(Ordering::SeqCst), 1);
        assert_eq!(first_sink.text, format!("{first_session}:alice"));
        assert_eq!(second_sink.text, format!("{second_session}:bob"));
        assert_eq!(first_sink.tools, ["identity_blocking"]);
        assert_eq!(second_sink.tools, ["identity_blocking"]);
        assert_eq!(
            persisted_callers(&events, &first_session, "turn.identity"),
            ["alice"]
        );
        assert_eq!(
            persisted_callers(&events, &second_session, "turn.identity"),
            ["bob"]
        );
        assert_eq!(
            persisted_callers(&events, &first_session, "tool_call"),
            ["alice"]
        );
        assert_eq!(
            persisted_callers(&events, &second_session, "tool_call"),
            ["bob"]
        );
        assert_eq!(
            persisted_callers(&events, &first_session, "test.identity_context"),
            ["alice"]
        );
        assert_eq!(
            persisted_callers(&events, &second_session, "test.identity_context"),
            ["bob"]
        );
        assert_eq!(
            engine.executor.identity().get().0.principal.id,
            "local",
            "explicit turns must not mutate the executor's assembly-time identity"
        );
    }

    #[tokio::test]
    async fn nested_authored_flow_reuses_the_active_lifecycle_without_relocking() {
        let (engine, events, _root) =
            tool_engine(Arc::new(NestedAuthoredTool), nested_authored_agent_loop());
        let session = events.create_session("test-model").unwrap();
        let mut sink = CollectSink::default();

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            engine.run_turn(&session, "run nested", &mut sink),
        )
        .await
        .expect("a nested authored flow must not recursively acquire the engine turn gate")
        .unwrap();

        assert_eq!(sink.text, "nested result");
        assert_eq!(sink.ended, 1);
    }

    #[tokio::test]
    async fn distinct_raw_engines_can_overlap() {
        let barrier = Arc::new(TurnBarrier::default());
        let (first_engine, first_events, _root) = tool_engine(
            Arc::new(BlockingTurnTool(barrier.clone())),
            blocking_agent_loop(),
        );
        let (second_engine, second_events, _root) = tool_engine(
            Arc::new(BlockingTurnTool(barrier.clone())),
            blocking_agent_loop(),
        );
        let first_engine = Arc::new(first_engine);
        let second_engine = Arc::new(second_engine);
        let first_session = first_events.create_session("test-model").unwrap();
        let second_session = second_events.create_session("test-model").unwrap();

        let first = tokio::spawn({
            let engine = first_engine.clone();
            let session = first_session.clone();
            async move {
                let mut sink = CollectSink::default();
                let result = engine.run_turn(&session, "first", &mut sink).await;
                (result, sink)
            }
        });
        let second = tokio::spawn({
            let engine = second_engine.clone();
            let session = second_session.clone();
            async move {
                let mut sink = CollectSink::default();
                let result = engine.run_turn(&session, "second", &mut sink).await;
                (result, sink)
            }
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            barrier.wait_for_entered(2),
        )
        .await
        .expect("independent engines must reach their operations concurrently");
        assert_eq!(barrier.max_active.load(Ordering::SeqCst), 2);
        barrier.release(2);
        let (first_result, first_sink) = first.await.unwrap();
        let (second_result, second_sink) = second.await.unwrap();
        first_result.unwrap();
        second_result.unwrap();
        assert_eq!(first_sink.text, first_session);
        assert_eq!(second_sink.text, second_session);
    }

    #[test]
    fn explicit_authored_loop_is_validated_against_the_live_catalog() {
        let loop_spec = AgentLoopSpec::parse(
            "flow invalid -> string\n  $value = missing_operation()\n  return $value",
        )
        .expect("source parses before semantic validation");
        let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider {
            responses: Mutex::new(VecDeque::new()),
            requests: Arc::new(Mutex::new(Vec::new())),
        });
        let (engine, _) = assemble_test_engine(provider, loop_spec);
        let error = engine.err().expect("unknown op must fail assembly");
        assert!(error.to_string().contains("missing_operation"));
    }

    #[tokio::test]
    async fn cancellation_keeps_a_valid_user_assistant_session_shape() {
        let provider: Arc<dyn Provider> = Arc::new(PendingProvider);
        let (engine, events) = assemble_test_engine(provider, AgentLoopSpec::default());
        let engine = engine.unwrap();
        let session = events.create_session("pending/test-model").unwrap();
        let mut sink = CollectSink::default();
        let cancel = CancellationToken::new();
        cancel.cancel();

        engine
            .run_turn_cancellable(&session, "wait forever", &mut sink, &cancel)
            .await
            .unwrap();

        let conversation = events.conversation(&session).unwrap();
        assert_eq!(conversation.len(), 2);
        assert_eq!(conversation[0].text(), "wait forever");
        assert_eq!(conversation[1].text(), "(turn cancelled)");
        assert_eq!(sink.ended, 1);
    }

    // ---- D-175: run_turn_pinned ----

    #[tokio::test]
    async fn run_turn_pinned_produces_a_normal_turn_and_resets_the_cassette() {
        let loop_spec = AgentLoopSpec::parse("flow custom -> string\n  return \"pinned answer\"")
            .expect("custom loop parses");
        let (engine, events, requests) = scripted_engine(Vec::new(), loop_spec);
        let session = events.create_session("scripted/test-model").unwrap();
        let mut sink = CollectSink::default();
        let scope = Arc::new(crate::cassette::CassetteScope::Record(
            crate::cassette::RecordScope::new(events.clone(), session.clone()),
        ));

        engine
            .run_turn_pinned(&session, "hello", scope, &mut sink)
            .await
            .unwrap();

        assert_eq!(sink.text, "pinned answer");
        assert_eq!(sink.ended, 1);
        assert!(
            requests.lock().unwrap().is_empty(),
            "a custom loop with no stage calls needs no model round-trip"
        );
        let turns = events.turns(&session).unwrap();
        assert_eq!(
            turns.len(),
            1,
            "one TurnStarted/TurnEnded pair, like any other turn"
        );
        assert!(turns[0].ended_at_ms.is_some());
        assert_eq!(turns[0].outcome, "ok");
        assert!(
            engine.flow.cassette().is_none(),
            "the pinned scope is reset on finish, exactly like the default `Record` scope"
        );
    }

    /// Serializes tests that mutate the process-global `FLUX_CASSETTE` env var (mirrors the
    /// `SANDBOX_ENV_LOCK`/`HOME_LOCK` guards elsewhere in the repo).
    static CASSETTE_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct CassetteEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Option<std::ffi::OsString>,
    }

    impl CassetteEnvGuard {
        fn set(value: &str) -> Self {
            let lock = CASSETTE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = std::env::var_os("FLUX_CASSETTE");
            std::env::set_var("FLUX_CASSETTE", value);
            Self { _lock: lock, saved }
        }
    }

    impl Drop for CassetteEnvGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(v) => std::env::set_var("FLUX_CASSETTE", v),
                None => std::env::remove_var("FLUX_CASSETTE"),
            }
        }
    }

    #[tokio::test]
    async fn run_turn_pinned_scope_wins_over_the_flux_cassette_kill_switch() {
        // A counting `echo` — same spec/name as `EchoTool`, so the SAME scripted
        // declare_intent → echo → prose shape `default_turn_routes_intent_then_uses_exact_native_tool_schema`
        // uses also drives it through the built-in adaptive loop's action-batch executor, which is the
        // path that actually self-wires the store's cassette (the outer loop's OWN stage dispatches
        // never do — only nested authored/action-batch execution does).
        struct CountingEcho(Arc<AtomicUsize>);
        #[async_trait]
        impl Tool for CountingEcho {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only(
                    "echo",
                    "Return the supplied text.",
                    json!({
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"],
                        "additionalProperties": false
                    }),
                )
            }
            async fn execute(&self, _ctx: &ToolContext, input: Value) -> Result<ToolResult> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ToolResult::ok(
                    input
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ))
            }
        }

        fn script() -> Vec<Vec<Chunk>> {
            vec![
                native_call(
                    "intent-1",
                    "declare_intent",
                    json!({"intent": "echo it", "capability_families": ["core"]}),
                ),
                native_call("echo-1", "echo", json!({"text": "live-value"})),
                prose("live-value"),
            ]
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(ScriptedProvider {
            responses: Mutex::new(script().into()),
            requests: requests.clone(),
        });
        let provider_dyn: Arc<dyn Provider> = provider.clone();
        let sequence = TEST_ROOT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "flux-run-turn-pinned-cassette-override-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CountingEcho(calls.clone())));
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["echo".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap()))),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = FlowEngine::assemble_with_loop(
            provider_dyn,
            executor,
            events.clone(),
            flow,
            "test-model".into(),
            "Use only observed evidence.".into(),
            2_048,
            5,
            Vec::new(),
            0,
            Vec::new(),
            root.clone(),
            AgentLoopSpec::default(),
        )
        .unwrap();

        // Record once, live, to get one real `echo` cell.
        let session = events.create_session("scripted/test-model").unwrap();
        let mut sink = CollectSink::default();
        engine
            .run_turn(&session, "echo it", &mut sink)
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the first turn dispatched echo live once"
        );

        // Pin a hermetic `Frozen` scope over that recording, force the kill switch OFF, and re-drive
        // the SAME script (the model still runs — only leaf-op dispatch is pinned): `run_turn_pinned`
        // must still install the scope despite `FLUX_CASSETTE=0`, so `echo` is served from tape and
        // never touches the live tool again.
        let trace = events.run_trace(&session).unwrap();
        let tape = crate::cassette::ReplayTape::from_trace(&trace);
        let scope = Arc::new(crate::cassette::CassetteScope::Frozen(
            crate::cassette::FrozenTape::hermetic(tape),
        ));
        provider.responses.lock().unwrap().extend(script());
        let _guard = CassetteEnvGuard::set("0");
        assert!(
            !crate::cassette::enabled(),
            "the kill switch is off for this test"
        );

        let mut sink2 = CollectSink::default();
        engine
            .run_turn_pinned(&session, "echo it again", scope, &mut sink2)
            .await
            .unwrap();

        assert_eq!(sink2.text, "live-value");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the pinned scope must win over FLUX_CASSETTE=0 — a live re-dispatch would bump this to 2"
        );
        assert!(engine.flow.cassette().is_none(), "still reset on finish");
    }

    /// Minimal test fixture for a discovered skill, filling every field D-190 doesn't care about
    /// with its inert default.
    fn skill_fixture(
        name: &str,
        description: &str,
        body: &str,
        source: Option<std::path::PathBuf>,
    ) -> flux_skill::Skill {
        flux_skill::Skill {
            name: name.into(),
            description: description.into(),
            triggers: Vec::new(),
            body: body.into(),
            format: flux_skill::SkillFormat::AgentSkills,
            source,
            allowed_ops: Vec::new(),
            model: None,
            disable_model_invocation: false,
            argument_hint: String::new(),
            agent_triggerable: false,
        }
    }

    /// D-190: a `SKILL.md`-backed skill discloses its **directory** in the injected `<skill>` tag —
    /// the anchor a model needs to `read` sibling `references/*.md` — while a flat `.md` skill
    /// discloses the file itself, and a skill with no captured `source` (constructed in-memory, as
    /// the SDK does) discloses nothing at all.
    #[test]
    fn skill_tag_discloses_directory_for_skill_md_and_file_for_flat_skills() {
        let dir_skill = skill_fixture(
            "pkg",
            "",
            "See references/extra.md",
            Some(std::path::PathBuf::from(
                "/proj/.claude/skills/pkg/SKILL.md",
            )),
        );
        assert_eq!(
            skill_disclosed_path(&dir_skill),
            Some(std::path::PathBuf::from("/proj/.claude/skills/pkg"))
        );

        let flat_skill = skill_fixture(
            "flat",
            "",
            "flat body",
            Some(std::path::PathBuf::from("/proj/.claude/skills/flat.md")),
        );
        assert_eq!(
            skill_disclosed_path(&flat_skill),
            Some(std::path::PathBuf::from("/proj/.claude/skills/flat.md"))
        );

        let in_memory_skill = skill_fixture("sdk", "", "sdk body", None);
        assert_eq!(skill_disclosed_path(&in_memory_skill), None);
    }

    /// The same behavior through the actual injection path: `base_system_with_skills` renders a
    /// `path="…"` attribute for skills with a source and omits it entirely otherwise, matching the
    /// `<skill name="x" path="…">` shape the model is meant to anchor a `read` on.
    #[tokio::test]
    async fn injected_skill_tag_carries_the_disclosed_path_attribute() {
        let (engine, _events) =
            assemble_test_engine(Arc::new(PendingProvider), AgentLoopSpec::default());
        let mut engine = engine.unwrap();
        engine.skills = vec![
            skill_fixture(
                "with-dir",
                "",
                "dir body",
                Some(std::path::PathBuf::from(
                    "/proj/.claude/skills/with-dir/SKILL.md",
                )),
            ),
            skill_fixture("no-source", "", "in-memory body", None),
        ];
        let mut sink = CollectSink::default();
        let rendered = engine.base_system_with_skills("session", "hello", &mut sink);

        assert!(
            rendered.contains("<skill name=\"with-dir\" path=\"/proj/.claude/skills/with-dir\">"),
            "expected a directory path attribute for the SKILL.md-backed skill: {rendered}"
        );
        assert!(
            rendered.contains("<skill name=\"no-source\">\nin-memory body\n</skill>"),
            "a source-less skill must not gain a fabricated path attribute: {rendered}"
        );
        assert!(
            !rendered.contains("no-source\" path"),
            "a source-less skill must not disclose any path: {rendered}"
        );
    }

    /// End-to-end: a skill whose body points at `references/extra.md` discloses its directory, and a
    /// turn that reads that supporting file through the normal `read` op succeeds under the same
    /// default policy the CLI ships (`read` is in `DEFAULT_ALLOW`) — disclosure does not widen any
    /// grant, the file is reachable because it already sits inside the project workspace jail.
    #[tokio::test]
    async fn turn_reads_a_skills_supporting_file_via_the_disclosed_path() {
        let sequence = TEST_ROOT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "flux-skill-supporting-file-{}-{sequence}",
            std::process::id()
        ));
        let skill_dir = root.join(".claude/skills/pkg");
        let refs_dir = skill_dir.join("references");
        std::fs::create_dir_all(&refs_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: pkg\ndescription: a multi-file skill\n---\nSee references/extra.md for detail.",
        )
        .unwrap();
        std::fs::write(refs_dir.join("extra.md"), "the extra detail").unwrap();

        let skill = skill_fixture(
            "pkg",
            "a multi-file skill",
            "See references/extra.md for detail.",
            Some(skill_dir.join("SKILL.md")),
        );

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(flux_tools::ReadTool));
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        // Mirrors flux-cli's `DEFAULT_ALLOW` (`crates/flux-cli/src/execution.rs`): `read` is
        // pre-allowed with no configured rules, so this is the standard policy outcome, not a
        // widened grant for this story.
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["read".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap()))),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider {
            responses: Mutex::new(
                vec![
                    native_call(
                        "intent-1",
                        "declare_intent",
                        json!({
                            "intent": "read the pkg skill's reference",
                            "capability_families": ["workspace.read"]
                        }),
                    ),
                    native_call(
                        "read-1",
                        "read",
                        json!({"path": ".claude/skills/pkg/references/extra.md"}),
                    ),
                    prose("Found it: the extra detail."),
                ]
                .into(),
            ),
            requests: requests.clone(),
        });
        let engine = FlowEngine::assemble_with_loop(
            provider,
            executor,
            events.clone(),
            flow,
            "test-model".into(),
            "Use only observed evidence.".into(),
            2_048,
            5,
            vec![skill],
            0,
            Vec::new(),
            root.clone(),
            AgentLoopSpec::default(),
        )
        .unwrap();

        let session = events.create_session("scripted/test-model").unwrap();
        let mut sink = CollectSink::default();
        engine
            .run_turn(
                &session,
                "what does the pkg skill's reference say?",
                &mut sink,
            )
            .await
            .unwrap();

        assert_eq!(
            sink.tools,
            vec!["read"],
            "the read op was actually dispatched"
        );
        // The system prompt segments handed to the model carry the disclosed directory path.
        let expected = skill_dir.display().to_string();
        let seen = requests.lock().unwrap();
        let carried_path = seen
            .iter()
            .any(|r| r.system_segments.iter().any(|s| s.text.contains(&expected)));
        assert!(
            carried_path,
            "the skill's directory path was not carried into the request"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
