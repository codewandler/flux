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
use flux_runtime::Executor;

use crate::ast::DraftAst;
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
        let composites = Arc::new(DynamicComposites::load(executor.context().system.as_ref())?);
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

    /// Apply one reasoning policy to the full agent call graph: intent, exploration, repair,
    /// presentation, authored model stages, and compaction calls.
    pub fn with_reasoning(mut self, thinking: bool, effort: Option<Effort>) -> Self {
        self.thinking = thinking;
        self.effort = effort;
        self.loop_host.set_reasoning(thinking, effort);
        self
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
        // If a flow is suspended on a top-level `await`, THIS turn's message is the awaited input:
        // resume the persisted flow instead of starting a fresh outer-loop turn. (`take_suspension`
        // clears it.)
        if let Some((flow_name, body, node, _source)) = self.flow.take_suspension(session_id)? {
            return self
                .resume_suspended(session_id, user_input, flow_name, body, node, sink)
                .await;
        }

        self.events
            .record_message(session_id, &Message::user_text(user_input))?;
        // Turn boundary for the deterministic read cache (L-54): between turns the user (or any
        // external process) may have changed what a read observes, so the cache never survives a
        // turn — its reuse window is repair rounds / retries / sub-plans WITHIN this turn.
        self.executor.begin_cache_turn();
        // Non-fatal: a DB hiccup must never prevent a turn from running.
        let turn_id = self
            .events
            .begin_turn(
                session_id,
                user_input,
                // Canonical attribution key (C-15) — the old-log cost fallback rolls turns up by
                // this stamp, so it must match the per-call `CallUsage` keys.
                &flux_core::canonical_model_spec(Some(self.provider.name()), &self.model),
            )
            .unwrap_or(-1);

        // Agent identity + project context + explicitly enabled skills — included in every model
        // stage without activating discovery metadata implicitly.
        let base_system = self.base_system_with_skills(user_input, sink);

        // Evidence-gated surfacing for this turn: probe the workspace signals once and hand the
        // advertised op set to the loop host, so every adaptive stage sees the gated catalog.
        let advertised = self.surfaced_for_turn(session_id, user_input, sink);

        // Drive the flux-lang agent loop (`agent_loop`) through an OWNED channel sink — the `'static`
        // loop host owns it while stages are in flight — draining its events onto the borrowed
        // `sink` live (inner ops stream as they happen; loop-machinery ops are filtered by
        // `drain_event`).
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::loop_host::SinkEvent>();
        let channel: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
            crate::loop_host::ChannelSink::new(tx),
        ));
        self.loop_host.set_turn(
            session_id.to_string(),
            Some(base_system),
            channel.clone(),
            Some(advertised),
            Some((self.events.clone(), turn_id)),
        );

        // Snapshot per-turn evidence BEFORE compaction: a failed/cancelled compaction is still this
        // turn and must close with correctly scoped telemetry through the same finalization path.
        let iter_base = self.executor.evidence().by_kind("turn.iteration").count();
        let subagent_base = self.executor.evidence().by_kind("subagent.usage").count();

        // Compact only after `set_turn` reset the host's accounting. Summary generation is a real
        // provider call, and usage that arrives before a stream error must be charged exactly once.
        // Any error occurs after the user message + TurnStarted are durable, so close the turn with
        // one assistant message instead of `?`-returning an invalid user tail / pending turn.
        let (compaction, compaction_usage) =
            self.compaction_attempt(session_id, sink, cancel).await;
        if let Some(usage) = compaction_usage {
            self.loop_host
                .record_external_call(self.provider.name(), &self.model, usage);
        }
        if let Err(error) = compaction {
            let answer = format!(
                "I couldn't compact the conversation before continuing — {}",
                model_error(&error)
            );
            sink.text_delta(&answer);
            let subagent_calls = self.subagent_calls_since(subagent_base);
            self.record_call_usage_events(session_id, turn_id, &subagent_calls);
            let usage = self.turn_usage(&subagent_calls);
            let _ = self
                .events
                .end_turn(session_id, turn_id, "error", 0, &answer, usage.clone());
            return self.finish_turn(session_id, turn_id, sink, &answer, false, usage);
        }
        // Thread this turn's cancellation into the tool context so a spawning tool (`task`) can hand a
        // child token to its sub-agent — cancelling the parent turn then cancels the child. The session
        // id rides along so `task` can correlate the child's audit stream to THIS turn (A-08).
        self.executor.context().set_cancel(cancel.clone());
        self.executor.context().set_session(session_id);

        // C-43: arm the per-turn cassette recorder — every leaf-op dispatch this turn lands as a
        // redacted `OpRecorded` cell on the session stream, making the turn hermetically
        // replayable (`flux replay`). Off with FLUX_CASSETTE=0; the recorder is telemetry-grade
        // (append failures never fail the turn).
        if crate::cassette::enabled() {
            self.flow.set_cassette(Some(std::sync::Arc::new(
                crate::cassette::CassetteScope::Record(crate::cassette::RecordScope::new(
                    self.events.clone(),
                    session_id,
                )),
            )));
        } else {
            self.flow.set_cassette(None);
        }

        // Reveal the loop machinery on the surface when `--show-loop`/`FLUX_SHOW_LOOP` is set.
        let reveal = show_loop();
        let outcome = {
            let mut outer = crate::loop_host::SharedSink::new(channel.clone());
            let flow_fut = execute_flow_traced(
                &self.flow,
                &self.executor,
                session_id,
                &self.agent_loop,
                &mut outer,
                trace_loop(),
            );
            tokio::pin!(flow_fut);

            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break None,
                    maybe = rx.recv() => {
                        if let Some(ev) = maybe { drain_event(ev, sink, reveal); }
                    }
                    res = &mut flow_fut => break Some(res),
                }
            }
        };
        // Cancellation drops the in-flight flow (and any `task` child collector) at the end of the
        // scope above. Drain only afterwards so drop-time terminal activity is not stranded in the
        // channel when the borrowed surface is about to return.
        while let Ok(ev) = rx.try_recv() {
            drain_event(ev, sink, reveal);
        }
        let Some(outcome) = outcome else {
            let subagent_calls = self.subagent_calls_since(subagent_base);
            self.record_call_usage_events(session_id, turn_id, &subagent_calls);
            let usage = self.turn_usage(&subagent_calls);
            let _ = self.events.end_turn(
                session_id,
                turn_id,
                "cancelled",
                0,
                "(turn cancelled)",
                usage.clone(),
            );
            return self.finish_turn(session_id, turn_id, sink, "(turn cancelled)", true, usage);
        };

        // The adaptive loop's decision point is an authored, top-level conditional `await`. Park it
        // through the SAME durable suspension store used by every authored flow, so the next user
        // message resumes after the await with all prior stage artifacts still bound.
        if let Ok(flow_outcome) = &outcome {
            if let Some(suspension) = &flow_outcome.suspension {
                self.flow.save_suspension(
                    session_id,
                    self.agent_loop.name.as_deref(),
                    &self.agent_loop.body,
                    suspension.node,
                    &suspension.source,
                )?;
                let answer = suspension_prompt(flow_outcome);
                sink.text_delta(&answer);
                let iterations = self
                    .executor
                    .evidence()
                    .by_kind("turn.iteration")
                    .count()
                    .saturating_sub(iter_base) as u32;
                let subagent_calls = self.subagent_calls_since(subagent_base);
                self.record_call_usage_events(session_id, turn_id, &subagent_calls);
                let usage = self.turn_usage(&subagent_calls);
                let _ = self.events.end_turn(
                    session_id,
                    turn_id,
                    "suspended",
                    iterations,
                    &answer,
                    usage.clone(),
                );
                return self.finish_turn(session_id, turn_id, sink, &answer, false, usage);
            }
        }

        // The loop returns `$answer` — the model's prose, grounded in the fed-back results (the `chat`
        // case). On failure (e.g. a model stage errored through the op envelope) we surface it as
        // the answer so the session shape stays valid and the turn never ends in silence.
        let (answer, tag) = match outcome {
            Ok(o) => {
                let a = o.result.trim().to_string();
                if a.is_empty() {
                    (
                        format!(
                            "Reached the maximum of {} adaptive iterations for this turn; stopping.",
                            self.max_iterations
                        ),
                        "max_iter",
                    )
                } else {
                    (a, "ok")
                }
            }
            Err(e) => (format!("I couldn't complete the turn — {e}"), "error"),
        };
        // The loop binds `$answer` but does not stream it (a `jq`/`fmt` bind is silent), so emit it now.
        sink.text_delta(&answer);
        let iterations = self
            .executor
            .evidence()
            .by_kind("turn.iteration")
            .count()
            .saturating_sub(iter_base) as u32;
        let subagent_calls = self.subagent_calls_since(subagent_base);
        self.record_call_usage_events(session_id, turn_id, &subagent_calls);
        let usage = self.turn_usage(&subagent_calls);
        let _ = self
            .events
            .end_turn(session_id, turn_id, tag, iterations, &answer, usage.clone());
        self.finish_turn(session_id, turn_id, sink, &answer, false, usage)
    }

    fn surfaced_for_turn(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
    ) -> std::collections::HashSet<String> {
        let (advertised, surfaced) = surfaced_op_names(
            self.executor.registry(),
            &self.groups,
            &self.cwd,
            &self.sticky_groups,
            session_id,
            &self.ambient_signals,
            user_input,
        );
        if let Some(surfaced) = surfaced.as_ref() {
            self.record_active_groups(surfaced, sink);
        }
        advertised
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
    /// model-backed stage.
    fn base_system_with_skills(&self, _user_input: &str, sink: &mut dyn AgentSink) -> String {
        let mut base_system = self.system_prompt.clone();
        for skill in &self.skills {
            base_system.push_str(&format!(
                "\n\n<skill name=\"{}\">\n{}\n</skill>",
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
        base_system
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
        let log = self.executor.evidence();
        let all = log.all();
        let start = self
            .evidence_flushed
            .load(std::sync::atomic::Ordering::SeqCst)
            .min(all.len());
        // C-24: advance the watermark only past observations whose write returned `Ok` — a transient
        // `record_observation` failure (WAL `BUSY`, disk-full) leaves the unwritten tail behind the
        // watermark to be retried next flush, instead of being lost forever behind an
        // unconditionally-advanced mark.
        let written = flush_tail(&all[start..], |obs| {
            let redacted = redact_observation(redactor, obs);
            self.events
                .record_observation(session_id, turn_id, &redacted)
        });
        self.evidence_flushed
            .store(start + written, std::sync::atomic::Ordering::SeqCst);
    }

    /// This turn's token tally, as an `Option` for the sink: `None` when nothing was billed (e.g. an
    /// offline `-m mock` turn) so a surface needn't render a misleading all-zero annotation. Includes
    /// any sub-agents this turn spawned (`subagent_calls`, from [`Self::subagent_calls_since`]) — a
    /// `task` call's tokens are real spend the parent turn incurred, so its total must reflect them
    /// (C-06 sub-agent rollup).
    fn turn_usage(&self, subagent_calls: &[(String, Usage)]) -> Option<Usage> {
        let mut usage = self.loop_host.turn_usage();
        for (_, call) in subagent_calls {
            usage.output_tokens += call.output_tokens;
            usage.input_tokens += call.input_tokens;
            usage.cache_creation_input_tokens += call.cache_creation_input_tokens;
            usage.cache_read_input_tokens += call.cache_read_input_tokens;
            usage.reasoning_tokens += call.reasoning_tokens;
        }
        (usage.total() > 0).then_some(usage)
    }

    /// Every `subagent.usage` observation recorded since `base` (this turn's sub-agents, per the
    /// snapshot-then-diff scoping [`run_turn_cancellable`] already uses for `turn.iteration`), parsed
    /// into `(model, usage)` pairs. A malformed/missing field is skipped rather than panicking — this
    /// reads a cross-crate string-keyed contract (`flux-orchestrate`'s `TaskTool`), not a typed one.
    fn subagent_calls_since(&self, base: usize) -> Vec<(String, Usage)> {
        self.executor
            .evidence()
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
    /// routes through the existing suspension-first branch (`resume_suspended`), and surfaces the
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
        // Open a first-class turn. The flow speaks first (no user utterance), so the turn's
        // attribution label is the flow's name — or a generic marker for an anonymous flow.
        let label = flow.name.as_deref().unwrap_or("(flow start)");
        let turn_id = self
            .events
            .begin_turn(
                session_id,
                label,
                &flux_core::canonical_model_spec(Some(self.provider.name()), &self.model),
            )
            .unwrap_or(-1);
        // Fresh turn boundary for the deterministic read cache (L-54), as in `run_turn`.
        self.executor.begin_cache_turn();
        // A fresh authored-flow drive bypasses the adaptive loop host exactly like a resume: the only billable
        // spend is `task` sub-agents in the flow body. Snapshot the count so only this turn's fold in
        // (`record_resume_usage` reads observations since this base — the same helper resume uses).
        let subagent_base = self.executor.evidence().by_kind("subagent.usage").count();

        // Arm the authored-loop host so a top-level `ai_segment` can delegate a bounded
        // native-schema model stage (D-131 Phase B). No user utterance drives this turn, so skills match
        // against an empty input. For a flow with no `ai_segment` this is harmless overhead — the
        // authored prompt is still surfaced explicitly below (Phase A unchanged).
        let base_system = self.base_system_with_skills("", sink);
        let advertised = self.surfaced_for_turn(session_id, "", sink);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::loop_host::SinkEvent>();
        let channel: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
            crate::loop_host::ChannelSink::new(tx),
        ));
        self.loop_host.set_turn(
            session_id.to_string(),
            Some(base_system),
            channel.clone(),
            Some(advertised),
            Some((self.events.clone(), turn_id)),
        );
        self.executor.context().set_session(session_id);

        self.composites
            .ensure_session_loaded(&self.flow, session_id)?;
        let composites = self.composites.active_for_session(session_id);

        // Run the authored flow through an OWNED channel sink, draining its events onto the borrowed
        // `sink` live — so an `ai_segment`'s native leaf ops stream as they happen (the machinery
        // ops are filtered by `drain_event`). Mirrors `run_turn_cancellable`'s plumbing.
        let mut outer = crate::loop_host::SharedSink::new(channel.clone());
        let reveal = show_loop();
        let result = {
            let flow_fut = execute_flow_with_composites(
                &self.flow,
                &self.executor,
                session_id,
                flow,
                &composites,
                &mut outer,
            );
            tokio::pin!(flow_fut);
            loop {
                tokio::select! {
                    biased;
                    maybe = rx.recv() => { if let Some(ev) = maybe { drain_event(ev, sink, reveal); } }
                    res = &mut flow_fut => {
                        while let Ok(ev) = rx.try_recv() { drain_event(ev, sink, reveal); }
                        break res;
                    }
                }
            }
        };
        let outcome = match result {
            Ok(o) => o,
            Err(e) => {
                let msg = format!("The flow failed to start — {e}");
                sink.text_delta(&msg);
                let usage = self.record_resume_usage(session_id, turn_id, subagent_base);
                let _ = self
                    .events
                    .end_turn(session_id, turn_id, "error", 0, &msg, usage.clone());
                return self.finish_turn(session_id, turn_id, sink, &msg, true, usage);
            }
        };

        // Suspended on the first top-level `await`: persist the resume point (the flow name rides
        // along so a NAMED flow's resume derives the same checkpoint `flow_key`, L-21) and surface
        // the flow's authored prompt. Every subsequent `run_turn` now routes through the existing
        // suspension-first branch — no second parking mechanism (invariant 3).
        if let Some(susp) = &outcome.suspension {
            self.flow.save_suspension(
                session_id,
                flow.name.as_deref(),
                &flow.body,
                susp.node,
                &susp.source,
            )?;
            let prompt = suspension_prompt(&outcome);
            sink.text_delta(&prompt);
            let usage = self.record_resume_usage(session_id, turn_id, subagent_base);
            let _ = self.events.end_turn(
                session_id,
                turn_id,
                "suspended",
                outcome.steps as u32,
                &prompt,
                usage.clone(),
            );
            return self.finish_turn(session_id, turn_id, sink, &prompt, false, usage);
        }

        // Completed with no `await`: the flow's own output is the answer.
        let answer = if !outcome.result.trim().is_empty() {
            outcome.result.trim().to_string()
        } else {
            format!("Flow completed ({} step(s)).", outcome.steps)
        };
        sink.text_delta(&answer);
        let usage = self.record_resume_usage(session_id, turn_id, subagent_base);
        let _ = self.events.end_turn(
            session_id,
            turn_id,
            "completed",
            outcome.steps as u32,
            &answer,
            usage.clone(),
        );
        self.finish_turn(session_id, turn_id, sink, &answer, false, usage)
    }

    /// Resume a flow suspended on a top-level `await`, with this turn's message as the awaited input.
    /// Continues from the next statement (the prefix and its side effects are not re-run); the flow may
    /// suspend again on a later `await` (persist + wait) or complete (surface its result). Bypasses
    /// intent detection — a resume is deterministic continuation, not a fresh outer-loop turn.
    ///
    /// v1 limitations (accepted; refinements later): (1) the suspension is taken (deleted) before the
    /// remainder runs, so if a post-await op *fails*, the unfinished flow is not retryable (its earlier
    /// side effects stay committed) — per-statement resume checkpoints would fix this. (2) Once a flow
    /// is awaiting, the next message is *always* consumed as the input — there is no escape sentinel or
    /// TTL, so the user can't redirect to a new request without first answering (a REPL `/cancel` is the
    /// natural home for an escape, above the engine).
    async fn resume_suspended(
        &self,
        session_id: &str,
        user_input: &str,
        flow_name: Option<String>,
        body: Vec<flux_lang::ast::Node>,
        node: flux_lang::ast::NodeId,
        sink: &mut dyn AgentSink,
    ) -> Result<()> {
        self.events
            .record_message(session_id, &Message::user_text(user_input))?;
        // C-26: a resumed continuation is a first-class turn. Open it here so its observations are
        // turn-scoped and it emits a `TurnSummary`/`CallUsage`, instead of flushing unscoped under a
        // hardcoded `turn_id = -1`. A NEW turn id (not a continuation of the suspended one) — the
        // suspended turn already closed when it parked; this reply is a distinct unit of work.
        let turn_id = self
            .events
            .begin_turn(
                session_id,
                user_input,
                &flux_core::canonical_model_spec(Some(self.provider.name()), &self.model),
            )
            .unwrap_or(-1);
        // Sub-agent spend during the resume (a `task` op in the resumed body) rides the shared
        // evidence log as `subagent.usage` observations, exactly as in `run_turn` — snapshot the
        // count so only THIS resume's sub-agents fold in. The resumed body may also invoke the
        // model via a top-level `ai_segment` (D-131 Phase B), whose spend rides the loop host's
        // per-turn tally, reset by the `set_turn` below.
        let subagent_base = self.executor.evidence().by_kind("subagent.usage").count();

        let input = flux_lang::ast::Value::String(user_input.to_string());

        // Arm the authored-loop host so a top-level `ai_segment` AFTER the resumed `await` can
        // delegate a bounded run of model turns (D-131 Phase B). Skills match the reply text; a
        // resume with no segment pays only harmless overhead.
        let base_system = self.base_system_with_skills(user_input, sink);
        let advertised = self.surfaced_for_turn(session_id, user_input, sink);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::loop_host::SinkEvent>();
        let channel: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
            crate::loop_host::ChannelSink::new(tx),
        ));
        self.loop_host.set_turn(
            session_id.to_string(),
            Some(base_system),
            channel.clone(),
            Some(advertised),
            Some((self.events.clone(), turn_id)),
        );
        self.executor.context().set_session(session_id);

        self.composites
            .ensure_session_loaded(&self.flow, session_id)?;
        let composites = self.composites.active_for_session(session_id);

        // Run the resumed continuation through an owned channel sink, draining onto the borrowed
        // `sink` live (a segment's inner ops stream as they happen). The persisted flow name rides
        // along so a NAMED flow's resumed run derives the same checkpoint `flow_key` (L-21).
        let mut outer = crate::loop_host::SharedSink::new(channel.clone());
        let reveal = show_loop();
        let result = {
            let flow_fut = resume_flow_with_composites(
                &self.flow,
                &self.executor,
                session_id,
                flow_name.as_deref(),
                &body,
                node,
                input,
                &composites,
                &mut outer,
            );
            tokio::pin!(flow_fut);
            loop {
                tokio::select! {
                    biased;
                    maybe = rx.recv() => { if let Some(ev) = maybe { drain_event(ev, sink, reveal); } }
                    res = &mut flow_fut => {
                        while let Ok(ev) = rx.try_recv() { drain_event(ev, sink, reveal); }
                        break res;
                    }
                }
            }
        };
        let outcome = match result {
            Ok(o) => o,
            Err(e) => {
                let msg = format!("The resumed flow failed — {e}");
                sink.text_delta(&msg);
                let usage = self.record_resume_usage(session_id, turn_id, subagent_base);
                let _ = self
                    .events
                    .end_turn(session_id, turn_id, "error", 0, &msg, usage.clone());
                return self.finish_turn(session_id, turn_id, sink, &msg, true, usage);
            }
        };

        // Suspended again on a later `await`: persist the new resume point (name included) and wait
        // for more input.
        if let Some(susp) = &outcome.suspension {
            self.flow.save_suspension(
                session_id,
                flow_name.as_deref(),
                &body,
                susp.node,
                &susp.source,
            )?;
            // D-131: surface the flow's own authored prompt (its last emitted view) rather than the
            // fixed hint — the hint remains only as the empty-emit fallback (`suspension_prompt`).
            let prompt = suspension_prompt(&outcome);
            sink.text_delta(&prompt);
            let usage = self.record_resume_usage(session_id, turn_id, subagent_base);
            let _ = self.events.end_turn(
                session_id,
                turn_id,
                "suspended",
                outcome.steps as u32,
                &prompt,
                usage.clone(),
            );
            return self.finish_turn(session_id, turn_id, sink, &prompt, false, usage);
        }

        // Completed: the flow's own output is the answer (a model-grounded summary is a later refinement).
        let answer = if !outcome.result.trim().is_empty() {
            outcome.result.trim().to_string()
        } else {
            format!("Resumed and completed ({} step(s)).", outcome.steps)
        };
        sink.text_delta(&answer);
        let usage = self.record_resume_usage(session_id, turn_id, subagent_base);
        let _ = self.events.end_turn(
            session_id,
            turn_id,
            "resumed",
            outcome.steps as u32,
            &answer,
            usage.clone(),
        );
        self.finish_turn(session_id, turn_id, sink, &answer, false, usage)
    }

    /// Record a flow-driven turn's spend and return the turn total (C-26). Used by both
    /// [`Self::start_flow_turn`] and [`Self::resume_suspended`]; both now arm (and reset) the loop
    /// host before running, so this folds BOTH the loop host's model-stage calls — non-zero only when a
    /// top-level `ai_segment` delegated to the model this turn (D-131) — AND the `task` sub-agent
    /// spend recorded since `subagent_base`, emitting one `CallUsage` per call and returning the
    /// turn aggregate. `None` when nothing billed, mirroring [`Self::turn_usage`]. Identical
    /// accounting to `run_turn_cancellable`'s turn-end (`record_call_usage_events` + `turn_usage`).
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
    let max = u32::try_from(max_iterations).map_err(|_| {
        flux_core::Error::Other(format!(
            "max_iterations {max_iterations} exceeds Flux-Lang's u32 repeat bound"
        ))
    })?;
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
fn suspension_prompt(outcome: &FlowOutcome) -> String {
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use flux_core::{StopReason, ToolResultContent};
    use flux_provider::ChunkStream;
    use flux_runtime::{
        AllowApprover, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
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

    #[derive(Default)]
    struct CollectSink {
        text: String,
        tools: Vec<String>,
        observations: Vec<flux_evidence::Observation>,
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

        fn turn_end(&mut self, _usage: Option<Usage>) {
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
            root,
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
}
