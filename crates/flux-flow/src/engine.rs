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
use flux_provider::{Provider, Request};
use flux_runtime::Executor;
use flux_session::SessionStore;

use crate::ast::DraftAst;
use crate::compile::{compile_turn, CompileOptions, TurnOutput};
use crate::registry::OpRegistry;
use crate::runtime::execute_flow;
use crate::state::FlowStore;

/// flux-flow's turn engine: a provider, the tool executor (safety envelope), the session message
/// store, and flux-flow's own value/symbol/trace store.
pub struct FlowEngine {
    pub provider: Box<dyn Provider>,
    pub executor: Executor,
    pub store: Arc<SessionStore>,
    pub flow: FlowStore,
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

    /// Run one user turn, abortable via `cancel` (checked between plan rounds). Every termination path
    /// persists exactly one assistant message, so the session stays a valid `user → assistant`
    /// alternation.
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

        self.store
            .append_message(session_id, &Message::user_text(user_input))?;

        // Agent identity + project context + any skills whose triggers match this turn — prepended to
        // the planner's own instructions inside `compile_turn`.
        let base_system = self.base_system_with_skills(user_input, sink);

        // Compact the persisted session if it has grown past the budget.
        self.maybe_compact(session_id, sink, cancel).await?;

        // The op catalog (a plan may use any *advertised* op). Pure DAG: the model's only tool is
        // `emit_plan`, so there is no research executor — reads are plan nodes, executed by the
        // multi-round loop. Evidence-gated surfacing happens inside `advertised_registry`.
        let ops = self.advertised_registry(Some(sink));
        let opts = CompileOptions {
            max_tokens: self.max_tokens,
            ..CompileOptions::default()
        };

        // Working conversation: seeded from the clean persisted log, then extended *ephemerally* with
        // each plan's results so the model can iterate within one turn. Only ONE assistant summary is
        // persisted (below); the ephemeral rounds never touch the session log.
        let mut working = self.store.load_messages(session_id)?;
        let mut answer: Option<String> = None;
        // Whether `answer` was already streamed to the sink during the loop (a chat turn or a plan's
        // closing reply). Terminal answers that were NOT streamed — a compile/provider failure or the
        // max-iterations fallback — are surfaced after the loop so a turn never ends silently.
        let mut shown = false;

        for _ in 0..self.max_iterations {
            if cancel.is_cancelled() {
                return self.finish_turn(session_id, sink, "(turn cancelled)", true);
            }
            let view = self.flow.view(session_id)?;
            let view_ref = (!view.symbols.is_empty()).then_some(&view);

            sink.planning(true);
            let compiled = compile_turn(
                &*self.provider,
                &self.model,
                &working,
                Some(&base_system),
                &ops,
                view_ref,
                None,
                Some(sink),
                opts.clone(),
            )
            .await;
            sink.planning(false);
            let out = match compiled {
                Ok(out) => out,
                // No fallback (one engine): a turn the planner can't compile fails cleanly, surfaced as
                // the assistant's answer so the session shape stays valid — and shown to the user (below)
                // so a provider failure (credit exhausted, auth, rate limit, transport) is never silent.
                Err(e) => {
                    answer = Some(format!("I couldn't produce a plan — {}", planner_error(&e)));
                    break;
                }
            };

            match out {
                TurnOutput::Chat(text) => {
                    sink.text_delta(&text);
                    shown = true;
                    answer = Some(text);
                    break;
                }
                TurnOutput::Plan(compiled) => {
                    // Surface the compiled plan before running it — this is what executes, so the turn
                    // is auditable (and visibly the planner, not a free-form tool loop).
                    sink.observation(&self.plan_observation(&compiled.ast));
                    match execute_flow(&self.flow, &self.executor, session_id, &compiled.ast, sink)
                        .await
                    {
                        Ok(outcome) => {
                            // The flow suspended on a top-level `await`: persist the resume point and end
                            // the turn — the user's next message resumes it (handled at the top of
                            // `run_turn`). The pre-await output already streamed through the sink.
                            if let Some(susp) = &outcome.suspension {
                                self.flow.save_suspension(
                                    session_id,
                                    &compiled.ast.body,
                                    susp.node,
                                    &susp.source,
                                )?;
                                let hint = "(awaiting your input — reply to continue the flow)";
                                sink.text_delta(hint);
                                answer = Some(hint.to_string());
                                shown = true;
                                break;
                            }
                            // Feed back the full transcript (EVERY read/call node's view), not just the
                            // last node — otherwise a plan that reads N files surfaces only file N and the
                            // model re-reads the rest every round (an infinite read loop). Both paths
                            // below see these results: the grounded `complete` call, and the next round.
                            let result = if !outcome.transcript.trim().is_empty() {
                                outcome.transcript.clone()
                            } else if !outcome.result.trim().is_empty() {
                                outcome.result.clone()
                            } else {
                                format!("(ran {} step(s), no textual output)", outcome.steps)
                            };
                            working.push(Message::assistant(vec![ContentBlock::Text {
                                text: format!("Ran a {}-step plan.", outcome.steps),
                            }]));
                            working.push(Message::user_text(format!(
                                "[results]\n{result}\n\nAnswer the user now, or emit another plan if \
                                 more work is needed."
                            )));
                            // The plan carried a `complete` directive ⇒ it finishes the request. Render
                            // the final message NOW, grounded in the results we just fed back (never a
                            // pre-composed summary), then end the turn.
                            if let Some(directive) = compiled.complete {
                                let summary = crate::compile::render_completion(
                                    &*self.provider,
                                    &self.model,
                                    &working,
                                    &directive,
                                    self.max_tokens,
                                )
                                .await?;
                                sink.text_delta(&summary);
                                shown = true;
                                answer = Some(summary);
                                break;
                            }
                            // No `complete` ⇒ keep looping: next round the model answers in prose (which
                            // ends the turn) or emits another plan — the standard agent loop.
                        }
                        // Feed the error back so the model can self-correct (model-in-the-loop),
                        // bounded by max_iterations.
                        Err(e) => {
                            working.push(Message::assistant(vec![ContentBlock::Text {
                                text: "A plan step failed.".to_string(),
                            }]));
                            working.push(Message::user_text(format!(
                                "[plan error]\n{e}\n\nAdjust and try another plan, or give your final \
                                 answer in prose."
                            )));
                        }
                    }
                }
            }
        }

        let answer = answer.unwrap_or_else(|| {
            format!(
                "Reached the maximum of {} plan iterations for this turn; stopping.",
                self.max_iterations
            )
        });
        // A compile/provider failure or the max-iterations fallback never streamed anything — emit it
        // here so the turn shows *why* it ended instead of returning to the prompt in silence.
        if !shown {
            sink.text_delta(&answer);
        }
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
    pub async fn plan_turn(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
    ) -> Result<Option<DraftAst>> {
        self.store
            .append_message(session_id, &Message::user_text(user_input))?;
        let base_system = self.base_system_with_skills(user_input, sink);
        let ops = self.advertised_registry(Some(sink));
        let view = self.flow.view(session_id)?;
        let view_ref = (!view.symbols.is_empty()).then_some(&view);
        let opts = CompileOptions {
            max_tokens: self.max_tokens,
            ..CompileOptions::default()
        };
        let conversation = self.store.load_messages(session_id)?;
        sink.planning(true);
        let out = compile_turn(
            &*self.provider,
            &self.model,
            &conversation,
            Some(&base_system),
            &ops,
            view_ref,
            None,
            Some(sink),
            opts,
        )
        .await;
        sink.planning(false);
        // Surface a provider failure (credit, auth, rate limit, transport) with a readable message
        // rather than the raw API JSON body — the REPL prints this `error:` line directly.
        let out = out.map_err(|e| flux_core::Error::Other(planner_error(&e)))?;

        match out {
            TurnOutput::Plan(compiled) => {
                let rendered = crate::render::render_pretty(&compiled.ast);
                sink.observation(&self.plan_observation(&compiled.ast));
                self.store.append_message(
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
                self.store.append_message(
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
        self.store.append_message(
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
        self.store
            .append_message(session_id, &Message::user_text(user_input))?;
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
        let messages = self.store.load_messages(session_id)?;
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
        self.store.rewrite_messages(session_id, &new_msgs)?;

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

    fn engine_with(responses: VecDeque<Vec<Chunk>>, store: Arc<SessionStore>) -> FlowEngine {
        engine_with_provider(
            Box::new(MockProvider {
                responses: Mutex::new(responses),
            }),
            store,
        )
    }

    fn engine_with_provider(provider: Box<dyn Provider>, store: Arc<SessionStore>) -> FlowEngine {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-engine-{}-{}",
            std::process::id(),
            store.latest_session_id().ok().flatten().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["echo".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        FlowEngine {
            provider,
            executor,
            store,
            flow: FlowStore::in_memory().unwrap(),
            model: "mock".into(),
            system_prompt: "test".into(),
            max_tokens: 1024,
            max_iterations: 5,
            skills: Vec::new(),
            compact_threshold_chars: 0,
            groups: Vec::new(),
            cwd: dir,
        }
    }

    #[tokio::test]
    async fn engine_plans_executes_and_keeps_session_shape() {
        let store = Arc::new(SessionStore::in_memory().unwrap());
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
        let msgs = store.load_messages(&sid).unwrap();
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
        let store = Arc::new(SessionStore::in_memory().unwrap());
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
        let msgs = store.load_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 2, "user + the prose answer");
        assert!(msgs[1].text().contains("Echoed hi."));
    }

    /// A plan that `await`s suspends the flow across the turn boundary: turn 1 runs the prefix and
    /// stops at the await; turn 2's message resumes it (binding the awaited value, running the
    /// remainder) without re-running the prefix or recompiling.
    #[tokio::test]
    async fn await_suspends_a_turn_then_the_next_message_resumes_it() {
        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();

        // Prefix echo, then await the user's name, then echo a greeting interpolating it.
        let plan_ast = json!({
            "body": [
                { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "your name?" }] },
                { "kind": "await", "binding": "name", "source": "user_input" },
                { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "Hello {name}" }] }
            ]
        });
        // Only ONE provider response is needed — the resume turn bypasses the planner entirely.
        let responses = VecDeque::from(vec![emit_plan(plan_ast)]);
        let engine = engine_with(responses, store.clone());

        // Turn 1: the prefix echo runs, then the flow suspends at the await.
        let mut sink1 = CollectSink::default();
        engine.run_turn(&sid, "greet me", &mut sink1).await.unwrap();
        assert_eq!(
            sink1.tools,
            vec!["echo"],
            "only the prefix echo ran before the await"
        );
        assert!(
            sink1.text.contains("awaiting"),
            "the turn ends prompting for input"
        );

        // Turn 2: the reply resumes the flow — the prefix is NOT re-run, the awaited value binds, and
        // the post-await echo (interpolating it) runs to completion.
        let mut sink2 = CollectSink::default();
        engine.run_turn(&sid, "Ada", &mut sink2).await.unwrap();
        assert_eq!(
            sink2.tools,
            vec!["echo"],
            "only the post-await echo ran (prefix not replayed)"
        );
        assert!(
            sink2.text.contains("Hello Ada"),
            "the awaited reply flowed into the resumed flow: {:?}",
            sink2.text
        );
        let name = engine
            .flow
            .resolve(&sid, &SymbolName("name".into()))
            .unwrap()
            .and_then(|id| engine.flow.get_value(&id).unwrap());
        assert_eq!(name, Some(crate::ast::Value::String("Ada".into())));

        // The suspension was consumed — a third turn would compile fresh, not resume.
        assert!(engine.flow.take_suspension(&sid).unwrap().is_none());
    }

    #[tokio::test]
    async fn plan_with_complete_renders_grounded_summary() {
        let store = Arc::new(SessionStore::in_memory().unwrap());
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
        let msgs = store.load_messages(&sid).unwrap();
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
        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let responses = VecDeque::from(vec![prose("Here's the explanation.")]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "explain", &mut sink).await.unwrap();

        assert!(sink.tools.is_empty(), "a chat turn runs no ops");
        assert!(sink.text.contains("explanation"));
        let msgs = store.load_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs.iter().all(|m| !m.content.is_empty()));
    }

    #[tokio::test]
    async fn provider_error_is_surfaced_not_silent() {
        // A provider/API failure during planning (e.g. credit exhausted) must reach the user — the
        // turn used to store the answer but never emit it, ending the turn in silence.
        let store = Arc::new(SessionStore::in_memory().unwrap());
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
        let msgs = store.load_messages(&sid).unwrap();
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
