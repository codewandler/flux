//! The engine: flux-flow's single turn loop, and the seat of "the LLM plans, the runtime runs".
//!
//! There is exactly **one** engine. Every turn the model is a compiler front-end: it either emits a
//! typed Flux-Lang plan (a graph the runtime executes through [`Executor::dispatch`](flux_runtime))
//! or answers in prose (a chat turn). The free-form "one provider-native tool call at a time" loop is
//! gone — a single op is just a one-node plan. `flux plan` shows exactly what a turn would
//! run, because the same [`compile_turn`] drives the engine and the CLI.
//!
//! Per turn: append the user message → compile a plan (pure DAG — the model's only tool is emit_plan) →
//! risk-gated execution via [`execute_flow`] (per-op approval through the same envelope) → feed the
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
use flux_core::{Chunk, ContentBlock, Message, Result, Usage};
use flux_events::EventStore;
use flux_provider::{Provider, Request};
use flux_runtime::Executor;

use crate::ast::DraftAst;
use crate::compile::{compile_turn, CompileOptions, TurnOutput};
use crate::composites::DynamicComposites;
use crate::registry::OpRegistry;
use crate::runtime::{execute_flow, resume_flow_with_composites};
use crate::state::FlowStore;

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
    /// Skills whose triggers, when matched against a turn's input, inject their body into that turn's
    /// system prompt (and record a `skill.activated` observation).
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
        executor.allow(&["plan", "run_plan", "observe", "evidence", "metrics"]);
        composites.validate_base(executor.registry())?;
        let agent_loop = load_agent_loop(&cwd)?;
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
            skills,
            compact_threshold_chars,
            groups,
            cwd,
        })
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
        if let Some((body, node, _source)) = self.flow.take_suspension(session_id)? {
            return self
                .resume_suspended(session_id, user_input, body, node, sink)
                .await;
        }

        self.events
            .record_message(session_id, &Message::user_text(user_input))?;
        // Non-fatal: a DB hiccup must never prevent a turn from running.
        let turn_id = self
            .events
            .begin_turn(session_id, user_input, &self.model)
            .unwrap_or(-1);

        // Agent identity + project context + any skills whose triggers match this turn — prepended to
        // the planner's own instructions inside `compile_turn`.
        let base_system = self.base_system_with_skills(user_input, sink);

        // Evidence-gated surfacing for this turn: probe the workspace signals once and hand the
        // advertised op set to the loop host, so every planner iteration sees the gated catalog.
        let advertised = self.surfaced_for_turn(sink);

        // Compact the persisted session if it has grown past the budget.
        self.maybe_compact(session_id, sink, cancel).await?;

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
        );
        // Thread this turn's cancellation into the tool context so a spawning tool (`task`) can hand a
        // child token to its sub-agent — cancelling the parent turn then cancels the child.
        self.executor.context().set_cancel(cancel.clone());

        // Per-turn iteration count: snapshot the cumulative `turn.iteration` evidence now so we can
        // report only THIS turn's rounds. The executor (and its evidence log) is shared and persists
        // across turns, so an unscoped count grows monotonically over a long-lived served agent.
        let iter_base = self.executor.evidence().by_kind("turn.iteration").count();
        // Same scoping trick for sub-agent usage (C-06 rollup): `task` (flux-orchestrate) records a
        // `subagent.usage` observation per completed sub-agent call onto this SAME shared evidence
        // log — the cross-crate side-channel `ToolResult` (a plain string) can't carry structured
        // usage through. Snapshotting the count now (not after the turn) means only sub-agents
        // spawned by THIS turn are folded in, never a prior turn's.
        let subagent_base = self.executor.evidence().by_kind("subagent.usage").count();

        let mut outer = crate::loop_host::SharedSink::new(channel.clone());
        let flow_fut = execute_flow(
            &self.flow,
            &self.executor,
            session_id,
            &self.agent_loop,
            &mut outer,
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
                    return self.finish_turn(session_id, sink, "(turn cancelled)", true, usage);
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
        self.finish_turn(session_id, sink, &answer, false, usage)
    }

    /// Compile a single instruction into a [`TurnOutput`] using this engine's full catalog + current
    /// session symbols — *without executing*. The one-shot `--plan` surface uses this, so what it shows
    /// is exactly what the engine would run.
    pub async fn compile_once(
        &self,
        session_id: &str,
        prompt: &str,
        ask: Option<&dyn crate::compile::AskUser>,
    ) -> Result<TurnOutput> {
        self.composites
            .ensure_session_loaded(&self.flow, session_id)?;
        let ops = self.advertised_registry(Some(session_id), None);
        let view = self.flow.view(session_id)?;
        let view_ref = (!view.symbols.is_empty()).then_some(&view);
        let opts = CompileOptions {
            max_tokens: self.max_tokens,
            ..CompileOptions::default()
        };
        let (out, _usage) = compile_turn(
            &*self.provider,
            &self.model,
            &[Message::user_text(prompt)],
            Some(&self.system_prompt),
            &ops,
            view_ref,
            ask,
            None,
            opts,
        )
        .await?;
        Ok(out)
    }

    /// A plan-mode turn (the REPL `/plan` toggle): compile ONE plan from the conversation, render it,
    /// and persist it as the assistant turn (so a refinement sees it) — but DO NOT execute. Returns the
    /// AST for the caller to hold and run later (`/run`); a chat answer is surfaced and returns `None`.
    /// Abortable via `cancel`: a Ctrl-C mid-compose drops the in-flight planner request and returns
    /// `Ok(None)` (nothing to run).
    pub async fn plan_turn(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
    ) -> Result<Option<DraftAst>> {
        self.events
            .record_message(session_id, &Message::user_text(user_input))?;
        let base_system = self.base_system_with_skills(user_input, sink);
        self.composites
            .ensure_session_loaded(&self.flow, session_id)?;
        let ops = self.advertised_registry(Some(session_id), Some(sink));
        let view = self.flow.view(session_id)?;
        let view_ref = (!view.symbols.is_empty()).then_some(&view);
        let opts = CompileOptions {
            max_tokens: self.max_tokens,
            ..CompileOptions::default()
        };
        let conversation = self.events.conversation(session_id)?;
        sink.planning(true);
        // Race the planner call against `cancel` so Ctrl-C mid-compose drops the in-flight request
        // (dropping the future aborts its HTTP) instead of blocking until the plan lands. The future
        // borrows `sink`, so scope it in a block: its drop at the block's end releases the borrow
        // before we touch `sink` again. `None` => cancelled.
        let out = {
            let fut = compile_turn(
                &*self.provider,
                &self.model,
                &conversation,
                Some(&base_system),
                &ops,
                view_ref,
                None,
                Some(sink),
                opts,
            );
            tokio::pin!(fut);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                res = &mut fut => Some(res),
            }
        };
        sink.planning(false);
        let Some(out) = out else {
            // Cancelled mid-compose: nothing to run; end the turn cleanly.
            sink.turn_end(None);
            return Ok(None);
        };
        // Surface a provider failure (credit, auth, rate limit, transport) with a readable message
        // rather than the raw API JSON body — the REPL prints this `error:` line directly.
        let (out, usage) = out.map_err(|e| flux_core::Error::Other(planner_error(&e)))?;
        // The compose-a-plan call is the turn's only model call here, so its usage IS the turn's.
        let usage = (usage.total() > 0).then_some(usage);

        match out {
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

    /// Build the op catalog view for a turn, advertising only ops whose group is surfaced by the
    /// current workspace signals (an empty `groups` manifest disables gating, advertising everything).
    /// Execution is unaffected — `OpRegistry::get` still resolves any registered op, so a pre-authored
    /// flow naming a hidden-group op keeps working. `sink`, when given, receives a `groups.active`
    /// observation for visibility.
    fn advertised_registry(
        &self,
        session_id: Option<&str>,
        sink: Option<&mut dyn AgentSink>,
    ) -> OpRegistry<'_> {
        let reg = self.executor.registry();
        let composites = session_id
            .map(|sid| self.composites.active_for_session(sid))
            .unwrap_or_default();
        let (advertised, active) = surfaced_op_names(reg, &self.groups, &self.cwd);
        if let (Some(sink), Some(active)) = (sink, active.as_ref()) {
            self.record_active_groups(active, sink);
        }
        OpRegistry::new(reg)
            .with_owned_composites(composites)
            .with_advertised(advertised)
    }

    /// This turn's advertised op-name set, computed once per turn and handed to the loop host so the
    /// self-hosted loop's `plan` op advertises (and enforces) the SAME gated catalog as the preview
    /// paths — before A-04 the loop built an ungated registry, so every op (incl. `bash`) was
    /// advertised every turn and the opt-in `shell` group gated nothing. Records the `groups.active`
    /// observation when gating is on.
    fn surfaced_for_turn(&self, sink: &mut dyn AgentSink) -> std::collections::HashSet<String> {
        let (advertised, active) =
            surfaced_op_names(self.executor.registry(), &self.groups, &self.cwd);
        if let Some(active) = active.as_ref() {
            self.record_active_groups(active, sink);
        }
        advertised
    }

    /// Record (audit + surface) which evidence-gated groups are active this turn, so the user can see
    /// what the workspace surfaced. Mirrors the skill-activation observation pattern.
    fn record_active_groups(
        &self,
        active: &std::collections::HashSet<String>,
        sink: &mut dyn AgentSink,
    ) {
        let mut names: Vec<&str> = active.iter().map(String::as_str).collect();
        names.sort_unstable();
        let obs = flux_evidence::Observation::new(
            "groups.active",
            flux_evidence::Phase::Turn,
            serde_json::json!({ "groups": names }),
        );
        self.executor.observe(obs.clone());
        sink.observation(&obs);
    }

    /// The agent identity + project context + any skills whose triggers match this turn — the base the
    /// planner prompt is appended to (shared by `run_turn` and `plan_turn`).
    fn base_system_with_skills(&self, user_input: &str, sink: &mut dyn AgentSink) -> String {
        let mut base_system = self.system_prompt.clone();
        for skill in flux_skill::active_for(
            &self.skills,
            user_input,
            flux_skill::ActivationLimits::default(),
        ) {
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
    fn finish_turn(
        &self,
        session_id: &str,
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
        body: Vec<flux_lang::ast::Node>,
        node: flux_lang::ast::NodeId,
        sink: &mut dyn AgentSink,
    ) -> Result<()> {
        self.events
            .record_message(session_id, &Message::user_text(user_input))?;
        let input = flux_lang::ast::Value::String(user_input.to_string());
        self.composites
            .ensure_session_loaded(&self.flow, session_id)?;
        let composites = self.composites.active_for_session(session_id);
        let outcome = match resume_flow_with_composites(
            &self.flow,
            &self.executor,
            session_id,
            &body,
            node,
            input,
            &composites,
            sink,
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                let msg = format!("The resumed flow failed — {e}");
                sink.text_delta(&msg);
                return self.finish_turn(session_id, sink, &msg, true, None);
            }
        };

        // Suspended again on a later `await`: persist the new resume point and wait for more input.
        if let Some(susp) = &outcome.suspension {
            self.flow
                .save_suspension(session_id, &body, susp.node, &susp.source)?;
            let hint = "(awaiting your input — reply to continue the flow)";
            sink.text_delta(hint);
            return self.finish_turn(session_id, sink, hint, false, None);
        }

        // Completed: the flow's own output is the answer (a model-grounded summary is a later refinement).
        let answer = if !outcome.result.trim().is_empty() {
            outcome.result.trim().to_string()
        } else {
            format!("Resumed and completed ({} step(s)).", outcome.steps)
        };
        sink.text_delta(&answer);
        self.finish_turn(session_id, sink, &answer, false, None)
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
        if self.compact_threshold_chars == 0 {
            return Ok(());
        }
        let messages = self.events.conversation(session_id)?;
        if messages.len() < 4 {
            return Ok(());
        }
        let total: usize = messages
            .iter()
            .map(|m| serde_json::to_string(m).map(|s| s.len()).unwrap_or(0))
            .sum();
        if total <= self.compact_threshold_chars {
            return Ok(());
        }

        let keep = 2.min(messages.len());
        let mut split = messages.len() - keep;
        while split > 0 && has_tool_result(&messages[split]) {
            split -= 1;
        }
        if split == 0 {
            return Ok(()); // can't summarize without splitting a tool_use/tool_result pair
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
        let req = Request::new(self.model.clone(), prompt).with_max_tokens(1024);
        let mut stream = self.provider.stream(req).await?;
        let mut summary = String::new();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                chunk = stream.next() => {
                    let Some(chunk) = chunk else { break };
                    if let Chunk::TextDelta(t) = chunk? {
                        summary.push_str(&t);
                    }
                }
            }
        }
        if summary.trim().is_empty() {
            return Ok(());
        }

        let mut new_msgs = vec![Message::user_text(format!(
            "[summary of earlier conversation]\n{}",
            summary.trim()
        ))];
        new_msgs.extend(recent.iter().cloned());
        let to = new_msgs.len();
        self.events.record_compaction(session_id, &new_msgs)?;

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
        Ok(())
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
) -> (
    std::collections::HashSet<String>,
    Option<std::collections::HashSet<String>>,
) {
    if groups.is_empty() {
        let advertised = reg
            .specs()
            .iter()
            .filter(|s| s.group.as_deref() != Some(flux_runtime::REFLECT_GROUP))
            .map(|s| s.name.clone())
            .collect();
        return (advertised, None);
    }
    let signals = flux_runtime::detect_signals(cwd);
    let active = flux_evidence::resolve_active_groups(groups, &signals);
    let advertised = flux_runtime::advertised_op_names(&reg.specs(), groups, &active);
    (advertised, Some(active))
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

/// The loop-machinery ops a turn dispatches to *drive* the loop (not to do the user's work). Their
/// tool-call/result events are filtered out of the user-facing sink so the surface shows the actual
/// operations (`read`/`edit`/`bash`/…) the inner `run_plan` performs, not the plumbing.
const MACHINERY_OPS: &[&str] = &[
    "plan", "run_plan", "observe", "evidence", "metrics", "grade",
];

/// Whether the loop-machinery ops are revealed on the surface — the CLI `--show-loop`, exported as
/// `FLUX_SHOW_LOOP` so the engine reads it without new plumbing. When set, the user watches the loop
/// iterate (`plan → run_plan → observe`) instead of only the work the inner plan performs.
pub fn show_loop() -> bool {
    std::env::var_os("FLUX_SHOW_LOOP").is_some()
}

/// Drain one captured sink event onto the real sink. By default the loop-machinery tool calls/results
/// are dropped (the surface shows real work, not plumbing); `reveal` keeps them so `--show-loop` can
/// stream the loop's own iterations.
fn drain_event(ev: crate::loop_host::SinkEvent, sink: &mut dyn AgentSink, reveal: bool) {
    use crate::loop_host::SinkEvent;
    let machinery = match &ev {
        SinkEvent::ToolCall(name, _) | SinkEvent::ToolResult(name, _) => {
            MACHINERY_OPS.contains(&name.as_str())
        }
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
    if let flux_core::Error::Api { status, message } = e {
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
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
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
    }
    impl AgentSink for CollectSink {
        fn text_delta(&mut self, t: &str) {
            self.text.push_str(t);
        }
        fn tool_call(&mut self, name: &str, _input: &serde_json::Value) {
            self.tools.push(name.to_string());
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

    fn engine_with(responses: VecDeque<Vec<Chunk>>, events: Arc<EventStore>) -> FlowEngine {
        engine_with_provider(
            Box::new(MockProvider {
                responses: Mutex::new(responses),
            }),
            events,
        )
    }

    fn engine_with_provider(provider: Box<dyn Provider>, events: Arc<EventStore>) -> FlowEngine {
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
            5,
            Vec::new(),
            0,
            Vec::new(),
            dir,
        )
        .unwrap()
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
        let engine = engine_with_provider(provider, store.clone());
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
        let engine = engine_with(responses, store.clone());
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
            .loop_host
            .set_model(provider_b, "model-b".to_string());
        store.set_model(&sid, "model-b").unwrap();

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
}
