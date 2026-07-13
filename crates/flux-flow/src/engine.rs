//! The engine: flux-flow's single turn loop, and the seat of "the LLM plans, the runtime runs".
//!
//! There is exactly **one** engine. Every turn the model is a compiler front-end: it either emits a
//! typed Flux-Lang plan (a graph the runtime executes through [`Executor::dispatch`](flux_runtime))
//! or answers in prose (a chat turn). The free-form "one provider-native tool call at a time" loop is
//! gone — a single op is just a one-node plan. `flux plan` shows exactly what a turn would
//! run, because the same [`compile_turn`] drives the engine and the CLI.
//!
//! Per turn: append the user message → compile a plan (pure DAG — the model's only tool is emit_plan) →
//! risk-gated execution via [`execute_flow_traced`] (per-op approval through the same envelope) → feed the
//! result back *ephemerally* so the model can iterate (read → fix → re-run) → persist **one** assistant
//! summary. The persisted session log is pure `user → assistant(text)` alternation: raw op outputs
//! never re-enter history (the "don't re-send" token win), which also removes the session-shape bug
//! class (no persisted tool_use/tool_result pairs). Symbols + summaries carry state forward.
//!
//! The engine reuses [`crate::AgentSink`] so a surface (CLI/TUI) can drive it with the same sink.
//! Every op still executes through `Executor::dispatch` — there is no new bypass surface.

use std::sync::Arc;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::AgentSink;
use flux_core::{Chunk, ContentBlock, Error, Message, Result, Usage};
use flux_events::EventStore;
use flux_provider::{Effort, Provider, Request};
use flux_runtime::Executor;

use crate::ast::DraftAst;
use crate::compile::{compile_turn, Brief, CompileOptions, Phase, TurnOutput, GATHER_ROUND_BUDGET};
use crate::composites::DynamicComposites;
use crate::registry::OpRegistry;
use crate::runtime::{
    execute_flow_resumable_with_composites, execute_flow_traced, execute_flow_with_composites,
    resume_flow_with_composites,
};
use crate::state::FlowStore;
use flux_lang::runtime::FlowOutcome;

/// flux-flow's turn engine: a provider, the tool executor (safety envelope), the unified event store
/// (conversation + run trace + turn telemetry), and flux-flow's own value/symbol/suspension store.
pub struct FlowEngine {
    /// Shared with the loop host so the planner and `maybe_compact` use one provider instance.
    pub provider: Arc<dyn Provider>,
    /// Shared (`Arc`): the loop host re-enters this same executor for `run_plan`, so the inner runs share
    /// one perms/approver/evidence/context with the outer agent loop.
    pub executor: Arc<Executor>,
    pub events: Arc<EventStore>,
    /// Shared (`Arc`) with the loop host: inner runs bind symbols / trace into the same session store.
    pub flow: Arc<FlowStore>,
    /// The agent loop itself, written in flux-lang (`assets/agent-loop.flux`): plan → match → run_plan →
    /// feed back → repeat-until-prose. The bootstrap runs THIS each turn — there is no Rust turn loop.
    pub agent_loop: DraftAst,
    /// The installed reflexive host; `set_turn` points it at the current session + sink before each run.
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
    sticky_groups: std::sync::Mutex<std::collections::HashSet<String>>,
    /// How many in-memory evidence observations have been flushed to the event store so far — the
    /// per-turn watermark [`flush_observations`](Self::flush_observations) advances (C-14). The
    /// executor's log is append-only and shared across this engine's turns, so a plain high-water
    /// mark attributes each tail to the turn that just ended.
    evidence_flushed: std::sync::atomic::AtomicUsize,
}

/// Outcome of executing a plan the user already reviewed through a surface's plan mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewedPlanOutcome {
    Completed,
    Cancelled,
}

impl FlowEngine {
    /// Assemble an engine: wrap the store/provider in `Arc`, install the reflexive [`EngineLoopHost`] on
    /// the executor (so `plan`/`run_plan` re-enter it), and load the built-in `agent-loop.flux`. This is
    /// the one place the executor⇄host cycle is tied — a plain struct literal can't express it. A
    /// `.flux/agent-loop.flux` in the workspace overrides the built-in loop (parsed if present + valid).
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
        let flow = Arc::new(flow);
        let composites = Arc::new(DynamicComposites::load(executor.context().system.as_ref())?);
        let opts = CompileOptions {
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
        // The loop machinery is the engine's own control flow (call the model, run its plan, record
        // evidence), not a user action — pre-allow it so a turn never prompts to approve `plan`/
        // `run_plan`/`observe`. The inner ops a plan runs still gate individually.
        executor.allow(&[
            "plan",
            "run_plan",
            "ai_segment",
            "observe",
            "evidence",
            "metrics",
        ]);
        composites.validate_base(executor.registry())?;
        let agent_loop = load_agent_loop_with_iterations(&cwd, max_iterations)?;
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
            sticky_groups: std::sync::Mutex::new(std::collections::HashSet::new()),
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

    /// Apply one reasoning policy to the full agent call graph: reflexive planner/repair and
    /// completion calls on the loop host, plus outer one-shot planning and compaction calls.
    pub fn with_reasoning(mut self, thinking: bool, effort: Option<Effort>) -> Self {
        self.thinking = thinking;
        self.effort = effort;
        self.loop_host.set_reasoning(thinking, effort);
        self
    }

    /// Atomically switch the live planner and the session's durable model attribution. Persistence
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

    /// Execute a plan explicitly reviewed by the user (`/plan` then `/run`). The reviewed graph is
    /// one approved scope; dynamically assembled destructive behavior that was not visible in that
    /// graph still re-prompts through the executor. Output and usage stream through `sink` on every
    /// termination path.
    pub async fn run_reviewed_plan_cancellable(
        &self,
        session_id: &str,
        ast: &DraftAst,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
    ) -> Result<ReviewedPlanOutcome> {
        let composites = self.composites.active_for_session(session_id);
        let risk =
            crate::runtime::plan_risk_with_composites(ast, self.executor.registry(), &composites);
        let _scope = self.executor.enter_approved_scope(risk.destructive);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let channel: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
            crate::loop_host::ChannelSink::new(tx),
        ));
        self.loop_host.set_turn(
            session_id.to_string(),
            Some(self.system_prompt.clone()),
            channel.clone(),
            None,
            None,
        );
        let mut shared = crate::loop_host::SharedSink::new(channel);
        let result = {
            let fut = crate::runtime::execute_flow(
                &self.flow,
                &self.executor,
                session_id,
                ast,
                &mut shared,
            );
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break None,
                    maybe = rx.recv() => {
                        if let Some(ev) = maybe {
                            drain_event(ev, sink, show_loop());
                        }
                    }
                    res = &mut fut => break Some(res),
                }
            }
        };
        while let Ok(ev) = rx.try_recv() {
            drain_event(ev, sink, show_loop());
        }
        let usage = self.loop_host.turn_usage();
        let usage = (usage.total() > 0).then_some(usage);

        match result {
            Some(Ok(outcome)) => {
                if !outcome.result.trim().is_empty() {
                    sink.text_delta(outcome.result.trim());
                }
                sink.turn_end(usage);
                Ok(ReviewedPlanOutcome::Completed)
            }
            Some(Err(error)) => {
                sink.turn_end(usage);
                Err(Error::Other(error.to_string()))
            }
            None => {
                sink.turn_end(usage);
                Ok(ReviewedPlanOutcome::Cancelled)
            }
        }
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
        // resume the persisted flow instead of compiling a fresh plan. (`take_suspension` clears it.)
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

        // Agent identity + project context + any skills whose triggers match this turn — prepended to
        // the planner's own instructions inside `compile_turn`.
        let base_system = self.base_system_with_skills(user_input, sink);

        // Evidence-gated surfacing for this turn: probe the workspace signals once and hand the
        // advertised op set to the loop host, so every planner iteration sees the gated catalog.
        let advertised = self.surfaced_for_turn(user_input, sink);

        // Drive the flux-lang agent loop (`agent_loop`) through an OWNED channel sink — the `'static`
        // loop host holds it for reentrant `run_plan` — draining its events onto the borrowed `sink`
        // LIVE (inner ops stream as they happen; the loop-machinery ops are filtered, see `drain_event`).
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
                planner_error(&error)
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

        // Reveal the loop machinery on the surface when `--show-loop`/`FLUX_SHOW_LOOP` is set.
        let reveal = show_loop();
        let outcome = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    while let Ok(ev) = rx.try_recv() { drain_event(ev, sink, reveal); }
                    let subagent_calls = self.subagent_calls_since(subagent_base);
                    self.record_call_usage_events(session_id, turn_id, &subagent_calls);
                    let usage = self.turn_usage(&subagent_calls);
                    let _ = self.events.end_turn(session_id, turn_id, "cancelled", 0, "(turn cancelled)", usage.clone());
                    return self.finish_turn(session_id, turn_id, sink, "(turn cancelled)", true, usage);
                }
                maybe = rx.recv() => {
                    if let Some(ev) = maybe { drain_event(ev, sink, reveal); }
                }
                res = &mut flow_fut => {
                    while let Ok(ev) = rx.try_recv() { drain_event(ev, sink, reveal); }
                    break res;
                }
            }
        };

        // The loop returns `$answer` — the model's prose, grounded in the fed-back results (the `chat`
        // case). On failure (e.g. the planner errored, surfaced through the op envelope) we surface it as
        // the answer so the session shape stays valid and the turn never ends in silence.
        let (answer, tag) = match outcome {
            Ok(o) => {
                let a = o.result.trim().to_string();
                if a.is_empty() {
                    (
                        format!(
                            "Reached the maximum of {} plan iterations for this turn; stopping.",
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

    /// Compile a single instruction into a [`TurnOutput`] using this engine's full catalog + current
    /// session symbols — *without executing the settled result*. The one-shot `--plan` surface uses
    /// this, so what it shows is exactly what the engine would run.
    ///
    /// A-18: this now runs the SAME orient/gather contract normal turns do
    /// ([`Self::compile_with_gather`]) — a complex/context-hungry prompt may auto-run a bounded
    /// number of read-only gather rounds (A-13 enforces them non-mutating at compile time) before
    /// settling on the full execution plan this function returns. Only the settled plan is handed
    /// back; it is never executed here — `flux plan`'s contract ("show me the plan before anything
    /// runs") holds for the FINAL plan even though gather already ran.
    ///
    /// A-42: `sink` streams every gather round's ops/results live (the CLI's one-shot `flux plan`
    /// now has somewhere to render them, matching `plan_turn`'s REPL `/plan`) — see
    /// [`Self::compile_with_gather`]'s doc comment for the channel/drain-loop shape.
    pub async fn compile_once(
        &self,
        session_id: &str,
        prompt: &str,
        sink: &mut dyn AgentSink,
        ask: Option<&dyn crate::compile::AskUser>,
    ) -> Result<TurnOutput> {
        self.composites
            .ensure_session_loaded(&self.flow, session_id)?;
        // Unrelated to the sink threading below: `flux plan` has never observed `groups.active`
        // (`surfaced_op_names` directly, not `surfaced_for_turn`) — preserved as-is, since wiring a
        // live sink for gather rendering is no reason to also start emitting a new observation kind
        // this surface never has.
        let (advertised, _surfaced) = surfaced_op_names(
            self.executor.registry(),
            &self.groups,
            &self.cwd,
            &self.sticky_groups,
            &self.ambient_signals,
            prompt,
        );
        let opts = CompileOptions {
            max_tokens: self.max_tokens,
            thinking: self.thinking,
            effort: self.effort,
            ..CompileOptions::default()
        };
        // A-42: drive `compile_with_gather` over an owned, channel-backed sink and drain the
        // channel onto the caller's borrowed `sink` concurrently — the same ChannelSink/drain-loop
        // shape `run_turn_cancellable` uses, so gather rounds stream live instead of through the
        // internal `NullSink` A-18 shipped with. Usage is folded into `_usage`, deliberately
        // unused — same C-31 stance as before A-18.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::loop_host::SinkEvent>();
        let channel: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
            crate::loop_host::ChannelSink::new(tx),
        ));
        let reveal = show_loop();
        let messages = [Message::user_text(prompt)];
        let fut = self.compile_with_gather(
            session_id,
            &messages,
            Some(&self.system_prompt),
            Some(advertised),
            ask,
            channel,
            opts,
        );
        tokio::pin!(fut);
        let (out, _usage) = loop {
            tokio::select! {
                biased;
                maybe = rx.recv() => {
                    if let Some(ev) = maybe { drain_event(ev, sink, reveal); }
                }
                res = &mut fut => {
                    while let Ok(ev) = rx.try_recv() { drain_event(ev, sink, reveal); }
                    break res;
                }
            }
        };
        out
    }

    /// A-18 — the phased orient/gather contract, shared by [`Self::compile_once`] and
    /// [`Self::plan_turn`]: call `compile_turn` with [`Phase::Orient`], and if the model tags the
    /// result `gather: true`, execute that plan (A-13 already guarantees it is read-only and small
    /// at compile time — the SAME trust `run_plan` already grants a non-mutating plan, so it runs
    /// without an approval prompt) and feed its transcript back through up to
    /// [`GATHER_ROUND_BUDGET`] rounds of [`Phase::Gather`] — the identical bound normal-mode
    /// gather uses (`agent-loop.flux`'s `repeat 3`), not a second invented budget.
    ///
    /// Once the model settles (an ordinary execution plan, or a chat answer) the settled
    /// [`TurnOutput`] is returned UNEXECUTED. If the budget is spent while the model is still
    /// tagging `gather: true`, this deliberately does **not** run that leftover plan (unlike
    /// normal mode's Part-1 graceful degradation, which folds it into the very next execute-loop
    /// round): plan mode's contract is that ONLY gather ever runs automatically, capped at exactly
    /// `GATHER_ROUND_BUDGET` rounds, so the next call is forced into [`Phase::Execute`] (whose
    /// contract rejects `gather: true`) to compel a real settlement instead.
    ///
    /// A-42: every round streams live through `sink` — an owned, clonable `Arc<Mutex<dyn
    /// AgentSink>>`, exactly the handle normal-mode `run_plan` shares via `SharedSink` — instead of
    /// the internal `NullSink` A-18 shipped with. A-18's attempt to thread the caller's plain,
    /// non-'static `&mut dyn AgentSink` straight into this loop hit a well-known NLL limitation
    /// (reborrowing it more than once per iteration — once for `compile_turn`'s thinking-sink,
    /// again for the gather dispatch — unifies the reborrow's lifetime with the loop's back-edge);
    /// an owned `Arc` clone per round sidesteps that entirely, since each round's `SharedSink` is a
    /// brand-new value, never a reborrow of a captured outer reference. The caller (`compile_once`/
    /// `plan_turn`) owns the OTHER half of the channel this `sink` was built from and drains it onto
    /// its own borrowed sink concurrently with this future — the same ChannelSink/drain-loop shape
    /// `run_turn_cancellable` already uses. `thinking_sink` stays `None` (unchanged from A-18): that
    /// would also flip `Request.thinking` on, a real behavioral change this rendering-only story
    /// must not make.
    #[allow(clippy::too_many_arguments)]
    async fn compile_with_gather(
        &self,
        session_id: &str,
        base_conversation: &[Message],
        base_system: Option<&str>,
        advertised: Option<std::collections::HashSet<String>>,
        ask: Option<&dyn crate::compile::AskUser>,
        sink: Arc<std::sync::Mutex<dyn AgentSink>>,
        opts: CompileOptions,
    ) -> (Result<TurnOutput>, Usage) {
        let mut usage_total = Usage::default();
        let mut phase = Phase::Orient;
        let mut feedback: Option<String> = None;
        let mut brief: Option<Brief> = None;
        let mut gather_rounds: u32 = 0;

        loop {
            let mut conversation = base_conversation.to_vec();
            if let Some(fb) = &feedback {
                conversation.push(Message::user_text(fb.clone()));
            }
            if conversation.is_empty() {
                conversation.push(Message::user_text(""));
            }

            // Same per-round freshness as normal mode's `plan()` (loop_host.rs): the op registry
            // is rebuilt each round from the CURRENT composites (so any newly registered composite
            // is visible) but reuses the ONE advertised-name set computed for this whole turn — A-04
            // gating stays turn-stable rather than re-probing workspace signals every round.
            let composites = self.composites.active_for_session(session_id);
            let mut ops =
                OpRegistry::new(self.executor.registry()).with_owned_composites(composites.clone());
            if let Some(adv) = advertised.clone() {
                ops = ops.with_advertised(adv);
            }
            let view = match self.flow.view(session_id) {
                Ok(v) => v,
                Err(e) => return (Err(e), usage_total),
            };
            let view_ref = (!view.symbols.is_empty()).then_some(&view);

            // A-42 (A-15 parity): announce this round's phase on the live sink before the
            // otherwise-silent planner call — mirrors `EngineLoopHost::plan`'s own `loop.phase`
            // observation (loop_host.rs), so the CLI's phase-aware spinner label
            // ("orienting…"/"gathering…") applies to plan mode's rounds too. Sink-only (unlike
            // `plan()`, this does not also record it to durable evidence — the phased Rust loop
            // isn't the reflexive `plan()` op, and this story is rendering-only).
            crate::loop_host::SharedSink::new(sink.clone()).observation(
                &flux_evidence::Observation::new(
                    "loop.phase",
                    flux_evidence::Phase::Turn,
                    serde_json::json!({ "phase": phase.as_str() }),
                ),
            );
            let (out, call_usage) = {
                // Scoped so the spinner stops the instant `compile_turn` returns — the round's
                // gather dispatch below streams its own tool_call/tool_result progress instead.
                let _planning = crate::loop_host::PlanningGuard::start(sink.clone());
                compile_turn(
                    &*self.provider,
                    &self.model,
                    &conversation,
                    base_system,
                    &ops,
                    view_ref,
                    ask,
                    None,
                    opts.clone(),
                    phase,
                )
                .await
            };
            usage_total.accumulate(&call_usage);

            let out = match out {
                Ok(v) => v,
                Err(e) => return (Err(e), usage_total),
            };
            let compiled = match out {
                TurnOutput::Chat(text) => return (Ok(TurnOutput::Chat(text)), usage_total),
                TurnOutput::Plan(c) => c,
            };

            if !compiled.gather {
                // Settled: either the model emitted the full execution plan directly (the common,
                // zero-added-latency case), or `phase == Execute` forced settlement after the
                // gather budget was spent. Either way, return it UNEXECUTED.
                return (Ok(TurnOutput::Plan(compiled)), usage_total);
            }

            if gather_rounds >= GATHER_ROUND_BUDGET {
                // Budget spent and the model is still gathering — force a real settlement rather
                // than running this leftover plan (see the doc comment above).
                phase = Phase::Execute;
                continue;
            }

            // Automatic, read-only gather round: A-13's `gather_violation` check inside
            // `compile_turn` already refused this plan if it named a mutating op or exceeded the
            // node cap, so running it here is exactly as safe as `run_plan` already treats any
            // non-mutating plan (no approval prompt). A-42: dispatched through the SAME shared
            // sink every other round uses (no more `NullSink`), so the ops/results stream live.
            gather_rounds += 1;
            let mut round_sink = crate::loop_host::SharedSink::new(sink.clone());
            let outcome = execute_flow_resumable_with_composites(
                self.flow.as_ref(),
                self.executor.as_ref(),
                session_id,
                &compiled.ast,
                &composites,
                None,
                None,
                &mut round_sink,
            )
            .await;

            let mut fb = match outcome {
                Ok(o) => {
                    let mut fb = o.transcript;
                    if let Some(halt) = &o.failure {
                        fb = format!(
                            "{fb}\n\n[gather step {} failed] {}\n{}",
                            halt.node.0 + 1,
                            halt.message,
                            crate::loop_host::halt_guidance(halt.kind, halt.node),
                        );
                    }
                    fb
                }
                // An infrastructure failure only (store IO, …) — a statement's own failure is
                // reified onto `outcome.failure` above instead. Feed the error back so the next
                // round can adjust, exactly like `run_plan` does for the same case.
                Err(e) => {
                    format!("[plan error] {e}\nAdjust and emit another plan, or answer in prose.")
                }
            };
            fb = crate::loop_host::cap_loop_feedback(fb);
            if let Some(b) = compiled.brief.clone().or_else(|| brief.clone()) {
                fb = format!("{}\n\n{fb}", crate::loop_host::format_brief(&b));
                brief = Some(b);
            }
            feedback = Some(fb);
            phase = Phase::Gather;
        }
    }

    /// A plan-mode turn (the REPL `/plan` toggle): run the phased orient/gather contract
    /// ([`Self::compile_with_gather`], A-18) over the conversation, render the settled result, and
    /// persist it as the assistant turn (so a refinement sees it) — but DO NOT execute it. A
    /// complex/context-hungry prompt may auto-run a bounded number of read-only gather rounds first
    /// (A-13 enforces them non-mutating at compile time); only the settled plan is ever surfaced.
    /// Returns the AST for the caller to hold and run later (`/run`); a chat answer is surfaced and
    /// returns `None`. Abortable via `cancel`: a Ctrl-C mid-compose (whether during a planner call
    /// or a gather round's execution) drops the in-flight work and returns `Ok(None)` (nothing to
    /// run).
    ///
    /// A-42: `sink` streams every gather round's ops/results live, and each round brackets its own
    /// `planning(true/false)` (moved inside [`Self::compile_with_gather`], mirroring normal-mode
    /// `plan()`'s per-round [`crate::loop_host::PlanningGuard`]) rather than one bracket around the
    /// whole phased sequence — see `compile_with_gather`'s doc comment for the channel/drain-loop
    /// shape this reuses from `run_turn_cancellable`.
    pub async fn plan_turn(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
    ) -> Result<Option<DraftAst>> {
        self.events
            .record_message(session_id, &Message::user_text(user_input))?;
        // Same turn boundary as `run_turn_cancellable`: without this, repeated `/plan` turns
        // (whose gather is read-only, so no write ever invalidates) would reuse cached reads
        // across the whole editing session (L-54 review, 2026-07-09).
        self.executor.begin_cache_turn();
        let base_system = self.base_system_with_skills(user_input, sink);
        self.composites
            .ensure_session_loaded(&self.flow, session_id)?;
        // Computed ONCE for the whole phased sequence (mirrors `run_turn_cancellable`'s
        // `surfaced_for_turn`) — A-04 gating stays turn-stable across orient/gather rounds rather
        // than re-probing workspace signals (and re-observing `groups.active`) every round.
        let advertised = self.surfaced_for_turn(user_input, sink);
        let opts = CompileOptions {
            max_tokens: self.max_tokens,
            thinking: self.thinking,
            effort: self.effort,
            ..CompileOptions::default()
        };
        let conversation = self.events.conversation(session_id)?;
        // A-42: an owned, channel-backed sink for `compile_with_gather` to share across rounds;
        // drained onto the caller's borrowed `sink` concurrently, racing `cancel` too so Ctrl-C
        // still drops the in-flight request/dispatch and returns promptly (dropping `fut` aborts
        // whatever it was awaiting).
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::loop_host::SinkEvent>();
        let channel: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
            crate::loop_host::ChannelSink::new(tx),
        ));
        let reveal = show_loop();
        let out = {
            let fut = self.compile_with_gather(
                session_id,
                &conversation,
                Some(&base_system),
                Some(advertised),
                None,
                channel,
                opts,
            );
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        while let Ok(ev) = rx.try_recv() { drain_event(ev, sink, reveal); }
                        break None;
                    }
                    maybe = rx.recv() => {
                        if let Some(ev) = maybe { drain_event(ev, sink, reveal); }
                    }
                    res = &mut fut => {
                        while let Ok(ev) = rx.try_recv() { drain_event(ev, sink, reveal); }
                        break Some(res);
                    }
                }
            }
        };
        let Some(out) = out else {
            // Cancelled mid-compose: nothing to run; end the turn cleanly.
            sink.turn_end(None);
            return Ok(None);
        };
        // Usage first (C-31 shape), then the outcome: this surface renders usage on `turn_end`
        // only — there is no per-call ledger on the `/plan` path, so an errored compose's spend
        // has nowhere durable to go and the drop below is deliberate, not a leak.
        let (out, usage) = out;
        // Surface a provider failure (credit, auth, rate limit, transport) with a readable message
        // rather than the raw API JSON body — the REPL prints this `error:` line directly.
        let out = out.map_err(|e| flux_core::Error::Other(planner_error(&e)))?;
        // This turn's total spend across every planner call it made (orient + any gather/settle
        // rounds) — `compile_with_gather` accumulates them all, so this is no longer always
        // exactly one call's usage as it was pre-A-18.
        let usage = (usage.total() > 0).then_some(usage);

        match out {
            // C-17 (F2) backstop: a diagnostics-carrying plan must never be handed back for `/run`.
            // `compile_turn` no longer produces one (it repairs or rejects the turn), but this is a
            // safety gate, so the executing surface enforces the `Compiled` contract ("surfaced
            // rather than executed") itself instead of trusting the compiler's invariant.
            TurnOutput::Plan(compiled) if !compiled.diagnostics.is_empty() => {
                let text = format!(
                    "The proposed plan was rejected — it references operations this workspace \
                     does not know: {}. Nothing will run; refine the request and try again.",
                    compiled
                        .diagnostics
                        .iter()
                        .map(|d| d.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                );
                sink.text_delta(&text);
                self.events.record_message(
                    session_id,
                    &Message::assistant(vec![ContentBlock::Text { text }]),
                )?;
                sink.turn_end(usage);
                Ok(None)
            }
            TurnOutput::Plan(compiled) => {
                let rendered = crate::render::render_pretty(&compiled.ast);
                sink.observation(&self.plan_observation(session_id, &compiled.ast));
                self.events.record_message(
                    session_id,
                    &Message::assistant(vec![ContentBlock::Text {
                        text: format!("Proposed plan:\n{rendered}"),
                    }]),
                )?;
                sink.turn_end(usage);
                Ok(Some(compiled.ast))
            }
            TurnOutput::Chat(text) => {
                sink.text_delta(&text);
                self.events.record_message(
                    session_id,
                    &Message::assistant(vec![ContentBlock::Text { text }]),
                )?;
                sink.turn_end(usage);
                Ok(None)
            }
        }
    }

    /// The `flow.plan` observation surfaced before a plan executes: the plain-rendered tree (for any
    /// sink), the AST (so a terminal surface can syntax-highlight it), and the risk preview (for a badge).
    fn plan_observation(
        &self,
        session_id: &str,
        ast: &crate::ast::DraftAst,
    ) -> flux_evidence::Observation {
        let composites = self.composites.active_for_session(session_id);
        let risk =
            crate::runtime::plan_risk_with_composites(ast, self.executor.registry(), &composites);
        flux_evidence::Observation::new(
            "flow.plan",
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "plan": crate::render::render_pretty(ast),
                "plan_ast": serde_json::to_value(ast).unwrap_or(serde_json::Value::Null),
                "risk": risk.summary(),
                "ops": risk.ops.len(),
            }),
        )
    }

    /// This turn's advertised op-name set, computed once per turn and handed to the loop host so the
    /// self-hosted loop's `plan` op advertises (and enforces) the SAME gated catalog as the preview
    /// paths — before A-04 the loop built an ungated registry, so every op (incl. `bash`) was
    /// advertised every turn and the opt-in `shell` group gated nothing. Records the `groups.active`
    /// observation when gating is on.
    fn surfaced_for_turn(
        &self,
        user_input: &str,
        sink: &mut dyn AgentSink,
    ) -> std::collections::HashSet<String> {
        let (advertised, surfaced) = surfaced_op_names(
            self.executor.registry(),
            &self.groups,
            &self.cwd,
            &self.sticky_groups,
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

    /// The agent identity + project context + explicitly enabled skills — the base the planner prompt
    /// is appended to (shared by `run_turn` and `plan_turn`).
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
    /// token tally (the planner calls summed), surfaced to the sink for the turn-end annotation.
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

    /// Append one `EventKind::CallUsage` per planner call this turn made (`self.loop_host.turn_calls`)
    /// PLUS one per sub-agent this turn spawned (`subagent_calls`), each stamped with the model that
    /// produced it — the planner's active model at the time, or the sub-agent's own resolved model —
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
    /// flow's own **authored prompt** (its last emitted view) as the assistant turn — no planner is
    /// invoked for this deterministic skeleton. A flow that completes without any `await` surfaces
    /// its result as a single completed turn.
    ///
    /// Turn 1 is flow-authored, not user-authored: the flow speaks first, so no user message is
    /// recorded (the session log opens with the authored prompt). Runs over the shared
    /// `Arc<Executor>`, so the authorization → approval → guarded-IO envelope applies exactly as on
    /// the planner path — a `RiskApprover` gates a flow-driven op identically to a planned one.
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
        // A fresh flow drive bypasses the planner/loop host exactly like a resume: the only billable
        // spend is `task` sub-agents in the flow body. Snapshot the count so only THIS turn's fold in
        // (`record_resume_usage` reads observations since this base — the same helper resume uses).
        let subagent_base = self.executor.evidence().by_kind("subagent.usage").count();

        // Arm the reflexive loop host so a top-level `ai_segment` in the flow can delegate a bounded
        // run of model turns (D-131 Phase B). No user utterance drives this turn, so skills match
        // against an empty input. For a flow with no `ai_segment` this is harmless overhead — the
        // authored prompt is still surfaced explicitly below (Phase A unchanged).
        let base_system = self.base_system_with_skills("", sink);
        let advertised = self.surfaced_for_turn("", sink);
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
        // `sink` LIVE — so an `ai_segment`'s inner plan/run_plan/leaf ops stream as they happen (the
        // machinery ops are filtered by `drain_event`). Mirrors `run_turn_cancellable`'s plumbing.
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
    /// suspend again on a later `await` (persist + wait) or complete (surface its result). Bypasses the
    /// planner entirely — a resume is deterministic continuation, not a fresh compile.
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
        // planner via a top-level `ai_segment` (D-131 Phase B), whose spend rides the loop host's
        // per-turn tally, reset by the `set_turn` below.
        let subagent_base = self.executor.evidence().by_kind("subagent.usage").count();

        let input = flux_lang::ast::Value::String(user_input.to_string());

        // Arm the reflexive loop host so a top-level `ai_segment` AFTER the resumed `await` can
        // delegate a bounded run of model turns (D-131 Phase B). Skills match the reply text; a
        // resume with no segment pays only harmless overhead.
        let base_system = self.base_system_with_skills(user_input, sink);
        let advertised = self.surfaced_for_turn(user_input, sink);
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
    /// host before running, so this folds BOTH the loop host's planner calls — non-zero only when a
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
    /// `Result`, mirroring `compile_turn`: a stream can report tokens before failing, and those
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
    /// A workspace override read from this path (`.flux/agent-loop.flux`).
    Override(std::path::PathBuf),
}

/// The advertised op-name set for a turn, plus the active group set when gating is on (`None` when
/// the manifest is empty = gating disabled). The ONE computation both catalog paths share — the
/// engine's preview registries and the loop host's per-iteration planner — so they can't drift.
///
/// Gating disabled still excludes the never-surfaced loop machinery (the `reflect` group —
/// `plan`/`run_plan`): those are registered for dispatch (the agent loop calls them) but must never
/// enter the model-facing catalog. With a manifest, `advertised_op_names` already excludes them.
pub(crate) fn surfaced_op_names(
    reg: &flux_runtime::ToolRegistry,
    groups: &[flux_evidence::ToolGroup],
    cwd: &std::path::Path,
    sticky: &std::sync::Mutex<std::collections::HashSet<String>>,
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
    // sticky union and advertise from the ACCUMULATED set. `resolve_active_groups` is stateless, so a
    // marker file appearing then disappearing would otherwise rewrite segment A's op catalog and miss
    // the cached `tools+A+phase+B` prefix; accumulating means the catalog only ever grows and the
    // prefix restabilizes. Advertising is not granting — the approval/policy envelope still gates ops.
    let accumulated = {
        let mut s = sticky.lock().unwrap_or_else(|e| e.into_inner());
        s.extend(active);
        s.clone()
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

/// The compiled-in default agent loop, as readable Flux-Lang text. This is the loop every turn runs
/// unless a workspace overrides it; surfaced by `flux loop show`.
pub fn builtin_agent_loop() -> &'static str {
    include_str!("../assets/agent-loop.flux")
}

/// Resolve the active agent loop's source + text: a readable `.flux/agent-loop.flux` override if the
/// workspace has one, otherwise the built-in. Reads the file but does not parse it (see
/// [`load_agent_loop`]); an unreadable override falls back to the built-in, matching the engine.
pub fn agent_loop_source(cwd: &std::path::Path) -> (LoopSource, String) {
    let override_path = cwd.join(".flux").join("agent-loop.flux");
    match std::fs::read_to_string(&override_path) {
        Ok(src) => (LoopSource::Override(override_path), src),
        Err(_) => (LoopSource::Builtin, builtin_agent_loop().to_string()),
    }
}

/// Load the agent loop: a `.flux/agent-loop.flux` in the workspace overrides the built-in (so a project
/// can shape its own loop), otherwise the compiled-in `agent-loop.flux`. Parsed as a [`DraftAst`].
/// `Err` if an override exists but is not valid Flux-Lang — `flux loop show` surfaces that.
pub fn load_agent_loop(cwd: &std::path::Path) -> Result<DraftAst> {
    let (_source, src) = agent_loop_source(cwd);
    // The loop is written in readable Flux-Lang text (it round-trips through `format`/`parse`), so parse
    // it through the language surface rather than the JSON wire form.
    flux_lang::parse::parse(&src)
        .map_err(|e| flux_core::Error::Other(format!("agent-loop.flux: invalid flow: {e}")))
}

/// Load the one checked-in/overridden agent loop and replace its execute-pass `repeat … until
/// $done` budget with the engine setting. Flux-Lang repeat bounds are literals by design, so this is
/// configuration of the parsed program, not a second Rust turn loop. Custom loops without that
/// conventional execute pass remain byte-for-byte as authored.
fn load_agent_loop_with_iterations(
    cwd: &std::path::Path,
    max_iterations: usize,
) -> Result<DraftAst> {
    let max = u32::try_from(max_iterations).map_err(|_| {
        flux_core::Error::Other(format!(
            "max_iterations {max_iterations} exceeds Flux-Lang's u32 repeat bound"
        ))
    })?;
    if max == 0 {
        return Err(flux_core::Error::Other(
            "max_iterations must be greater than zero".into(),
        ));
    }
    let mut ast = load_agent_loop(cwd)?;
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
    Ok(ast)
}

/// The loop-machinery ops a turn dispatches to *drive* the loop (not to do the user's work). Their
/// tool-call/result events are filtered out of the user-facing sink so the surface shows the actual
/// operations (`read`/`edit`/`bash`/…) the inner `run_plan` performs, not the plumbing.
const MACHINERY_OPS: &[&str] = &[
    "plan",
    "run_plan",
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
/// iterate (`plan → run_plan → observe`) instead of only the work the inner plan performs.
pub fn show_loop() -> bool {
    flux_system::env_truthy("FLUX_SHOW_LOOP")
}

/// Whether the OUTER agent loop's structure is traced (A-39) — the CLI `--trace-loop`, exported as
/// `FLUX_TRACE_LOOP` so the engine reads it without new plumbing. When set, [`execute_flow_traced`]
/// emits one live `loop.round`/`loop.node` observation per outer-loop round/structural node
/// executed. Scoped to the outer loop ONLY: inner `run_plan`, `flow run`, and resume paths call
/// plain `execute_flow` and never see this flag.
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

/// Render a planner failure for the user. A provider API error carries the raw JSON response body;
/// unwrap it to `error.message` so a credit/billing/auth/rate-limit failure reads as a plain sentence
/// instead of a JSON dump. Every other error uses its own `Display`.
pub fn planner_error(e: &flux_core::Error) -> String {
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
        // A-33: a decode-class error that escaped the planner's own step-budgeted retry loop (e.g.
        // from a one-shot completion call rather than the turn loop) — the same friendly framing as
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;

    use flux_core::{Error, StopReason};
    use flux_provider::{ChunkStream, Request};
    use flux_runtime::{
        AllowApprover, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
    };
    use flux_spec::ToolSpec;
    use flux_system::{System, Workspace};

    use crate::ast::SymbolName;

    /// A provider that replays canned chunk sequences, one per `stream()` call.
    struct MockProvider {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let chunks = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// A provider that counts `stream()` calls — for asserting the deterministic flow-driven path
    /// (D-131 `start_flow_turn` + resume) makes ZERO planner invocations (invariant 1). If it were
    /// ever called, the counter trips the assertion; the returned prose keeps a stray call from
    /// failing the turn in a confusing way.
    struct CountingProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for CountingProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = prose("(unexpected planner call)");
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// A counting provider whose every `plan` round emits a plan that binds `$slots` to a non-empty
    /// value — for the `ai_segment` `until` early-exit test (D-131 Phase B): the predicate should end
    /// the segment after the FIRST round even though `max_rounds` is larger.
    struct BindSlotsProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for BindSlotsProvider {
        fn name(&self) -> &str {
            "bind-slots"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = emit_plan(json!({
                "body": [{
                    "kind": "bind",
                    "name": "slots",
                    "value": { "kind": "call", "op": "echo",
                               "args": [{ "kind": "lit", "value": "filled" }] }
                }]
            }));
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// Build an authored `$_ = ai_segment({goal, tools, max_rounds, until?})` call node (D-131). A lone
    /// object argument is the op's named input map (runtime `call_named_input`).
    fn ai_segment_call(
        goal: &str,
        tools: &[&str],
        max_rounds: u32,
        until: Option<&str>,
    ) -> flux_lang::ast::Node {
        use flux_lang::ast::Node;
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "goal".to_string(),
            Box::new(Node::Lit { value: json!(goal) }),
        );
        fields.insert(
            "tools".to_string(),
            Box::new(Node::Lit {
                value: json!(tools),
            }),
        );
        fields.insert(
            "max_rounds".to_string(),
            Box::new(Node::Lit {
                value: json!(max_rounds),
            }),
        );
        if let Some(u) = until {
            fields.insert("until".to_string(), Box::new(Node::Lit { value: json!(u) }));
        }
        Node::Call {
            op: "ai_segment".into(),
            args: vec![Node::Obj { fields }],
        }
    }

    /// Echo the `text` param back as content (with a real schema so positional args map).
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "echo",
                "echo text",
                json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
            )
        }
        async fn execute(&self, _c: &ToolContext, params: serde_json::Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    #[derive(Default)]
    struct CollectSink {
        text: String,
        tools: Vec<String>,
        /// A-42: the op name of every `tool_result` this sink observed, in order — lets a test
        /// assert a gather round's dispatch streamed both the call AND its result live, not just
        /// one half of the pair.
        results: Vec<String>,
        turn_ends: Vec<Option<Usage>>,
    }
    impl AgentSink for CollectSink {
        fn text_delta(&mut self, t: &str) {
            self.text.push_str(t);
        }
        fn tool_call(&mut self, name: &str, _input: &serde_json::Value) {
            self.tools.push(name.to_string());
        }
        fn tool_result(&mut self, name: &str, _result: &ToolResult) {
            self.results.push(name.to_string());
        }
        fn turn_end(&mut self, usage: Option<Usage>) {
            self.turn_ends.push(usage);
        }
    }

    /// One model turn that emits an `emit_plan` tool call carrying `ast` (and an optional `complete`
    /// directive whose `instructions` are the given string).
    fn emit_plan(ast: serde_json::Value) -> Vec<Chunk> {
        emit_plan_complete(ast, None)
    }
    fn emit_plan_complete(ast: serde_json::Value, complete: Option<&str>) -> Vec<Chunk> {
        let mut input = json!({ "ast": ast });
        if let Some(instructions) = complete {
            input["complete"] = json!({ "instructions": instructions });
        }
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: "p1".into(),
                name: "emit_plan".into(),
                input,
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    /// One model turn that emits a bounded read-only `gather: true` plan (A-14) carrying `ast`.
    fn emit_gather_plan(ast: serde_json::Value) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: "p1".into(),
                name: "emit_plan".into(),
                input: json!({
                    "ast": ast,
                    "gather": true,
                    "brief": { "goal": "investigate", "needs": ["context"] },
                }),
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    /// One model turn that answers in prose (a chat turn).
    fn prose(text: &str) -> Vec<Chunk> {
        vec![
            Chunk::TextDelta(text.to_string()),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ]
    }

    /// Like [`prose`], but with a `Usage` chunk — so a test can assert usage rode back out AND was
    /// attributed to the model active at the time of this call (C-06).
    fn prose_with_usage(text: &str, usage: Usage) -> Vec<Chunk> {
        vec![
            Chunk::TextDelta(text.to_string()),
            Chunk::Usage(usage),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ]
    }

    /// Replays canned responses AND records every `Request` — so a test can distinguish a full
    /// planner round (tools + op catalog) from the toolless completion render (A-06). The plain
    /// [`MockProvider`] cannot: both consume the next queued response identically.
    struct CaptureProvider {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
        requests: Arc<Mutex<Vec<Request>>>,
    }

    #[async_trait]
    impl Provider for CaptureProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            self.requests.lock().unwrap().push(req);
            let chunks = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// An op that always fails at execution (but passes compile-time analysis) — drives the
    /// plan-error feedback path.
    struct BoomTool;
    #[async_trait]
    impl Tool for BoomTool {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only("boom", "always fails", json!({"type": "object"}))
        }
        async fn execute(
            &self,
            _c: &ToolContext,
            _params: serde_json::Value,
        ) -> Result<ToolResult> {
            Ok(ToolResult::error("boom: deliberate failure"))
        }
    }

    /// A MUTATING op (declared `Effect::Write`) — used to prove A-13's `gather_violation` gate
    /// holds through the plan-mode seam (A-18) too. Records every dispatch into a shared log so a
    /// test can assert it was NEVER called, not merely that its symbol wasn't bound.
    struct WriteTool(Arc<Mutex<Vec<String>>>);
    #[async_trait]
    impl Tool for WriteTool {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only("write_file", "write a file", json!({"type": "object"}))
                .with_effects(vec![flux_spec::Effect::Write])
                .with_risk(flux_spec::Risk::Medium)
        }
        async fn execute(
            &self,
            _c: &ToolContext,
            _params: serde_json::Value,
        ) -> Result<ToolResult> {
            self.0.lock().unwrap().push("write_file".to_string());
            Ok(ToolResult::ok("wrote"))
        }
    }

    /// A provider whose every `stream()` fails — simulates a provider/API failure (e.g. credit
    /// exhausted) so the engine's error-surfacing path is exercised.
    struct FailProvider {
        err: Box<dyn Fn() -> Error + Send + Sync>,
    }

    #[async_trait]
    impl Provider for FailProvider {
        fn name(&self) -> &str {
            "fail"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Err((self.err)())
        }
    }

    /// A provider whose stream emits nothing and never completes — only cancellation can end a call to
    /// it. Used to prove the compose paths abort promptly on Ctrl-C (mirrors flux-agent's test).
    struct BlockingProvider;

    #[async_trait]
    impl Provider for BlockingProvider {
        fn name(&self) -> &str {
            "blocking"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Ok(Box::pin(futures::stream::pending::<Result<Chunk>>()))
        }
    }

    /// A provider that changes its emitted plan on every request, so the loop's identical-plan
    /// guard cannot shorten the configured-iteration regression below. Each request binds a fresh
    /// symbol through `echo`, making the number of actual inner dispatches observable too.
    struct FreshPlanProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for FreshPlanProvider {
        fn name(&self) -> &str {
            "fresh-plan"
        }

        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = emit_plan(json!({
                "body": [{
                    "kind": "bind",
                    "name": format!("value_{n}"),
                    "value": {
                        "kind": "call",
                        "op": "echo",
                        "args": [{ "kind": "lit", "value": format!("round-{n}") }]
                    }
                }]
            }));
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// The compaction request either fails before a stream exists or reports usage and then fails
    /// mid-stream. A second request would return prose, making an accidental fallback visible in
    /// the request count instead of hanging the regression.
    struct CompactionFailProvider {
        calls: Arc<AtomicUsize>,
        fail_midstream: bool,
        usage: Usage,
    }

    #[async_trait]
    impl Provider for CompactionFailProvider {
        fn name(&self) -> &str {
            "compact-fail"
        }

        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call > 0 {
                let chunks = prose("fallback response");
                return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
            }
            if !self.fail_midstream {
                return Err(Error::Other("compaction provider unavailable".into()));
            }
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(Chunk::Usage(self.usage.clone())),
                Err(Error::StreamDecode("broken compaction stream".into())),
            ])))
        }
    }

    fn engine_with(responses: VecDeque<Vec<Chunk>>, events: Arc<EventStore>) -> FlowEngine {
        engine_with_provider(
            Box::new(MockProvider {
                responses: Mutex::new(responses),
            }),
            events,
        )
    }

    fn engine_with_provider(provider: Box<dyn Provider>, events: Arc<EventStore>) -> FlowEngine {
        engine_with_groups(provider, events, Vec::new())
    }

    fn engine_with_groups(
        provider: Box<dyn Provider>,
        events: Arc<EventStore>,
        groups: Vec<flux_evidence::ToolGroup>,
    ) -> FlowEngine {
        engine_with_groups_and_iterations(provider, events, groups, 5)
    }

    fn engine_with_groups_and_iterations(
        provider: Box<dyn Provider>,
        events: Arc<EventStore>,
        groups: Vec<flux_evidence::ToolGroup>,
        max_iterations: usize,
    ) -> FlowEngine {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-engine-{}-{}",
            std::process::id(),
            events.latest_session().ok().flatten().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        registry.register(Arc::new(BoomTool));
        // The flux-lang agent loop calls these — register them so a turn can run.
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(
                &[
                    "echo".into(),
                    "boom".into(),
                    "plan".into(),
                    "run_plan".into(),
                    "observe".into(),
                ],
                &[],
            ),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        FlowEngine::assemble(
            Arc::from(provider),
            executor,
            events,
            flow,
            "mock".into(),
            "test".into(),
            1024,
            max_iterations,
            Vec::new(),
            0,
            groups,
            dir,
        )
        .unwrap()
    }

    /// Like [`engine_with`], but with an extra registered [`WriteTool`] (a mutating op) — for tests
    /// proving A-13's gather enforcement holds through the plan-mode seam (A-18). Returns the
    /// engine plus the shared dispatch log `WriteTool` records into, so a test can assert it stayed
    /// empty.
    fn engine_with_write_tool(
        responses: VecDeque<Vec<Chunk>>,
        events: Arc<EventStore>,
    ) -> (FlowEngine, Arc<Mutex<Vec<String>>>) {
        let write_calls = Arc::new(Mutex::new(Vec::new()));
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-engine-write-{}-{}",
            std::process::id(),
            events.latest_session().ok().flatten().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        registry.register(Arc::new(WriteTool(write_calls.clone())));
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(
                &[
                    "echo".into(),
                    "write_file".into(),
                    "plan".into(),
                    "run_plan".into(),
                    "observe".into(),
                ],
                &[],
            ),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = FlowEngine::assemble(
            Arc::from(Box::new(MockProvider {
                responses: Mutex::new(responses),
            }) as Box<dyn Provider>),
            executor,
            events,
            flow,
            "mock".into(),
            "test".into(),
            1024,
            5,
            Vec::new(),
            0,
            Vec::new(),
            dir,
        )
        .unwrap();
        (engine, write_calls)
    }

    /// The built-in `agent-loop.flux` is readable Flux-Lang text: it parses, formats back to a stable
    /// (idempotent) text, and uses NO `@json` escape — every construct it uses has a native surface.
    #[test]
    fn builtin_agent_loop_is_readable_and_round_trips() {
        const SRC: &str = include_str!("../assets/agent-loop.flux");
        let ast = flux_lang::parse::parse(SRC).expect("agent-loop.flux parses");
        let formatted = flux_lang::format::format(&ast);
        assert!(
            !formatted.contains("@json"),
            "the loop must be fully readable (no @json):\n{formatted}"
        );
        let reparsed = flux_lang::parse::parse(&formatted).expect("formatted loop re-parses");
        assert_eq!(
            ast, reparsed,
            "agent-loop.flux round-trips through format/parse"
        );
    }

    /// A-18: `FlowEngine::compile_with_gather` (plan mode's bounded gather) reads
    /// `compile::GATHER_ROUND_BUDGET` for its round cap instead of a second hard-coded number —
    /// this asserts that constant still matches the literal `repeat 3` normal mode's Pass 2 spells
    /// in the built-in loop text, so the two surfaces cannot silently drift onto different bounds.
    #[test]
    fn agent_loop_flux_gather_budget_matches_the_shared_constant() {
        const SRC: &str = include_str!("../assets/agent-loop.flux");
        let needle = format!("repeat {}", crate::compile::GATHER_ROUND_BUDGET);
        assert!(
            SRC.contains(&needle),
            "agent-loop.flux's gather pass should read `{needle}` — update either the asset or \
             `compile::GATHER_ROUND_BUDGET` so normal-mode and plan-mode gather stay bounded \
             identically"
        );
    }

    /// C-50: the checked-in source's `repeat 25` is only its readable default template; engine
    /// assembly rewrites the execute pass to the configured value in the actual AST it runs.
    #[test]
    fn configured_agent_loop_replaces_the_literal_execute_budget() {
        let dir = std::env::temp_dir().join(format!("flux-loop-budget-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ast = load_agent_loop_with_iterations(&dir, 7).unwrap();
        let execute_max = ast.body.iter().find_map(|node| match node {
            crate::ast::Node::Repeat {
                max,
                until: Some(until),
                ..
            } if matches!(until.as_ref(), crate::ast::Node::Var { name } if name.0 == "done") => {
                Some(*max)
            }
            _ => None,
        });
        assert_eq!(execute_max, Some(7));
        std::fs::remove_dir_all(dir).ok();
    }

    /// `drain_event` hides the loop machinery by default and reveals it under `--show-loop`: a `plan`
    /// tool-call is dropped when `reveal=false`, kept when `reveal=true`, while real work (`read`)
    /// always streams.
    #[test]
    fn drain_event_hides_machinery_unless_revealed() {
        use crate::loop_host::SinkEvent;
        let plan_call = || SinkEvent::ToolCall("plan".into(), json!({}));
        let read_call = || SinkEvent::ToolCall("read".into(), json!({}));

        let mut hidden = CollectSink::default();
        drain_event(plan_call(), &mut hidden, false);
        drain_event(read_call(), &mut hidden, false);
        assert_eq!(hidden.tools, vec!["read"], "machinery filtered by default");

        let mut revealed = CollectSink::default();
        drain_event(plan_call(), &mut revealed, true);
        drain_event(read_call(), &mut revealed, true);
        assert_eq!(
            revealed.tools,
            vec!["plan", "read"],
            "--show-loop reveals the machinery too"
        );
    }

    /// `agent_loop_source` returns the built-in when no override exists, and the override (with its
    /// path + text) when the workspace has one.
    #[test]
    fn agent_loop_source_picks_override_over_builtin() {
        let dir = std::env::temp_dir().join(format!("flux-loop-src-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (source, text) = agent_loop_source(&dir);
        assert_eq!(source, LoopSource::Builtin);
        assert_eq!(text, builtin_agent_loop());

        let override_path = dir.join(".flux").join("agent-loop.flux");
        std::fs::create_dir_all(override_path.parent().unwrap()).unwrap();
        std::fs::write(
            &override_path,
            "flow agent-loop -> string\n  return \"hi\"\n",
        )
        .unwrap();
        let (source, text) = agent_loop_source(&dir);
        assert_eq!(source, LoopSource::Override(override_path));
        assert!(text.contains("return \"hi\""));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn engine_plans_executes_and_keeps_session_shape() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();

        // Round 1: the model emits a plan binding $greeting = echo("hi"). Round 2: it answers in prose.
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("Done.")]);

        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine
            .run_turn(&sid, "send the renewal", &mut sink)
            .await
            .unwrap();

        // The plan executed through dispatch: echo ran, $greeting bound, trace recorded.
        assert_eq!(sink.tools, vec!["echo"]);
        let vid = engine
            .flow
            .resolve(&sid, &SymbolName("greeting".into()))
            .unwrap();
        assert!(vid.is_some());
        assert!(!engine.flow.events(&sid).unwrap().is_empty());

        // Session log is pure user/assistant-text alternation: user input, then ONE assistant summary.
        let msgs = store.conversation(&sid).unwrap();
        assert_eq!(msgs.len(), 2, "user + one assistant summary");
        assert!(msgs.iter().all(|m| !m.content.is_empty()));
        assert_eq!(msgs[0].role, flux_core::Role::User);
        assert_eq!(msgs[1].role, flux_core::Role::Assistant);
        assert!(msgs[1].text().contains("Done."));
        // No tool_use/tool_result ever lands in the persisted log.
        assert!(msgs.iter().all(|m| m
            .content
            .iter()
            .all(|b| matches!(b, ContentBlock::Text { .. }))));
    }

    #[tokio::test]
    async fn plan_without_complete_loops_to_prose() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        // No `complete` ⇒ the standard agent loop: run the plan, feed results back, and the model ends
        // the turn by answering in prose the next round.
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("Echoed hi.")]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "echo hi", &mut sink).await.unwrap();

        assert_eq!(sink.tools, vec!["echo"], "the plan executed");
        let msgs = store.conversation(&sid).unwrap();
        assert_eq!(msgs.len(), 2, "user + the prose answer");
        assert!(msgs[1].text().contains("Echoed hi."));
    }

    /// `/plan` (plan_turn) is interruptible: a Ctrl-C mid-compose cancels the in-flight planner call
    /// and returns promptly with `Ok(None)` (nothing to run), rather than blocking until the model
    /// replies. Without the `select!` on `cancel`, this would hang on the never-completing stream.
    #[tokio::test]
    async fn plan_turn_aborts_an_in_flight_compose() {
        use std::time::Duration;

        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("blocking").unwrap();
        let engine = engine_with_provider(Box::new(BlockingProvider), store.clone());

        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c2.cancel();
        });

        let mut sink = CollectSink::default();
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            engine.plan_turn(&sid, "compose a plan", &mut sink, &cancel),
        )
        .await
        .expect("plan_turn did not return after cancellation")
        .unwrap();

        assert!(out.is_none(), "a cancelled compose yields no plan to run");
    }

    /// C-17 (F2): a plan that never passes analysis (every attempt references an unknown op) must
    /// NOT be handed back to the plan-mode caller for `/run` — the diagnostics are surfaced (as the
    /// turn's text or its error) and nothing executable escapes. Before C-17, the final repair step
    /// "accepted with diagnostics" and `plan_turn` returned the AST blind.
    #[tokio::test]
    async fn plan_turn_rejects_a_diagnostics_plan_instead_of_handing_it_to_run() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let bad_ast = json!({
            "body": [{ "kind": "call", "op": "nope.op", "args": [] }]
        });
        // Every repair round re-emits the same unknown-op plan, exhausting the planner's budget.
        let responses: VecDeque<Vec<Chunk>> = (0..8).map(|_| emit_plan(bad_ast.clone())).collect();
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        let res = engine
            .plan_turn(
                &sid,
                "run the impossible op",
                &mut sink,
                &CancellationToken::new(),
            )
            .await;
        match res {
            Ok(Some(_)) => {
                panic!("a plan that never passed analysis must not be handed back for /run")
            }
            Ok(None) => assert!(
                sink.text.contains("unknown operation"),
                "the diagnostics are surfaced: {}",
                sink.text
            ),
            Err(e) => assert!(
                e.to_string().contains("unknown operation"),
                "the rejection carries the diagnostic text: {e}"
            ),
        }
    }

    /// A-18 named acceptance: single-shot behavior is unchanged when the model emits a full plan
    /// immediately (the common "simple/actionable request" case) — the orient call settles right
    /// away, so `compile_once` makes exactly ONE planner call, byte-compatible with pre-A-18
    /// `flux plan`.
    #[tokio::test]
    async fn compile_once_stays_single_shot_when_orient_settles_immediately() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({ "body": [{
            "kind": "bind", "name": "greeting",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
        }]});
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = CaptureProvider {
            responses: Mutex::new(VecDeque::from(vec![emit_plan(plan_ast)])),
            requests: requests.clone(),
        };
        let engine = engine_with_provider(Box::new(provider), store.clone());

        let mut sink = CollectSink::default();
        let out = engine
            .compile_once(&sid, "read a file then answer", &mut sink, None)
            .await
            .unwrap();
        match out {
            TurnOutput::Plan(compiled) => {
                assert!(
                    !compiled.gather,
                    "a simple request settles directly, untagged"
                );
                assert!(crate::render::render_pretty(&compiled.ast).contains("echo"));
            }
            TurnOutput::Chat(t) => panic!("expected a plan, got chat: {t}"),
        }
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "no added latency for a simple request: exactly one planner call"
        );
        // Never executed — `flux plan`'s contract holds for the settled plan too.
        assert!(
            engine
                .flow
                .resolve(&sid, &SymbolName("greeting".into()))
                .unwrap()
                .is_none(),
            "compile_once must never execute the plan it returns"
        );
    }

    /// A-18 named acceptance: a complex/context-hungry `flux plan` prompt auto-runs its read-only
    /// gather plan (A-13 already enforces it non-mutating at compile time), then the SETTLED
    /// execution plan is returned UNEXECUTED — `flux plan`'s "show me the plan before anything
    /// runs" contract holds for the final plan even though gather already ran automatically.
    #[tokio::test]
    async fn compile_once_runs_gather_then_shows_the_final_plan_unexecuted() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let gather_ast = json!({ "body": [{
            "kind": "bind", "name": "g",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "GATHER" }] }
        }]});
        let final_ast = json!({ "body": [{
            "kind": "bind", "name": "f",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "FINAL" }] }
        }]});
        let responses = VecDeque::from(vec![emit_gather_plan(gather_ast), emit_plan(final_ast)]);
        let engine = engine_with(responses, store.clone());

        let mut sink = CollectSink::default();
        let out = engine
            .compile_once(&sid, "investigate then fix", &mut sink, None)
            .await
            .unwrap();

        // The gather round ran for real: its read-only op dispatched and bound a session symbol.
        assert!(
            engine
                .flow
                .resolve(&sid, &SymbolName("g".into()))
                .unwrap()
                .is_some(),
            "the gather plan's op actually executed automatically"
        );
        // The settled result is the FINAL plan — and it is NOT the one that ran.
        match out {
            TurnOutput::Plan(compiled) => {
                assert!(
                    !compiled.gather,
                    "the returned plan is not itself tagged gather"
                );
                let rendered = crate::render::render_pretty(&compiled.ast);
                assert!(
                    rendered.contains("FINAL"),
                    "the returned plan is settled: {rendered}"
                );
            }
            TurnOutput::Chat(t) => panic!("expected the settled plan, got chat: {t}"),
        }
        assert!(
            engine
                .flow
                .resolve(&sid, &SymbolName("f".into()))
                .unwrap()
                .is_none(),
            "the final plan is shown, never auto-run"
        );
    }

    /// The same A-18 contract through the REPL `/plan` seam (`plan_turn`, not `compile_once`):
    /// gather auto-runs, and the AST handed back for `/run` is the settled plan, never the one that
    /// already ran.
    #[tokio::test]
    async fn plan_turn_runs_gather_then_returns_only_the_settled_plan() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let gather_ast = json!({ "body": [{
            "kind": "bind", "name": "g",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "GATHER" }] }
        }]});
        let final_ast = json!({ "body": [{
            "kind": "bind", "name": "f",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "FINAL" }] }
        }]});
        let responses = VecDeque::from(vec![emit_gather_plan(gather_ast), emit_plan(final_ast)]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();

        let ast = engine
            .plan_turn(
                &sid,
                "investigate then propose a fix",
                &mut sink,
                &CancellationToken::new(),
            )
            .await
            .unwrap()
            .expect("the settled plan is returned for /run");

        let rendered = crate::render::render_pretty(&ast);
        assert!(
            rendered.contains("FINAL"),
            "the returned AST is the settled plan: {rendered}"
        );
        assert!(
            engine
                .flow
                .resolve(&sid, &SymbolName("g".into()))
                .unwrap()
                .is_some(),
            "the gather round executed automatically"
        );
        assert!(
            engine
                .flow
                .resolve(&sid, &SymbolName("f".into()))
                .unwrap()
                .is_none(),
            "the settled plan is surfaced for /run, never auto-executed"
        );
        let msgs = store.conversation(&sid).unwrap();
        assert_eq!(msgs.len(), 2, "user + the proposed-plan assistant message");
        assert!(msgs[1].text().contains("Proposed plan"));
    }

    /// A-18 named acceptance: gather is bounded to `GATHER_ROUND_BUDGET` — the same repeat-3-style
    /// budget normal mode's `agent-loop.flux` uses (checked against drift by
    /// `agent_loop_flux_gather_budget_matches_the_shared_constant`), not a second invented one. A
    /// model that keeps tagging `gather: true` past the budget gets forced into `Phase::Execute`
    /// instead of having that (budget+1)-th plan run automatically.
    #[tokio::test]
    async fn compile_once_bounds_gather_to_the_shared_round_budget() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let gather_ast = |n: u32| {
            json!({ "body": [{
                "kind": "bind", "name": format!("g{n}"),
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": format!("GATHER {n}") }] }
            }]})
        };
        let final_ast = json!({ "body": [{
            "kind": "bind", "name": "f",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "FINAL" }] }
        }]});
        // Orient + GATHER_ROUND_BUDGET (3) gather rounds all keep tagging `gather: true`; the 4th
        // such plan (past the budget) must never execute. The clean, untagged final response is
        // accepted immediately once `Phase::Execute` is forced (it must NOT be gather-tagged, or
        // the compiler would reject it as repair feedback instead of settling).
        let responses = VecDeque::from(vec![
            emit_gather_plan(gather_ast(1)),
            emit_gather_plan(gather_ast(2)),
            emit_gather_plan(gather_ast(3)),
            emit_gather_plan(gather_ast(4)),
            emit_plan(final_ast),
        ]);
        let engine = engine_with(responses, store.clone());

        let mut sink = CollectSink::default();
        let out = engine
            .compile_once(&sid, "dig deep", &mut sink, None)
            .await
            .unwrap();

        for n in 1..=GATHER_ROUND_BUDGET {
            assert!(
                engine
                    .flow
                    .resolve(&sid, &SymbolName(format!("g{n}")))
                    .unwrap()
                    .is_some(),
                "gather round {n} (within budget) executed"
            );
        }
        assert!(
            engine
                .flow
                .resolve(&sid, &SymbolName(format!("g{}", GATHER_ROUND_BUDGET + 1)))
                .unwrap()
                .is_none(),
            "the gather-tagged plan past the budget is never executed"
        );
        match out {
            TurnOutput::Plan(compiled) => {
                assert!(!compiled.gather);
                assert!(crate::render::render_pretty(&compiled.ast).contains("FINAL"));
            }
            TurnOutput::Chat(t) => panic!("expected the settled plan, got chat: {t}"),
        }
        assert!(
            engine
                .flow
                .resolve(&sid, &SymbolName("f".into()))
                .unwrap()
                .is_none(),
            "the settled plan is shown, never run"
        );
    }

    /// SAFETY (A-13, threaded through the new A-18 seam): a `gather: true` plan naming a MUTATING
    /// op must be rejected as repair feedback and NEVER dispatched — plan mode's entire contract is
    /// "nothing but read-only gather ever runs automatically." `WriteTool` records every dispatch;
    /// this test fails loudly if that log is ever non-empty.
    #[tokio::test]
    async fn compile_once_rejects_a_mutating_gather_plan_via_the_same_a13_gate() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let mutating_gather_ast =
            json!({ "body": [{ "kind": "call", "op": "write_file", "args": [] }] });
        let clean_gather_ast = json!({ "body": [{
            "kind": "bind", "name": "g",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "ok" }] }
        }]});
        let final_ast = json!({ "body": [{
            "kind": "bind", "name": "f",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "FINAL" }] }
        }]});
        // First attempt tags a WRITE op as `gather: true` — must be rejected inside the SAME
        // planner call (repair feedback), never accepted and auto-run. The model's corrected
        // second attempt is effect-clean.
        let responses = VecDeque::from(vec![
            emit_gather_plan(mutating_gather_ast),
            emit_gather_plan(clean_gather_ast),
            emit_plan(final_ast),
        ]);
        let (engine, write_calls) = engine_with_write_tool(responses, store.clone());

        let mut sink = CollectSink::default();
        let out = engine
            .compile_once(&sid, "change something", &mut sink, None)
            .await
            .unwrap();

        assert!(
            write_calls.lock().unwrap().is_empty(),
            "a mutating op must NEVER dispatch during automatic gather execution"
        );
        assert!(
            engine
                .flow
                .resolve(&sid, &SymbolName("g".into()))
                .unwrap()
                .is_some(),
            "the corrected, effect-clean gather plan ran instead"
        );
        match out {
            TurnOutput::Plan(compiled) => {
                assert!(crate::render::render_pretty(&compiled.ast).contains("FINAL"));
            }
            TurnOutput::Chat(t) => panic!("expected the settled plan, got chat: {t}"),
        }
    }

    /// A-42 named acceptance: a plan-mode gather round's op dispatch/result stream LIVE to the
    /// caller's sink through `compile_once` (the one-shot `flux plan` seam) — before this story
    /// they ran through an internal `NullSink` (A-18's documented deviation), so `sink.tools`/
    /// `sink.results` stayed empty even though the gather round genuinely executed (proven by the
    /// `g` symbol landing in the session, exactly like `compile_once_runs_gather_then_shows_the_final_plan_unexecuted`).
    #[tokio::test]
    async fn compile_once_streams_gather_round_ops_to_the_live_sink() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let gather_ast = json!({ "body": [{
            "kind": "bind", "name": "g",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "GATHER" }] }
        }]});
        let final_ast = json!({ "body": [{
            "kind": "bind", "name": "f",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "FINAL" }] }
        }]});
        let responses = VecDeque::from(vec![emit_gather_plan(gather_ast), emit_plan(final_ast)]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();

        let _ = engine
            .compile_once(&sid, "investigate then fix", &mut sink, None)
            .await
            .unwrap();

        assert_eq!(
            sink.tools,
            vec!["echo".to_string()],
            "the gather round's op dispatch streamed to the caller's sink live: {:?}",
            sink.tools
        );
        assert_eq!(
            sink.results,
            vec!["echo".to_string()],
            "the gather round's result streamed too: {:?}",
            sink.results
        );
    }

    /// The same A-42 contract through the REPL `/plan` seam (`plan_turn`, not `compile_once`).
    #[tokio::test]
    async fn plan_turn_streams_gather_round_ops_to_the_live_sink() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let gather_ast = json!({ "body": [{
            "kind": "bind", "name": "g",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "GATHER" }] }
        }]});
        let final_ast = json!({ "body": [{
            "kind": "bind", "name": "f",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "FINAL" }] }
        }]});
        let responses = VecDeque::from(vec![emit_gather_plan(gather_ast), emit_plan(final_ast)]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();

        let _ = engine
            .plan_turn(
                &sid,
                "investigate then propose a fix",
                &mut sink,
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(
            sink.tools,
            vec!["echo".to_string()],
            "the gather round's op dispatch streamed to the caller's sink live: {:?}",
            sink.tools
        );
        assert_eq!(
            sink.results,
            vec!["echo".to_string()],
            "the gather round's result streamed too: {:?}",
            sink.results
        );
    }

    /// Reified await (post-cutover; see the design's turn-boundary section): a top-level `await` inside a
    /// plan no longer suspends the *turn*. `run_plan` reifies it as `Outcome` data — the inner run halts
    /// at the await (the prefix ran; the post-await steps did not) and the flux-lang loop carries on. The
    /// turn completes normally and the next round answers in prose. Cross-turn await/resume is
    /// intentionally out of scope for the self-hosted loop.
    #[tokio::test]
    async fn await_inside_a_plan_is_reified_not_a_turn_suspension() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();

        // Pre-await echo, then await, then a post-await echo that must NOT run (the await halts the run).
        let plan_ast = json!({
            "body": [
                { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "before await" }] },
                { "kind": "await", "binding": "name", "source": "user_input" },
                { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "after await" }] }
            ]
        });
        // Round 1 emits the awaiting plan; round 2 answers in prose (the loop continues past the await).
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("All set.")]);
        let engine = engine_with(responses, store.clone());

        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "greet me", &mut sink).await.unwrap();

        // Only the pre-await echo ran; the post-await echo did not (the inner run stopped at the await).
        assert_eq!(
            sink.tools,
            vec!["echo"],
            "only the pre-await echo ran: {:?}",
            sink.tools
        );
        // The turn completed normally with the prose answer — it did NOT suspend across the turn boundary.
        assert!(
            sink.text.contains("All set."),
            "the loop answered after the reified await: {:?}",
            sink.text
        );
        assert!(
            engine.flow.take_suspension(&sid).unwrap().is_none(),
            "no turn-level suspension is persisted"
        );
        let msgs = store.conversation(&sid).unwrap();
        assert_eq!(
            msgs.len(),
            2,
            "user + one assistant answer (valid session shape)"
        );
    }

    #[tokio::test]
    async fn plan_with_complete_renders_grounded_summary() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        // Round 1 emits a plan WITH a `complete` directive. The engine runs the plan, then makes a
        // grounded (no-tools) call to render the final message — which consumes the NEXT queued
        // response. That response is the summary; it is what gets shown and persisted (proving the
        // final text is produced post-execution, not pre-composed in the tool call).
        let responses = VecDeque::from(vec![
            emit_plan_complete(plan_ast, Some("summarize what the plan did")),
            prose("Ran echo and it returned hi."),
        ]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "echo hi", &mut sink).await.unwrap();

        assert_eq!(sink.tools, vec!["echo"], "the plan still executed");
        let msgs = store.conversation(&sid).unwrap();
        assert_eq!(
            msgs.len(),
            2,
            "user + the grounded summary — one completion"
        );
        assert!(
            msgs[1].text().contains("Ran echo and it returned hi."),
            "the persisted answer is the grounded summary, not the directive"
        );
        assert!(
            !msgs[1].text().contains("summarize what the plan did"),
            "the directive instructions must not leak into the final message"
        );
    }

    /// A-06 discriminating test: with a `complete` directive, the turn makes EXACTLY two model
    /// calls — the planner round and the toolless, catalog-less completion render. The older
    /// `plan_with_complete_renders_grounded_summary` cannot distinguish the render from an ordinary
    /// second planner round (the mock consumes the next queued response either way); this one
    /// inspects the actual requests.
    #[tokio::test]
    async fn completion_directive_skips_the_second_planner_round() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(CaptureProvider {
            responses: Mutex::new(VecDeque::from(vec![
                emit_plan_complete(plan_ast, Some("summarize what the plan did")),
                prose("Ran echo and it returned hi."),
            ])),
            requests: requests.clone(),
        });
        let engine =
            engine_with_provider(provider, store.clone()).with_reasoning(true, Some(Effort::High));
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "echo hi", &mut sink).await.unwrap();

        assert_eq!(sink.tools, vec!["echo"], "the plan executed");
        let reqs = requests.lock().unwrap();
        assert_eq!(
            reqs.len(),
            2,
            "exactly two model calls: planner + completion render"
        );
        assert!(
            reqs[0].tools.iter().any(|t| t.name == "emit_plan"),
            "call 1 is the planner"
        );
        assert!(
            reqs[1].tools.is_empty(),
            "call 2 offers NO tools — it cannot recurse into planning"
        );
        assert!(reqs.iter().all(|req| req.thinking));
        assert!(reqs.iter().all(|req| req.effort == Some(Effort::High)));
        let sys2 = format!(
            "{}{}",
            reqs[1].system.as_deref().unwrap_or_default(),
            reqs[1]
                .system_segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>()
        );
        assert!(
            !sys2.contains("Operation catalog"),
            "call 2 carries no op catalog"
        );
        assert!(
            sys2.contains("The plan has run"),
            "call 2 is the grounded render: {sys2}"
        );
        let msgs = store.conversation(&sid).unwrap();
        assert!(
            msgs[1].text().contains("Ran echo and it returned hi."),
            "the persisted answer is the rendered summary"
        );
    }

    /// A-06: a failed plan run must NOT consume the completion directive — the loop re-plans
    /// normally (call 2 is a full planner round with tools, not a render).
    #[tokio::test]
    async fn failed_plan_run_does_not_consume_completion() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let failing_plan = json!({
            "body": [{ "kind": "call", "op": "boom", "args": [] }]
        });
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(CaptureProvider {
            responses: Mutex::new(VecDeque::from(vec![
                emit_plan_complete(failing_plan, Some("summarize the fix")),
                prose("The operation failed, so nothing was changed."),
            ])),
            requests: requests.clone(),
        });
        let engine = engine_with_provider(provider, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "do it", &mut sink).await.unwrap();

        let reqs = requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert!(
            reqs[1].tools.iter().any(|t| t.name == "emit_plan"),
            "after a FAILED run, call 2 is a planner round (tools offered) — never the render"
        );
        let msgs = store.conversation(&sid).unwrap();
        assert!(msgs[1].text().contains("nothing was changed"));
    }

    /// A-06: the identical-plan skip (A-05 silent-success guard) must NOT render the completion —
    /// the skipped plan did not run, so its directive dies with it and the loop re-plans.
    #[tokio::test]
    async fn identical_plan_skip_does_not_render_completion() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(CaptureProvider {
            responses: Mutex::new(VecDeque::from(vec![
                // Round 1: the plan runs successfully (no directive).
                emit_plan(plan_ast.clone()),
                // Round 2: the model re-emits the IDENTICAL plan, now with a directive — the
                // silent-success guard skips the re-run, so the directive must not arm.
                emit_plan_complete(plan_ast, Some("summarize")),
                // Round 3: a normal planner round ends the turn in prose.
                prose("done after skip"),
            ])),
            requests: requests.clone(),
        });
        let engine = engine_with_provider(provider, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "echo hi", &mut sink).await.unwrap();

        assert_eq!(sink.tools, vec!["echo"], "the plan ran exactly once");
        let reqs = requests.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert!(
            reqs[2].tools.iter().any(|t| t.name == "emit_plan"),
            "after the skip, call 3 is a planner round — the unexecuted plan's directive must not render"
        );
        let msgs = store.conversation(&sid).unwrap();
        assert!(msgs[1].text().contains("done after skip"));
    }

    /// C-14: the in-memory evidence trail is flushed to the durable event store at turn end — the
    /// `tool_call` markers and `turn.iteration` rounds survive process exit.
    #[tokio::test]
    async fn turn_evidence_persists_to_event_store() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("done")]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "echo hi", &mut sink).await.unwrap();

        let obs = store.observations(&sid).unwrap();
        assert!(
            obs.iter().any(|o| o.kind == "tool_call"),
            "the dispatcher's tool_call markers persist: {:?}",
            obs.iter().map(|o| &o.kind).collect::<Vec<_>>()
        );
        assert!(
            obs.iter().any(|o| o.kind == "turn.iteration"),
            "the loop's turn.iteration observations persist"
        );
    }

    /// C-22: the durable evidence trail is redacted with the SAME redactor the executor uses. A
    /// seeded secret carried in a persisted `tool_call` observation (the raw per-token permission
    /// subject the dispatcher builds BEFORE the model-facing result is redacted) OR in the accepted
    /// plan's rendered `plan_text` lands `[redacted]` in the event store — not in the clear.
    #[tokio::test]
    async fn redacts_secrets_in_durable_observations_and_plan_text() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        const SECRET: &str = "supersecretbearervalue12345";

        // Round 1: a plan whose arg carries the secret (so the accepted plan's `plan_text` renders
        // it). Round 2: prose ends the turn.
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo",
                           "args": [{ "kind": "lit", "value": SECRET }] }
            }]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("done")]);
        let engine = engine_with(responses, store.clone());

        // Seed the executor's redactor exactly as C-13 seeds it from `resolve_secrets`, and stage a
        // raw `tool_call` observation carrying the secret in a permission subject.
        engine.executor.context().redactor.add_secret(SECRET);
        engine.executor.observe(flux_evidence::Observation::new(
            "tool_call",
            flux_evidence::Phase::Turn,
            json!({ "tool": "bash",
                    "subjects": [format!("curl -H 'Authorization: Bearer {SECRET}'")] }),
        ));

        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "run it", &mut sink).await.unwrap();

        // No persisted observation carries the secret in the clear.
        for o in store.observations(&sid).unwrap() {
            let data = serde_json::to_string(&o.data).unwrap();
            assert!(
                !data.contains(SECRET),
                "observation `{}` leaked the secret to events.db: {data}",
                o.kind
            );
        }
        // The accepted plan's durable rendered graph is recorded AND redacted.
        let plan_texts: Vec<String> = store
            .turns(&sid)
            .unwrap()
            .into_iter()
            .flat_map(|t| t.plan_attempts)
            .filter_map(|a| a.plan_text)
            .collect();
        assert!(
            plan_texts.iter().any(|t| t.contains("greeting")),
            "the accepted plan's plan_text was recorded: {plan_texts:?}"
        );
        assert!(
            plan_texts.iter().all(|t| !t.contains(SECRET)),
            "plan_text leaked the secret to events.db: {plan_texts:?}"
        );
    }

    /// C-24: `flush_tail` advances the watermark only past durable (Ok) writes. A store that fails
    /// one `record_observation` stops the flush at that index; the dropped observations are retried on
    /// the next flush once the store recovers, rather than being lost forever behind an
    /// unconditionally-advanced watermark.
    #[test]
    fn flush_tail_stops_at_first_failed_write_and_retries_next_flush() {
        let obs =
            |k: &str| flux_evidence::Observation::new(k, flux_evidence::Phase::Turn, json!({}));
        let all = [obs("a"), obs("b"), obs("c"), obs("d")];

        // A store that fails the write of observation "c" once, then recovers.
        let fail_on = std::cell::Cell::new(Some("c"));
        let recorded = std::cell::RefCell::new(Vec::<String>::new());
        let mut record = |o: &flux_evidence::Observation| -> Result<()> {
            if fail_on.get() == Some(o.kind.as_str()) {
                return Err(flux_core::Error::Other("transient store failure".into()));
            }
            recorded.borrow_mut().push(o.kind.clone());
            Ok(())
        };

        // First flush: a, b written; the flush STOPS at the failed c — the watermark advances by 2.
        let n1 = flush_tail(&all[0..], &mut record);
        assert_eq!(
            n1, 2,
            "watermark advanced only past the two successful writes"
        );
        assert_eq!(*recorded.borrow(), vec!["a", "b"]);

        // The store recovers; the next flush resumes AT the watermark and writes c, d — nothing lost.
        fail_on.set(None);
        let n2 = flush_tail(&all[n1..], &mut record);
        assert_eq!(n2, 2);
        assert_eq!(
            *recorded.borrow(),
            vec!["a", "b", "c", "d"],
            "the dropped observation was retried, not lost"
        );
    }

    /// A-22: an engine with a NON-ZERO compaction threshold, driven past it, actually compacts —
    /// the persisted conversation shrinks (older turns collapse into one summary message) instead of
    /// growing unbounded. This is the behaviour the served/SDK non-zero default now unlocks; with the
    /// old `compact_threshold_chars = 0` default `maybe_compact` returned early and the transcript
    /// grew until the provider context window blew.
    #[tokio::test]
    async fn agent_past_threshold_compacts_the_conversation() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        // The only model call `maybe_compact` makes is the summary render.
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(CaptureProvider {
            responses: Mutex::new(VecDeque::from(vec![prose("SUMMARY: the earlier turns.")])),
            requests: requests.clone(),
        });
        let mut engine = engine_with_provider(provider, store.clone())
            .with_reasoning(true, Some(Effort::Medium));
        engine.compact_threshold_chars = 50; // tiny, so a handful of messages exceed it

        // Seed a persistent-session conversation that exceeds the threshold.
        for i in 0..6 {
            store
                .record_message(
                    &sid,
                    &Message::user_text(format!("message number {i} with some length")),
                )
                .unwrap();
        }
        let before = store.conversation(&sid).unwrap().len();
        assert_eq!(before, 6);

        let mut sink = CollectSink::default();
        engine
            .maybe_compact(&sid, &mut sink, &CancellationToken::new())
            .await
            .unwrap();

        let after = store.conversation(&sid).unwrap().len();
        assert!(
            after < before,
            "compaction bounded the conversation: {before} -> {after}"
        );
        let reqs = requests.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert!(reqs[0].thinking);
        assert_eq!(reqs[0].effort, Some(Effort::Medium));
    }

    fn seed_provider_valid_history(store: &EventStore, sid: &str) {
        for n in 0..2 {
            store
                .record_message(sid, &Message::user_text(format!("old question {n}")))
                .unwrap();
            store
                .record_message(sid, &Message::assistant_text(format!("old answer {n}")))
                .unwrap();
        }
    }

    fn assert_provider_valid_history(messages: &[Message]) {
        for (index, message) in messages.iter().enumerate() {
            assert!(!message.content.is_empty(), "message {index} is empty");
            assert!(
                !(message.role == flux_core::Role::Assistant && message.text().trim().is_empty()),
                "assistant message {index} is empty"
            );
            if let Some(previous) = index.checked_sub(1) {
                assert_ne!(
                    messages[previous].role, message.role,
                    "messages {previous} and {index} have the same role"
                );
            }
        }
    }

    /// C-50: a provider failure while STARTING compaction occurs after the user's message and the
    /// turn-start event are durable. It must still close through the one assistant-finalization path
    /// rather than returning an open user tail / pending turn.
    #[tokio::test]
    async fn compaction_provider_start_error_finishes_a_provider_valid_turn() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        seed_provider_valid_history(&store, &sid);
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Box::new(CompactionFailProvider {
            calls: calls.clone(),
            fail_midstream: false,
            usage: Usage::default(),
        });
        let mut engine = engine_with_provider(provider, store.clone());
        engine.compact_threshold_chars = 1;

        let mut sink = CollectSink::default();
        engine
            .run_turn(&sid, "new question", &mut sink)
            .await
            .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "turn stopped at compaction"
        );
        assert_provider_valid_history(&store.conversation(&sid).unwrap());
        let turns = store.turns(&sid).unwrap();
        assert_eq!(turns.len(), 1);
        assert_ne!(turns[0].outcome, "pending", "the opened turn was closed");
        assert!(turns[0].answer.as_deref().is_some_and(|a| !a.is_empty()));
    }

    /// C-50: usage emitted before a compaction stream error is real spend. It is included in the
    /// turn total and emitted as exactly one per-call attribution record even though the summary did
    /// not complete, while the persisted conversation still ends in a valid assistant message.
    #[tokio::test]
    async fn compaction_stream_error_finishes_turn_and_counts_attempt_usage_once() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        seed_provider_valid_history(&store, &sid);
        let calls = Arc::new(AtomicUsize::new(0));
        let compact_usage = Usage {
            input_tokens: 321,
            output_tokens: 7,
            ..Default::default()
        };
        let provider = Box::new(CompactionFailProvider {
            calls: calls.clone(),
            fail_midstream: true,
            usage: compact_usage.clone(),
        });
        let mut engine = engine_with_provider(provider, store.clone());
        engine.compact_threshold_chars = 1;

        let mut sink = CollectSink::default();
        engine
            .run_turn(&sid, "new question", &mut sink)
            .await
            .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "turn stopped at compaction"
        );
        assert_provider_valid_history(&store.conversation(&sid).unwrap());
        let turns = store.turns(&sid).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].calls, 1, "one compaction request, counted once");
        assert_eq!(turns[0].call_usage, compact_usage);
        assert_eq!(turns[0].usage, Some(compact_usage));
        assert_ne!(turns[0].outcome, "pending", "the opened turn was closed");
    }

    /// C-50: `max_iterations` is an executable limit on the checked-in Flux-Lang loop. Different
    /// configured values change both provider consultations and actual plan dispatches; this does
    /// not merely assert that the final max-iteration sentence interpolates the configured number.
    #[tokio::test]
    async fn configured_max_iterations_changes_real_loop_bound() {
        async fn run(max_iterations: usize) -> (usize, usize, u32) {
            let store = Arc::new(EventStore::in_memory().unwrap());
            let sid = store.create_session("mock").unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let provider = Box::new(FreshPlanProvider {
                calls: calls.clone(),
            });
            let engine = engine_with_groups_and_iterations(
                provider,
                store.clone(),
                Vec::new(),
                max_iterations,
            );
            let mut sink = CollectSink::default();
            engine
                .run_turn(&sid, "keep planning", &mut sink)
                .await
                .unwrap();
            let iterations = store.turns(&sid).unwrap()[0].iterations;
            (calls.load(Ordering::SeqCst), sink.tools.len(), iterations)
        }

        assert_eq!(run(1).await, (2, 1, 1));
        assert_eq!(run(2).await, (3, 2, 2));
    }

    /// C-26: a resumed (reply-parked) continuation is a first-class turn. Before the fix
    /// `resume_suspended` finished with a hardcoded `turn_id = -1`, so no `TurnStarted`/`TurnEnded`
    /// was emitted (no `TurnSummary`) and its observations flushed unscoped. Now it wraps its work in
    /// a real `begin_turn`/`end_turn`, so the continuation produces a `TurnSummary` and its
    /// observations are retrievable scoped to that turn.
    #[tokio::test]
    async fn resumed_flow_produces_turn_telemetry() {
        use flux_lang::ast::{Node, NodeId};
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        // A resume bypasses the planner, so no provider round is queued.
        let engine = engine_with(VecDeque::new(), store.clone());

        // A parked flow: awaiting the user's reply, then it echoes and returns.
        let body = vec![
            Node::Await {
                binding: Some(SymbolName("reply".into())),
                source: "user_input".into(),
                as_type: None,
            },
            Node::Bind {
                name: SymbolName("b".into()),
                value: Box::new(Node::Call {
                    op: "echo".into(),
                    args: vec![Node::Lit {
                        value: json!("resumed work"),
                    }],
                }),
                ty: None,
                effect: None,
            },
            Node::Return {
                value: Box::new(Node::Var {
                    name: SymbolName("b".into()),
                }),
            },
        ];
        engine
            .flow
            .save_suspension(&sid, None, &body, NodeId(0), "user_input")
            .unwrap();

        // This turn's message is the awaited input → the engine resumes the parked flow.
        let mut sink = CollectSink::default();
        engine
            .run_turn(&sid, "here is my reply", &mut sink)
            .await
            .unwrap();
        assert_eq!(sink.tools, vec!["echo"], "the resumed suffix ran");

        // The resume produced a real TurnSummary (today: none — turn_id was -1).
        let turns = store.turns(&sid).unwrap();
        assert_eq!(
            turns.len(),
            1,
            "the resumed continuation is a first-class turn"
        );
        let t = &turns[0];
        assert!(t.turn_id >= 0, "the turn has a real id, not -1");
        assert!(
            t.ended_at_ms.is_some(),
            "the turn closed via begin_turn + end_turn"
        );

        // Its observations are retrievable scoped to that turn (the echo `tool_call` ran on resume).
        let turn_events = store.load_turn(&sid, t.turn_id).unwrap();
        let scoped_obs = turn_events
            .iter()
            .filter(|e| e.kind.kind_tag() == "observation")
            .count();
        assert!(
            scoped_obs > 0,
            "resume observations are turn-scoped, not unscoped under -1"
        );
    }

    /// D-131 (invariant 1): an authored two-`await` flow driven turn-by-turn via `start_flow_turn`
    /// surfaces the flow's OWN authored prompts and makes ZERO planner invocations — the
    /// deterministic skeleton never calls the model. Turn 1 starts the flow fresh; each later reply
    /// resumes the persisted suspension through the existing suspension-first branch.
    #[tokio::test]
    async fn start_flow_turn_drives_a_two_await_flow_with_zero_planner_calls() {
        use flux_lang::ast::{DraftAst, Node};
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = engine_with_provider(
            Box::new(CountingProvider {
                calls: calls.clone(),
            }),
            store.clone(),
        );

        // `echo` emits the authored prompt (its view becomes `outcome.result`); `await` parks for
        // the reply.
        let prompt = |t: &str| Node::Call {
            op: "echo".into(),
            args: vec![Node::Lit { value: json!(t) }],
        };
        let await_reply = |name: &str| Node::Await {
            binding: Some(SymbolName(name.into())),
            source: "user_input".into(),
            as_type: None,
        };
        let flow = DraftAst {
            body: vec![
                prompt("What is your name?"),
                await_reply("name"),
                prompt("Nice to meet you. Favorite color?"),
                await_reply("color"),
                prompt("All done — thanks!"),
            ],
            ..Default::default()
        };

        // Turn 1: fresh start → first authored prompt, parked on await #1.
        let mut s1 = CollectSink::default();
        engine.start_flow_turn(&sid, &flow, &mut s1).await.unwrap();
        assert!(
            s1.text.contains("What is your name?"),
            "turn 1 surfaces the first authored prompt: {:?}",
            s1.text
        );

        // Turn 2: the reply resumes → second authored prompt, parked on await #2.
        let mut s2 = CollectSink::default();
        engine.run_turn(&sid, "Timo", &mut s2).await.unwrap();
        assert!(
            s2.text.contains("Favorite color?"),
            "turn 2 surfaces the second authored prompt: {:?}",
            s2.text
        );

        // Turn 3: the second reply resumes → the flow completes and surfaces its result.
        let mut s3 = CollectSink::default();
        engine.run_turn(&sid, "blue", &mut s3).await.unwrap();
        assert!(
            s3.text.contains("All done"),
            "turn 3 surfaces the completion result: {:?}",
            s3.text
        );

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the deterministic flow-driven path must not invoke the planner"
        );
        let turns = store.turns(&sid).unwrap();
        assert_eq!(turns.len(), 3, "each driven turn is a first-class turn");
    }

    /// D-131 (invariant 6): a flow that parks on `await` WITHOUT emitting any text first falls back
    /// to the generic hint — the user always gets a usable prompt. (The non-empty authored-prompt
    /// case is covered by `start_flow_turn_drives_a_two_await_flow_with_zero_planner_calls`.)
    #[tokio::test]
    async fn flow_driven_suspension_falls_back_to_hint_on_empty_authored_prompt() {
        use flux_lang::ast::{DraftAst, Node};
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let engine = engine_with(VecDeque::new(), store.clone());
        let flow = DraftAst {
            body: vec![Node::Await {
                binding: Some(SymbolName("reply".into())),
                source: "user_input".into(),
                as_type: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        engine
            .start_flow_turn(&sid, &flow, &mut sink)
            .await
            .unwrap();
        assert!(
            sink.text.contains("awaiting your input"),
            "an await with no emitted prompt falls back to the hint: {:?}",
            sink.text
        );
    }

    /// D-131 (invariant 2): the safety envelope applies inside a flow-driven session exactly as on
    /// the planner path — a blanket-deny `RiskApprover` refuses a destructive op the authored flow
    /// dispatches, and the op never executes. `write_file` is left at `Ask` (empty allow rules) so
    /// it reaches the approver, mirroring `fork_tail_keeps_the_envelope`.
    #[tokio::test]
    async fn flow_driven_session_applies_the_risk_approver() {
        use flux_lang::ast::{DraftAst, Node};
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();

        let write_calls = Arc::new(Mutex::new(Vec::new()));
        let dir =
            std::env::temp_dir().join(format!("flux-flow-driven-deny-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        registry.register(Arc::new(WriteTool(write_calls.clone())));
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&[], &[]),
            Arc::new(flux_runtime::DenyApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        let flow_store = FlowStore::in_memory_with_events(store.clone()).unwrap();
        let engine = FlowEngine::assemble(
            Arc::from(Box::new(MockProvider {
                responses: Mutex::new(VecDeque::new()),
            }) as Box<dyn Provider>),
            executor,
            store.clone(),
            flow_store,
            "mock".into(),
            "test".into(),
            1024,
            5,
            Vec::new(),
            0,
            Vec::new(),
            dir,
        )
        .unwrap();

        // An authored flow whose first op is a destructive write.
        let flow = DraftAst {
            body: vec![Node::Bind {
                name: SymbolName("w".into()),
                value: Box::new(Node::Call {
                    op: "write_file".into(),
                    args: vec![Node::Lit {
                        value: json!("danger.txt"),
                    }],
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        engine
            .start_flow_turn(&sid, &flow, &mut sink)
            .await
            .unwrap();

        assert!(
            write_calls.lock().unwrap().is_empty(),
            "the denied destructive op must not execute in a flow-driven session (surfaced: {:?})",
            sink.text
        );
    }

    /// D-131 Phase B (invariant 5): an `ai_segment` is BOUNDED — with `max_rounds: 1` against a mock
    /// that never completes, the segment stops after exactly one delegated planner round and control
    /// returns to the deterministic flow, whose next node runs and its result surfaces.
    #[tokio::test]
    async fn ai_segment_is_bounded_by_max_rounds_and_returns_control() {
        use flux_lang::ast::{DraftAst, Node};
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        // FreshPlanProvider returns a fresh echo plan on every call and NEVER completes → only the
        // `max_rounds` cap can end the segment.
        let engine = engine_with_provider(
            Box::new(FreshPlanProvider {
                calls: calls.clone(),
            }),
            store.clone(),
        );

        let flow = DraftAst {
            body: vec![
                Node::Bind {
                    name: SymbolName("seg".into()),
                    value: Box::new(ai_segment_call("do the thing", &["echo"], 1, None)),
                    ty: None,
                    effect: None,
                },
                // A deterministic node AFTER the segment — its view proves control returned to the
                // flow's own skeleton once the bounded delegation ended.
                Node::Call {
                    op: "echo".into(),
                    args: vec![Node::Lit {
                        value: json!("after the segment"),
                    }],
                },
            ],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        engine
            .start_flow_turn(&sid, &flow, &mut sink)
            .await
            .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "max_rounds:1 caps the segment at exactly ONE planner round"
        );
        assert!(
            sink.text.contains("after the segment"),
            "control returned to the flow's next deterministic node: {:?}",
            sink.text
        );
    }

    /// D-131 Phase B: the optional `until` predicate ends an `ai_segment` early — as soon as the
    /// named symbol is bound to a non-empty value — well before `max_rounds`.
    #[tokio::test]
    async fn ai_segment_exits_early_when_until_symbol_is_bound() {
        use flux_lang::ast::{DraftAst, Node};
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        // Every round binds `$slots`; with until:"slots" the segment must exit after the FIRST round
        // despite max_rounds:5.
        let engine = engine_with_provider(
            Box::new(BindSlotsProvider {
                calls: calls.clone(),
            }),
            store.clone(),
        );

        let flow = DraftAst {
            body: vec![Node::Bind {
                name: SymbolName("seg".into()),
                value: Box::new(ai_segment_call(
                    "fill the slots",
                    &["echo"],
                    5,
                    Some("slots"),
                )),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        engine
            .start_flow_turn(&sid, &flow, &mut sink)
            .await
            .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the `until` predicate exits after ONE round, not max_rounds(5)"
        );
    }

    /// D-131 Phase B (invariant 4): an `ai_segment` cannot call an op outside its capability scope —
    /// a segment scoped to `[echo]` whose model keeps trying `write_file` never executes it.
    #[tokio::test]
    async fn ai_segment_cannot_call_an_op_outside_its_scope() {
        use flux_lang::ast::{DraftAst, Node};
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        // The model keeps trying to write_file; scoped to [echo], every attempt is refused (advertised
        // catalog + cap-scope floor), so the write never runs.
        let write_plan = json!({
            "body": [{ "kind": "bind", "name": "w",
                "value": { "kind": "call", "op": "write_file",
                           "args": [{ "kind": "lit", "value": "danger.txt" }] } }]
        });
        let responses = VecDeque::from(vec![
            emit_plan(write_plan.clone()),
            emit_plan(write_plan.clone()),
            emit_plan(write_plan),
        ]);
        let (engine, write_calls) = engine_with_write_tool(responses, store.clone());

        let flow = DraftAst {
            body: vec![Node::Bind {
                name: SymbolName("seg".into()),
                value: Box::new(ai_segment_call("write the file", &["echo"], 2, None)),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        // The segment may end by erroring (every round refused) — that is fine; the invariant is only
        // that the out-of-scope op NEVER executed.
        let _ = engine.start_flow_turn(&sid, &flow, &mut sink).await;

        assert!(
            write_calls.lock().unwrap().is_empty(),
            "an op outside the segment's [echo] scope must never execute (surfaced: {:?})",
            sink.text
        );
    }

    /// C-14: every planning attempt lands durably — the accepted plan with its AST fingerprint and
    /// the human-readable rendered graph, and the closing chat round.
    #[tokio::test]
    async fn plan_attempts_recorded_with_fingerprint_and_text() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("done")]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "echo hi", &mut sink).await.unwrap();

        let turns = store.turns(&sid).unwrap();
        assert_eq!(turns.len(), 1);
        let attempts = &turns[0].plan_attempts;
        assert!(attempts.len() >= 2, "accepted + chat: {attempts:?}");
        assert_eq!(attempts[0].outcome, "accepted");
        assert!(
            attempts[0].fingerprint.is_some(),
            "the accepted plan carries its AST fingerprint"
        );
        assert!(
            attempts[0]
                .plan_text
                .as_deref()
                .is_some_and(|t| t.contains("echo")),
            "the accepted plan carries its readable graph: {:?}",
            attempts[0].plan_text
        );
        assert_eq!(attempts.last().unwrap().outcome, "chat");
    }

    /// A-14: `PlanAttempt` is stamped with the phase that produced it, so the audit trail can tell
    /// the orient call, the gather round(s), and the execute-phase rounds apart (C-15 wants to
    /// report gather/revise rounds per turn from exactly this field).
    #[tokio::test]
    async fn plan_attempts_are_phase_stamped_across_the_multipass_loop() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let gather_ast = json!({ "body": [{
            "kind": "bind", "name": "g",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "GATHER" }] }
        }]});
        let final_ast = json!({ "body": [{
            "kind": "bind", "name": "f",
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "FINAL" }] }
        }]});
        let responses = VecDeque::from(vec![
            emit_gather_plan(gather_ast),
            emit_plan(final_ast),
            prose("done"),
        ]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine
            .run_turn(&sid, "investigate then fix", &mut sink)
            .await
            .unwrap();

        let turns = store.turns(&sid).unwrap();
        assert_eq!(turns.len(), 1);
        let attempts = &turns[0].plan_attempts;
        let phases: Vec<Option<String>> = attempts.iter().map(|a| a.phase.clone()).collect();
        assert_eq!(
            phases,
            vec![
                Some("orient".to_string()),
                Some("gather".to_string()),
                Some("execute".to_string()),
            ],
            "{attempts:?}"
        );
    }

    /// C-14: the `groups.active` observation carries the workspace signals that justified the
    /// surfaced groups — durable signal→group provenance.
    #[tokio::test]
    async fn groups_active_observation_carries_signals() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let groups = vec![flux_evidence::ToolGroup {
            name: "git".into(),
            description: String::new(),
            tools: vec!["echo".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: flux_evidence::KIND_SIGNAL.to_string(),
                signal: Some("git_repo".into()),
            }],
        }];
        let responses = VecDeque::from(vec![prose("hi")]);
        let engine = engine_with_groups(
            Box::new(MockProvider {
                responses: Mutex::new(responses),
            }),
            store.clone(),
            groups,
        );
        // Make the workspace a git repo so detect_signals emits `git_repo`.
        std::fs::create_dir_all(engine.cwd.join(".git")).unwrap();
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "hello", &mut sink).await.unwrap();

        let obs = store.observations(&sid).unwrap();
        let groups_active = obs
            .iter()
            .find(|o| o.kind == "groups.active")
            .expect("groups.active recorded");
        let signals: Vec<&str> = groups_active.data["signals"]
            .as_array()
            .expect("signals array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            signals.contains(&"git_repo"),
            "the observation names the justifying signal: {signals:?}"
        );
        let active: Vec<&str> = groups_active.data["groups"]
            .as_array()
            .expect("groups array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(active.contains(&"git"), "the surfaced group: {active:?}");
    }

    /// The loop host's conversation cache is bounded: `set_turn` evicts every other session's entry,
    /// so a shared engine serving many sessions (the a2a server mints one per request) holds at most
    /// the active session's conversation instead of leaking one per session ever seen. Same-session
    /// turns keep their cache (the whole point of the incremental fold).
    #[test]
    fn conversation_cache_evicts_other_sessions_on_set_turn() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let a = store.create_session("mock").unwrap();
        let b = store.create_session("mock").unwrap();
        store.record_message(&a, &Message::user_text("ha")).unwrap();
        store.record_message(&b, &Message::user_text("hb")).unwrap();
        let engine = engine_with(VecDeque::new(), store.clone());
        let host = &engine.loop_host;
        assert_eq!(host.load_persisted_conversation(&a).len(), 1);
        assert_eq!(host.load_persisted_conversation(&b).len(), 1);
        assert_eq!(host.cached_conversation_sessions().len(), 2);

        let sink: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(NullSink));
        host.set_turn(b.clone(), None, sink, None, None);
        assert_eq!(
            host.cached_conversation_sessions(),
            vec![b.clone()],
            "only the active session's conversation stays cached"
        );
        // The surviving entry still serves the right history (and keeps folding incrementally).
        assert_eq!(host.load_persisted_conversation(&b).len(), 1);
    }

    /// Monotonic surfacing: once a group surfaces it stays advertised for the session, even after its
    /// marker signal disappears — so segment A's op catalog is a stable provider-cache prefix (A-03)
    /// instead of flapping with `resolve_active_groups`'s stateless, per-turn result.
    #[test]
    fn surfacing_is_monotonic_across_a_marker_flip() {
        // Use a synthetic ambient signal rather than a filesystem marker. `detect_signals` walks
        // ancestors, so a host-owned marker such as `/tmp/.git` must not decide this test's state.
        let dir = std::env::temp_dir();
        let marker = "test_sticky_marker".to_string();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        let groups = vec![flux_evidence::ToolGroup {
            name: "sticky-test".into(),
            description: String::new(),
            tools: vec!["echo".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: flux_evidence::KIND_SIGNAL.to_string(),
                signal: Some(marker.clone()),
            }],
        }];
        let sticky = std::sync::Mutex::new(std::collections::HashSet::new());

        // Turn A — no marker signal: the group is inactive, so `echo` is gated (not advertised).
        let (a, _) = surfaced_op_names(&registry, &groups, &dir, &sticky, &[], "");
        assert!(
            !a.contains("echo"),
            "echo gated before the marker appears: {a:?}"
        );

        // Turn B — marker signal present: the group surfaces, `echo` is advertised.
        let (b, _) = surfaced_op_names(
            &registry,
            &groups,
            &dir,
            &sticky,
            std::slice::from_ref(&marker),
            "",
        );
        assert!(
            b.contains("echo"),
            "echo advertised once the marker is present: {b:?}"
        );

        // Turn C — marker signal absent: stateless resolution would drop `echo`, but the sticky union
        // keeps the group surfaced, so the advertised catalog never shrinks.
        let (c, _) = surfaced_op_names(&registry, &groups, &dir, &sticky, &[], "");
        assert!(
            c.contains("echo"),
            "sticky surfacing keeps echo after the marker disappears: {c:?}"
        );
        assert!(
            b.is_subset(&c),
            "the advertised catalog never shrinks: {b:?} !⊆ {c:?}"
        );
    }

    /// D-115: session-ambient signals (host-known facts the per-turn workspace walk can't see —
    /// e.g. the CLI's "the endpoints store is non-empty") surface groups exactly like
    /// workspace-probed signals. A group gated on `endpoint` surfaces when the host injects the
    /// ambient signal and stays gated when it doesn't — no marker file, no kubeconfig.
    #[test]
    fn ambient_signals_surface_groups_without_workspace_evidence() {
        let dir = std::env::temp_dir().join(format!("flux-ambient-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        let groups = vec![flux_evidence::ToolGroup {
            name: "endpoint".into(),
            description: String::new(),
            tools: vec!["echo".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: flux_evidence::KIND_SIGNAL.to_string(),
                signal: Some("endpoint".into()),
            }],
        }];

        // No ambient signal (fresh sticky set): the group stays gated.
        let sticky = std::sync::Mutex::new(std::collections::HashSet::new());
        let (gated, _) = surfaced_op_names(&registry, &groups, &dir, &sticky, &[], "");
        assert!(
            !gated.contains("echo"),
            "gated without the ambient signal: {gated:?}"
        );

        // The ambient `endpoint` signal surfaces it.
        let sticky = std::sync::Mutex::new(std::collections::HashSet::new());
        let ambient = ["endpoint".to_string()];
        let (surfaced, _) = surfaced_op_names(&registry, &groups, &dir, &sticky, &ambient, "");
        assert!(
            surfaced.contains("echo"),
            "ambient signal surfaces the group: {surfaced:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn turn_intent_surfaces_and_sticks_an_integration_group() {
        let dir = std::env::temp_dir();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        let groups = vec![flux_evidence::ToolGroup {
            name: "plugin.slack".into(),
            description: String::new(),
            tools: vec!["echo".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: flux_evidence::KIND_TURN_INTENT.into(),
                signal: Some("slack".into()),
            }],
        }];
        let sticky = std::sync::Mutex::new(std::collections::HashSet::new());

        let (unrelated, _) =
            surfaced_op_names(&registry, &groups, &dir, &sticky, &[], "summarize docs");
        assert!(!unrelated.contains("echo"));

        let (named, surfaced) = surfaced_op_names(
            &registry,
            &groups,
            &dir,
            &sticky,
            &[],
            "post the summary in Slack",
        );
        assert!(named.contains("echo"));
        assert!(surfaced.unwrap().signals.contains(&"slack".to_string()));

        let (sticky_next, _) =
            surfaced_op_names(&registry, &groups, &dir, &sticky, &[], "continue");
        assert!(sticky_next.contains("echo"));
    }

    /// A-10: once the turn's accumulated planner usage crosses the installed token budget, the
    /// next `plan` ends the turn honestly (no further model call) — the runaway bound is tokens,
    /// not just the 25-iteration cap.
    #[tokio::test]
    async fn turn_ends_honestly_when_token_budget_exhausted() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        // The planner call reports 5000 tokens of usage — over the 100-token budget, so the second
        // `plan` must not reach the provider at all.
        let usage = Usage {
            input_tokens: 4000,
            output_tokens: 1000,
            ..Default::default()
        };
        let planner_call_with_usage = vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: "p1".into(),
                name: "emit_plan".into(),
                input: json!({ "ast": plan_ast }),
            }),
            Chunk::Usage(usage),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ];
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(CaptureProvider {
            responses: Mutex::new(VecDeque::from(vec![
                planner_call_with_usage,
                prose("this response must never be consumed"),
            ])),
            requests: requests.clone(),
        });
        let engine = engine_with_provider(provider, store.clone());
        engine.loop_host.set_token_budget(Some(100));
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "echo hi", &mut sink).await.unwrap();

        assert_eq!(sink.tools, vec!["echo"], "the accepted plan still ran");
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "the budget stop must not pay another model call"
        );
        let msgs = store.conversation(&sid).unwrap();
        let answer = msgs[1].text();
        assert!(
            answer.contains("token budget") && answer.contains("100"),
            "the turn ends with the honest budget message: {answer}"
        );
        // The stop is auditable: a turn.budget_exceeded observation landed on the evidence trail.
        assert!(
            store
                .observations(&sid)
                .unwrap()
                .iter()
                .any(|o| o.kind == "turn.budget_exceeded"),
            "budget stop recorded as evidence"
        );
    }

    #[tokio::test]
    async fn text_only_turn_answers_in_prose() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let responses = VecDeque::from(vec![prose("Here's the explanation.")]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "explain", &mut sink).await.unwrap();

        assert!(sink.tools.is_empty(), "a chat turn runs no ops");
        assert!(sink.text.contains("explanation"));
        let msgs = store.conversation(&sid).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs.iter().all(|m| !m.content.is_empty()));
    }

    #[tokio::test]
    async fn reviewed_plan_runs_through_shared_engine_path() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let engine = engine_with(VecDeque::new(), store);
        let ast: DraftAst = serde_json::from_value(json!({
            "body": [{
                "kind": "bind",
                "name": "answer",
                "value": {
                    "kind": "call",
                    "op": "echo",
                    "args": [{"kind": "lit", "value": {"text": "reviewed"}}]
                }
            }]
        }))
        .unwrap();
        let mut sink = CollectSink::default();
        let outcome = engine
            .run_reviewed_plan_cancellable(&sid, &ast, &mut sink, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(outcome, ReviewedPlanOutcome::Completed);
        assert_eq!(sink.tools, vec!["echo"]);
        assert_eq!(sink.turn_ends.len(), 1);
    }

    #[tokio::test]
    async fn reviewed_plan_cancellation_still_ends_the_surface_turn() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let engine = engine_with(VecDeque::new(), store);
        let ast: DraftAst = serde_json::from_value(json!({
            "body": [{
                "kind": "call",
                "op": "echo",
                "args": [{"kind": "lit", "value": {"text": "must not run"}}]
            }]
        }))
        .unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut sink = CollectSink::default();
        let outcome = engine
            .run_reviewed_plan_cancellable(&sid, &ast, &mut sink, &cancel)
            .await
            .unwrap();
        assert_eq!(outcome, ReviewedPlanOutcome::Cancelled);
        assert!(sink.tools.is_empty());
        assert_eq!(sink.turn_ends, [None]);
    }

    #[tokio::test]
    async fn provider_error_is_surfaced_not_silent() {
        // A provider/API failure during planning (e.g. credit exhausted) must reach the user — the
        // turn used to store the answer but never emit it, ending the turn in silence.
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("fail").unwrap();
        let provider = Box::new(FailProvider {
            err: Box::new(|| {
                Error::Api {
                status: 400,
                message: r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#.into(),
            }
            }),
        });
        let engine = engine_with_provider(provider, store.clone());
        let mut sink = CollectSink::default();
        engine
            .run_turn(&sid, "do something", &mut sink)
            .await
            .unwrap();

        // The failure was shown to the user, with the provider's message unwrapped from its JSON body.
        assert!(
            sink.text.contains("credit balance is too low"),
            "the credit error must be surfaced to the user, got: {:?}",
            sink.text
        );
        assert!(sink.text.contains("HTTP 400"), "the status is shown too");

        // The session stays a valid user → assistant alternation (no bricked session on the next turn).
        let msgs = store.conversation(&sid).unwrap();
        assert_eq!(msgs.len(), 2, "user + one assistant message");
        assert_eq!(msgs[0].role, flux_core::Role::User);
        assert_eq!(msgs[1].role, flux_core::Role::Assistant);
        assert!(msgs[1].text().contains("credit balance is too low"));
    }

    /// C-06 attribution (live path): the engine appends one `EventKind::CallUsage` per planner call,
    /// stamped with the model that was ACTIVE for that call — so a mid-turn model switch (the REPL
    /// `/model` command, which swaps the loop host's planner + emits `ModelChanged` before the NEXT
    /// turn) attributes each turn's tokens to the model that actually produced them, not to whichever
    /// model is current by the time the turn ends.
    #[tokio::test]
    async fn usage_attributed_per_model_after_switch() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        // `engine_with`/`engine_with_provider` always construct the engine with model "mock"
        // (FlowEngine::assemble's hardcoded test model) — this IS the pre-switch model-a.
        let sid = store.create_session("mock").unwrap();

        // Turn 1 on the engine's initial model ("mock"): a single prose call carrying usage.
        let responses = VecDeque::from(vec![prose_with_usage(
            "answer one",
            Usage {
                input_tokens: 100,
                output_tokens: 10,
                ..Default::default()
            },
        )]);
        let mut engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "first", &mut sink).await.unwrap();

        // Switch models mid-session (mirrors the REPL `/model` command: swap the loop host's planner
        // AND record `ModelChanged`) — a NEW provider whose usage must be attributed to `model-b`.
        let provider_b: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::from(vec![prose_with_usage(
                "answer two",
                Usage {
                    input_tokens: 50,
                    output_tokens: 5,
                    ..Default::default()
                },
            )])),
        });
        engine
            .switch_model_for_session(&sid, provider_b, "model-b".to_string())
            .unwrap();

        engine.run_turn(&sid, "second", &mut sink).await.unwrap();

        let events = store.load_stream(&sid, None).unwrap();
        let calls: Vec<(String, Usage)> = events
            .iter()
            .filter_map(|e| match &e.kind {
                flux_events::EventKind::CallUsage { model, usage } => {
                    Some((model.clone(), usage.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            calls.len(),
            2,
            "one CallUsage per turn's planner call: {calls:?}"
        );
        assert_eq!(calls[0].0, "mock");
        assert_eq!(calls[0].1.input_tokens, 100);
        assert_eq!(calls[1].0, "model-b");
        assert_eq!(calls[1].1.input_tokens, 50);

        // Each turn's own TurnEnded.usage total stays correct too (the fields the story keeps as
        // back-compat) — attribution didn't come at the cost of the existing turn-total field.
        let turns = store.turns(&sid).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].usage.as_ref().unwrap().input_tokens, 100);
        assert_eq!(turns[1].usage.as_ref().unwrap().input_tokens, 50);
    }

    #[test]
    fn planner_error_unwraps_api_json_and_passes_through_others() {
        // An Anthropic-style error body collapses to its `error.message`.
        let api = Error::Api {
            status: 429,
            message:
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"rate limited"}}"#
                    .into(),
        };
        let rendered = planner_error(&api);
        assert!(rendered.contains("HTTP 429"));
        assert!(rendered.contains("rate limited"));
        assert!(
            !rendered.contains('{'),
            "the raw JSON body is not shown: {rendered}"
        );

        // A non-JSON body falls back to the raw message.
        let plain = Error::Api {
            status: 500,
            message: "upstream exploded".into(),
        };
        assert!(planner_error(&plain).contains("upstream exploded"));

        // Non-API errors use their own Display.
        assert_eq!(planner_error(&Error::Other("boom".into())), "boom");
    }

    /// C-43 (failing-first acceptance): a turn's dispatched ops land as durable, REDACTED
    /// `OpRecorded` cassette cells on the session stream — and the loop-machinery ops
    /// (`plan`/`run_plan`) are never cassetted.
    #[tokio::test]
    async fn cassette_records_redacted_cells_for_dispatched_ops() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        const SECRET: &str = "cassettesecretvalue987";
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo",
                           "args": [{ "kind": "lit", "value": SECRET }] }
            }]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("done")]);
        let engine = engine_with(responses, store.clone());
        engine.executor.context().redactor.add_secret(SECRET);
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "run it", &mut sink).await.unwrap();

        let trace = store.run_trace(&sid).unwrap();
        let cells: Vec<(String, String, bool)> = trace
            .iter()
            .filter_map(|ev| match ev {
                flux_lang::ast::RunEvent::OpRecorded {
                    op,
                    content,
                    redacted,
                    ..
                } => Some((op.clone(), content.clone(), *redacted)),
                _ => None,
            })
            .collect();
        assert_eq!(
            cells.iter().filter(|(op, ..)| op == "echo").count(),
            1,
            "the one dispatched op has exactly one cell: {cells:?}"
        );
        assert!(
            cells
                .iter()
                .all(|(op, ..)| op != "plan" && op != "run_plan"),
            "loop machinery is never cassetted: {cells:?}"
        );
        // NO field of any cell — content, view, or even the input HASH — carries the secret.
        // (Empirically the engine path is redaction-stable end-to-end: the envelope scrubs the
        // `plan` op's own output, so a secret literal in a model-emitted plan is destroyed before
        // the inner plan executes — the echo tool received and output `[redacted]`.)
        for ev in &trace {
            if matches!(ev, flux_lang::ast::RunEvent::OpRecorded { .. }) {
                let raw = serde_json::to_string(ev).unwrap();
                assert!(!raw.contains(SECRET), "a cell leaked the secret: {raw}");
            }
        }
        let redacted_input_hash = flux_lang::runtime::sha256_hex(r#"{"text":"[redacted]"}"#);
        assert!(
            trace.iter().any(|ev| matches!(ev,
                flux_lang::ast::RunEvent::OpRecorded { op, input_hash, .. }
                    if op == "echo" && *input_hash == redacted_input_hash)),
            "the echo cell's input hash covers the REDACTED input — no raw-secret hash oracle"
        );
    }

    /// C-43/A-45 (hermetic-serve mechanism): rebuild the tape from a recorded turn's trace and
    /// re-execute the same plan under a `Replay` scope in a FRESH store — the recorded (redacted)
    /// outputs are served back and the side-effecting tool is NOT re-fired.
    #[tokio::test]
    async fn cassette_replay_serves_recorded_cells_without_refiring_side_effects() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "res",
                "value": { "kind": "call", "op": "write_file",
                           "args": [{ "kind": "lit", "value": "notes.txt" }] }
            }]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast.clone()), prose("done")]);
        let (engine, write_calls) = engine_with_write_tool(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "write it", &mut sink).await.unwrap();
        assert_eq!(
            write_calls.lock().unwrap().len(),
            1,
            "the live run wrote once"
        );

        // Hermetic replay: fresh store + scratch event log, tape from the recorded trace.
        let tape = crate::cassette::ReplayTape::from_trace(&store.run_trace(&sid).unwrap());
        assert_eq!(tape.len(), 1, "one recorded cell (write_file)");
        let scratch = Arc::new(EventStore::in_memory().unwrap());
        let rsid = scratch.create_session("replay").unwrap();
        let rstore = FlowStore::in_memory_with_events(scratch).unwrap();
        rstore.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Replay(tape))));

        let ast: crate::ast::DraftAst = serde_json::from_value(plan_ast).unwrap();
        let mut rsink = CollectSink::default();
        let out = crate::runtime::execute_flow_resumable_with_composites(
            &rstore,
            &engine.executor,
            &rsid,
            &ast,
            &[],
            None,
            None,
            &mut rsink,
        )
        .await
        .unwrap();
        assert!(
            out.failure.is_none(),
            "replay completes cleanly: {:?}",
            out.failure
        );
        // The killer assertion: the side effect did NOT re-fire.
        assert_eq!(
            write_calls.lock().unwrap().len(),
            1,
            "replay served the cell from tape — the write tool never ran again"
        );
        // And the replayed binding equals the recorded content.
        let scope = rstore.cassette().unwrap();
        if let crate::cassette::CassetteScope::Replay(t) = scope.as_ref() {
            assert_eq!(t.remaining(), 0, "every cell was consumed");
            assert!(t.diverged().is_none(), "no divergence: {:?}", t.diverged());
        }
    }

    /// A-45 (failing-first acceptance): the full driver — `replay_session` re-executes a recorded
    /// agent turn from `plan_source` + cassette alone: no model (the executor has no provider on
    /// this path), no side-effect re-fire, every cell consumed, zero divergence.
    #[tokio::test]
    async fn replay_session_reproduces_a_recorded_turn_hermetically() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [
                { "kind": "bind", "name": "res",
                  "value": { "kind": "call", "op": "write_file",
                             "args": [{ "kind": "lit", "value": "notes.txt" }] } },
                { "kind": "bind", "name": "back",
                  "value": { "kind": "call", "op": "echo",
                             "args": [{ "kind": "lit", "value": "after-write" }] } }
            ]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("done")]);
        let (engine, write_calls) = engine_with_write_tool(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "write it", &mut sink).await.unwrap();
        assert_eq!(write_calls.lock().unwrap().len(), 1);

        // The driver: recorded events + an executor for the CATALOG only. The mock provider's
        // canned responses are exhausted — any planner call would fail loudly.
        let mut rsink = CollectSink::default();
        let report =
            crate::replay::replay_session(&store, &engine.executor, &sid, None, &mut rsink)
                .await
                .unwrap();
        assert_eq!(report.plans.len(), 1, "one recorded execution: {report:?}");
        assert!(report.diverged.is_none(), "faithful replay: {report:?}");
        assert_eq!(report.cells_consumed, report.cells_total);
        assert_eq!(report.missing_sources, 0);
        assert!(report.plans[0].halted.is_none());
        // Hermetic: the side-effecting tool did NOT run again.
        assert_eq!(write_calls.lock().unwrap().len(), 1);
        // The replayed transcript surfaced the same ops through the sink.
        assert_eq!(rsink.tools, vec!["write_file", "echo"]);
    }

    /// A-46 (failing-first acceptance): fork reproduces the prefix from tape (no side-effect
    /// re-fire), then diverges by injection — the injected symbol replaces the fork statement's
    /// recorded binding, and the tail runs LIVE (its dispatches record fresh cells on the fork
    /// session, which only a real dispatch under a `Record` scope can produce).
    #[tokio::test]
    async fn fork_replays_prefix_then_diverges_by_injection() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [
                { "kind": "bind", "name": "res",
                  "value": { "kind": "call", "op": "write_file",
                             "args": [{ "kind": "lit", "value": "notes.txt" }] } },
                { "kind": "bind", "name": "mid",
                  "value": { "kind": "call", "op": "echo",
                             "args": [{ "kind": "lit", "value": "original" }] } },
                { "kind": "bind", "name": "fin",
                  "value": { "kind": "call", "op": "echo",
                             "args": [{ "kind": "lit", "value": "tail" }] } }
            ]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("done")]);
        let (engine, write_calls) = engine_with_write_tool(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "go", &mut sink).await.unwrap();
        assert_eq!(write_calls.lock().unwrap().len(), 1);

        // Fork at statement 1 (`mid`): a new session, correlated to the source.
        let fork_sid = store
            .create_session_with_context(
                "mock",
                &flux_events::EventContext {
                    correlation_id: Some(sid.clone()),
                    agent_id: Some(format!("fork:{sid}@1")),
                    ..Default::default()
                },
            )
            .unwrap();
        let fstore = FlowStore::in_memory_with_events(store.clone()).unwrap();
        let mut fsink = CollectSink::default();
        let prefix = crate::fork::replay_prefix(
            &store,
            &fstore,
            &engine.executor,
            &sid,
            &fork_sid,
            1,
            &mut fsink,
        )
        .await
        .unwrap();
        // The prefix (write_file) was served from tape — no re-fire.
        assert_eq!(write_calls.lock().unwrap().len(), 1);

        let out = crate::fork::diverge_inject(
            &fstore,
            &engine.executor,
            &fork_sid,
            &prefix,
            &serde_json::json!("INJECTED"),
            &mut fsink,
        )
        .await
        .unwrap();
        assert!(
            out.failure.is_none(),
            "live tail completes: {:?}",
            out.failure
        );

        // The injected value is bound in the fork session…
        let view = fstore.view(&fork_sid).unwrap();
        let mid = view.symbols.iter().find(|s| s.name.0 == "mid").unwrap();
        assert!(mid.summary.contains("INJECTED"), "injected bind: {mid:?}");
        // …and the tail's echo dispatched LIVE: a fresh cell landed on the FORK session's stream
        // (Replay mode never records — only the post-boundary Record scope can have).
        let fork_cells: Vec<String> = store
            .run_trace(&fork_sid)
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                flux_lang::ast::RunEvent::OpRecorded { op, content, .. } if op == "echo" => {
                    Some(content.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            fork_cells,
            vec!["tail".to_string()],
            "live tail recorded its own cell"
        );
    }

    /// A-46 (failing-first acceptance): the fork tail runs through the REAL approval envelope —
    /// a denying approver refuses the tail op (the cassette-vs-live boundary grants nothing).
    #[tokio::test]
    async fn fork_tail_keeps_the_envelope() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [
                { "kind": "bind", "name": "res",
                  "value": { "kind": "call", "op": "write_file",
                             "args": [{ "kind": "lit", "value": "notes.txt" }] } },
                { "kind": "bind", "name": "fin",
                  "value": { "kind": "call", "op": "write_file",
                             "args": [{ "kind": "lit", "value": "danger.txt" }] } }
            ]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("done")]);
        let (engine, write_calls) = engine_with_write_tool(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "go", &mut sink).await.unwrap();
        let live_writes = write_calls.lock().unwrap().len();

        // A tail executor whose approver DENIES everything and whose permission rules grant
        // nothing — the real envelope, hostile mode.
        struct DenyAll;
        #[async_trait]
        impl flux_runtime::Approver for DenyAll {
            async fn request(
                &self,
                _t: &str,
                _s: &[String],
                _i: &flux_spec::IntentSet,
            ) -> flux_runtime::ApprovalChoice {
                flux_runtime::ApprovalChoice::Deny
            }
        }
        let dir = std::env::temp_dir().join(format!("flux-fork-deny-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        registry.register(Arc::new(WriteTool(write_calls.clone())));
        let deny_exec = Executor::new(
            registry,
            PermissionManager::from_rules(&[], &[]),
            Arc::new(DenyAll),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );

        let fork_sid = store.create_session("mock").unwrap();
        let fstore = FlowStore::in_memory_with_events(store.clone()).unwrap();
        let mut fsink = CollectSink::default();
        // Prefix replay needs no approvals (tape-served) even under the hostile executor.
        let prefix =
            crate::fork::replay_prefix(&store, &fstore, &deny_exec, &sid, &fork_sid, 1, &mut fsink)
                .await
                .unwrap();
        assert_eq!(
            write_calls.lock().unwrap().len(),
            live_writes,
            "prefix re-fired nothing"
        );

        // The tail's write_file must be REFUSED by the envelope, and must not run.
        let out = crate::fork::diverge_edit(
            &fstore,
            &deny_exec,
            &fork_sid,
            &prefix,
            &prefix.plan.clone(),
            &mut fsink,
        )
        .await
        .unwrap();
        let halt = out.failure.expect("the denied tail halts the plan");
        assert!(
            matches!(
                halt.kind,
                crate::ast::FailureKind::Denied | crate::ast::FailureKind::ConfirmDenied
            ),
            "refused by the envelope: {halt:?}"
        );
        assert_eq!(
            write_calls.lock().unwrap().len(),
            live_writes,
            "the denied tail op never executed"
        );
    }

    /// A-45: a recorded `parallel` plan replays green — the out-of-order-tolerant matcher absorbs
    /// whatever branch interleaving the recording happened to capture (`try_join_all` makes
    /// record-time dispatch order nondeterministic across branches).
    #[tokio::test]
    async fn replay_session_absorbs_parallel_branch_interleaving() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "parallel",
                "branches": [
                    { "name": "a", "body": [ { "kind": "call", "op": "echo",
                        "args": [{ "kind": "lit", "value": "left" }] } ] },
                    { "name": "b", "body": [ { "kind": "call", "op": "echo",
                        "args": [{ "kind": "lit", "value": "right" }] } ] }
                ]
            }]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("done")]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "fan out", &mut sink).await.unwrap();

        let mut rsink = CollectSink::default();
        let report =
            crate::replay::replay_session(&store, &engine.executor, &sid, None, &mut rsink)
                .await
                .unwrap();
        assert!(
            report.diverged.is_none(),
            "parallel replay is green: {report:?}"
        );
        assert_eq!(report.cells_consumed, report.cells_total);
        assert_eq!(report.cells_total, 2, "both branch cells recorded");
    }

    /// A-45: a recording whose plan diverges from its cells (here: a hand-built session whose
    /// `plan_source` names a DIFFERENT input than the recorded cell) surfaces `diverged` loudly —
    /// never a silent wrong-output replay.
    #[tokio::test]
    async fn replay_session_surfaces_divergence_loudly() {
        // Record a real turn to harvest a well-formed trace shape.
        let store = Arc::new(EventStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "x" }] }
            }]
        });
        let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("done")]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "run", &mut sink).await.unwrap();

        // Hand-build session B: same recorded CELLS, but a plan_source calling echo("y") — the
        // replayed dispatch's input hash matches no cell.
        let sid_b = store.create_session("mock").unwrap();
        let ast_y: crate::ast::DraftAst = serde_json::from_value(json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "y" }] }
            }]
        }))
        .unwrap();
        let key_y = flux_lang::runtime::flow_key(None, &ast_y.body);
        let turn_id = store.begin_turn(&sid_b, "run", "mock").unwrap();
        store
            .record_plan_attempt(
                &sid_b,
                turn_id,
                flux_events::PlanAttempt {
                    step: 1,
                    outcome: "accepted".into(),
                    error: None,
                    fingerprint: Some("f".into()),
                    plan_text: None,
                    phase: None,
                    plan_source: Some(flux_lang::format::format(&ast_y)),
                    delta_source: None,
                },
            )
            .unwrap();
        for ev in store.run_trace(&sid).unwrap() {
            let ev = match ev {
                flux_lang::ast::RunEvent::StatementCompleted {
                    node,
                    stmt,
                    value,
                    skipped,
                    ..
                } => flux_lang::ast::RunEvent::StatementCompleted {
                    plan: key_y.clone(),
                    node,
                    stmt,
                    value,
                    skipped,
                },
                other => other,
            };
            store
                .append(&sid_b, flux_events::NewEvent::run(ev))
                .unwrap();
        }

        let mut rsink = CollectSink::default();
        let report =
            crate::replay::replay_session(&store, &engine.executor, &sid_b, None, &mut rsink)
                .await
                .unwrap();
        assert!(
            report
                .diverged
                .as_deref()
                .is_some_and(|d| d.contains("no matching unconsumed recorded cell")),
            "divergence is loud: {report:?}"
        );
    }
}
