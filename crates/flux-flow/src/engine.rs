//! The engine: flux-flow's single turn loop, and the seat of "the LLM plans, the runtime runs".
//!
//! There is exactly **one** engine. Every turn the model is a compiler front-end: it either emits a
//! typed Flux-Lang plan (a graph the runtime executes through [`Executor::dispatch`](flux_runtime))
//! or answers in prose (a chat turn). The free-form "one provider-native tool call at a time" loop is
//! gone — a single op is just a one-node plan. `flux --compile-only` shows exactly what a turn would
//! run, because the same [`compile_turn`] drives the engine and the CLI.
//!
//! Per turn: append the user message → compile a plan (pure DAG — the model's only tool is emit_plan) →
//! risk-gated execution via [`execute_flow`] (per-op approval through the same envelope) → feed the
//! result back *ephemerally* so the model can iterate (read → fix → re-run) → persist **one** assistant
//! summary. The persisted session log is pure `user → assistant(text)` alternation: raw op outputs
//! never re-enter history (the "don't re-send" token win), which also removes the session-shape bug
//! class (no persisted tool_use/tool_result pairs). Symbols + summaries carry state forward.
//!
//! The engine reuses [`flux_agent::AgentSink`] so a surface (CLI/TUI) can drive it with the same sink.
//! Every op still executes through `Executor::dispatch` — there is no new bypass surface.

use std::sync::Arc;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use flux_agent::AgentSink;
use flux_core::{Chunk, ContentBlock, Message, Result};
use flux_events::EventStore;
use flux_provider::{Provider, Request};
use flux_runtime::Executor;

use crate::ast::DraftAst;
use crate::compile::{compile_turn, CompileOptions, TurnOutput};
use crate::registry::OpRegistry;
use crate::runtime::execute_flow;
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
            String::new(),
            init_sink,
            opts,
        );
        let agent_loop = load_agent_loop(&cwd)?;
        Ok(FlowEngine {
            provider,
            executor,
            events,
            flow,
            agent_loop,
            loop_host,
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

        // Compact the persisted session if it has grown past the budget.
        self.maybe_compact(session_id, sink, cancel).await?;

        // Drive the flux-lang agent loop (`agent_loop`) through an OWNED channel sink — the `'static`
        // loop host holds it for reentrant `run_plan` — draining its events onto the borrowed `sink`
        // LIVE (inner ops stream as they happen; the loop-machinery ops are filtered, see `drain_event`).
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::loop_host::SinkEvent>();
        let channel: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
            crate::loop_host::ChannelSink::new(tx),
        ));
        self.loop_host
            .set_turn(session_id.to_string(), Some(base_system), channel.clone());

        let mut outer = crate::loop_host::SharedSink::new(channel.clone());
        let flow_fut = execute_flow(
            &self.flow,
            &self.executor,
            session_id,
            &self.agent_loop,
            &mut outer,
        );
        tokio::pin!(flow_fut);

        let outcome = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    while let Ok(ev) = rx.try_recv() { drain_event(ev, sink); }
                    let _ = self.events.end_turn(session_id, turn_id, "cancelled", 0, "(turn cancelled)");
                    return self.finish_turn(session_id, sink, "(turn cancelled)", true);
                }
                maybe = rx.recv() => {
                    if let Some(ev) = maybe { drain_event(ev, sink); }
                }
                res = &mut flow_fut => {
                    while let Ok(ev) = rx.try_recv() { drain_event(ev, sink); }
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
        let iterations = self.executor.evidence().by_kind("turn.iteration").count() as u32;
        let _ = self
            .events
            .end_turn(session_id, turn_id, tag, iterations, &answer);
        self.finish_turn(session_id, sink, &answer, false)
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
        let ops = self.advertised_registry(None);
        let view = self.flow.view(session_id)?;
        let view_ref = (!view.symbols.is_empty()).then_some(&view);
        let opts = CompileOptions {
            max_tokens: self.max_tokens,
            ..CompileOptions::default()
        };
        compile_turn(
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
        .await
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
        let ops = self.advertised_registry(Some(sink));
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
        let out = out.map_err(|e| flux_core::Error::Other(planner_error(&e)))?;

        match out {
            TurnOutput::Plan(compiled) => {
                let rendered = crate::render::render_pretty(&compiled.ast);
                sink.observation(&self.plan_observation(&compiled.ast));
                self.events.record_message(
                    session_id,
                    &Message::assistant(vec![ContentBlock::Text {
                        text: format!("Proposed plan:\n{rendered}"),
                    }]),
                )?;
                sink.turn_end(None);
                Ok(Some(compiled.ast))
            }
            TurnOutput::Chat(text) => {
                sink.text_delta(&text);
                self.events.record_message(
                    session_id,
                    &Message::assistant(vec![ContentBlock::Text { text }]),
                )?;
                sink.turn_end(None);
                Ok(None)
            }
        }
    }

    /// The `flow.plan` observation surfaced before a plan executes: the plain-rendered tree (for any
    /// sink), the AST (so a terminal surface can syntax-highlight it), and the risk preview (for a badge).
    fn plan_observation(&self, ast: &crate::ast::DraftAst) -> flux_evidence::Observation {
        let risk = crate::runtime::plan_risk(ast, self.executor.registry());
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
    fn advertised_registry(&self, sink: Option<&mut dyn AgentSink>) -> OpRegistry<'_> {
        let reg = self.executor.registry();
        if self.groups.is_empty() {
            return OpRegistry::new(reg);
        }
        let signals = flux_runtime::detect_signals(&self.cwd);
        let active = flux_evidence::resolve_active_groups(&self.groups, &signals);
        if let Some(sink) = sink {
            self.record_active_groups(&active, sink);
        }
        let advertised = flux_runtime::advertised_op_names(&reg.specs(), &self.groups, &active);
        OpRegistry::new(reg).with_advertised(advertised)
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
        for skill in &self.skills {
            if skill.matches(user_input) {
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
        }
        base_system
    }

    /// Persist the single assistant message for this turn (keeping the `user → assistant` session
    /// shape) and end the turn. `cancelled` records the audit observation.
    fn finish_turn(
        &self,
        session_id: &str,
        sink: &mut dyn AgentSink,
        answer: &str,
        cancelled: bool,
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
        self.events.record_message(
            session_id,
            &Message::assistant(vec![ContentBlock::Text {
                text: answer.to_string(),
            }]),
        )?;
        sink.turn_end(None);
        Ok(())
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
        let outcome = match crate::runtime::resume_flow(
            &self.flow,
            &self.executor,
            session_id,
            &body,
            node,
            input,
            sink,
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                let msg = format!("The resumed flow failed — {e}");
                sink.text_delta(&msg);
                return self.finish_turn(session_id, sink, &msg, true);
            }
        };

        // Suspended again on a later `await`: persist the new resume point and wait for more input.
        if let Some(susp) = &outcome.suspension {
            self.flow
                .save_suspension(session_id, &body, susp.node, &susp.source)?;
            let hint = "(awaiting your input — reply to continue the flow)";
            sink.text_delta(hint);
            return self.finish_turn(session_id, sink, hint, false);
        }

        // Completed: the flow's own output is the answer (a model-grounded summary is a later refinement).
        let answer = if !outcome.result.trim().is_empty() {
            outcome.result.trim().to_string()
        } else {
            format!("Resumed and completed ({} step(s)).", outcome.steps)
        };
        sink.text_delta(&answer);
        self.finish_turn(session_id, sink, &answer, false)
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

/// Load the agent loop: a `.flux/agent-loop.flux` in the workspace overrides the built-in (so a project
/// can shape its own loop), otherwise the compiled-in `agent-loop.flux`. Parsed as a [`DraftAst`].
fn load_agent_loop(cwd: &std::path::Path) -> Result<DraftAst> {
    const BUILTIN: &str = include_str!("../assets/agent-loop.flux");
    let override_path = cwd.join(".flux").join("agent-loop.flux");
    let src = std::fs::read_to_string(&override_path).unwrap_or_else(|_| BUILTIN.to_string());
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

/// Drain one captured sink event onto the real sink, dropping the loop-machinery tool calls/results.
fn drain_event(ev: crate::loop_host::SinkEvent, sink: &mut dyn AgentSink) {
    use crate::loop_host::SinkEvent;
    let machinery = match &ev {
        SinkEvent::ToolCall(name, _) | SinkEvent::ToolResult(name, _) => {
            MACHINERY_OPS.contains(&name.as_str())
        }
        _ => false,
    };
    if !machinery {
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
        // The flux-lang agent loop calls these — register them so a turn can run.
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(
                &[
                    "echo".into(),
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
