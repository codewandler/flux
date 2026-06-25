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
        self.store
            .append_message(session_id, &Message::user_text(user_input))?;

        // Agent identity + project context + any skills whose triggers match this turn — prepended to
        // the planner's own instructions inside `compile_turn`.
        let base_system = self.base_system_with_skills(user_input, sink);

        // Compact the persisted session if it has grown past the budget.
        self.maybe_compact(session_id, sink, cancel).await?;

        // The full op catalog (a plan may use any op). Pure DAG: the model's only tool is `emit_plan`,
        // so there is no research executor — reads are plan nodes, executed by the multi-round loop.
        let ops = OpRegistry::new(self.executor.registry());
        let opts = CompileOptions {
            max_tokens: self.max_tokens,
            ..CompileOptions::default()
        };

        // Working conversation: seeded from the clean persisted log, then extended *ephemerally* with
        // each plan's results so the model can iterate within one turn. Only ONE assistant summary is
        // persisted (below); the ephemeral rounds never touch the session log.
        let mut working = self.store.load_messages(session_id)?;
        let mut answer: Option<String> = None;

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
                opts.clone(),
            )
            .await;
            sink.planning(false);
            let out = match compiled {
                Ok(out) => out,
                // No fallback (one engine): a turn the planner can't compile fails cleanly, surfaced as
                // the assistant's answer so the session shape stays valid.
                Err(e) => {
                    answer = Some(format!("I couldn't produce a plan: {e}"));
                    break;
                }
            };

            match out {
                TurnOutput::Chat(text) => {
                    sink.text_delta(&text);
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
                            // The plan carried a closing message ⇒ it completes the request: show it and
                            // end the turn — one round, no extra "summarize" call.
                            if let Some(reply) = compiled.reply {
                                sink.text_delta(&reply);
                                answer = Some(reply);
                                break;
                            }
                            // No reply ⇒ the model needs to see the results before it can answer. Feed
                            // them back and loop.
                            let result = if outcome.result.trim().is_empty() {
                                format!("(ran {} step(s), no textual output)", outcome.steps)
                            } else {
                                outcome.result.clone()
                            };
                            working.push(Message::assistant(vec![ContentBlock::Text {
                                text: format!("Ran a {}-step plan.", outcome.steps),
                            }]));
                            working.push(Message::user_text(format!(
                                "[results]\n{result}\n\nAnswer the user now, or emit another plan if \
                                 more work is needed."
                            )));
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
        let ops = OpRegistry::new(self.executor.registry());
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
        let ops = OpRegistry::new(self.executor.registry());
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
            opts,
        )
        .await;
        sink.planning(false);
        let out = out?;

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

    /// If the session has grown past `compact_threshold_chars`, summarize everything but the most
    /// recent messages into one synthetic message and rewrite the session log. A no-op when
    /// compaction is disabled or the session is small. Snaps the boundary back so a `tool_result` is
    /// never orphaned from the `tool_use` that compaction would summarize away.
    async fn maybe_compact(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;

    use flux_core::StopReason;
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

    /// One model turn that emits an `emit_plan` tool call carrying `ast` (and an optional `reply`).
    fn emit_plan(ast: serde_json::Value) -> Vec<Chunk> {
        emit_plan_reply(ast, None)
    }
    fn emit_plan_reply(ast: serde_json::Value, reply: Option<&str>) -> Vec<Chunk> {
        let mut input = json!({ "ast": ast });
        if let Some(r) = reply {
            input["reply"] = json!(r);
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

    fn engine_with(responses: VecDeque<Vec<Chunk>>, store: Arc<SessionStore>) -> FlowEngine {
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
            provider: Box::new(MockProvider {
                responses: Mutex::new(responses),
            }),
            executor,
            store,
            flow: FlowStore::in_memory().unwrap(),
            model: "mock".into(),
            system_prompt: "test".into(),
            max_tokens: 1024,
            max_iterations: 5,
            skills: Vec::new(),
            compact_threshold_chars: 0,
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
    async fn plan_with_reply_ends_in_one_round() {
        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let plan_ast = json!({
            "body": [{
                "kind": "bind", "name": "greeting",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        // Round 1 emits a plan WITH a closing reply → the turn ends after running it. The second
        // response must never be consumed (no "summarize" round).
        let responses = VecDeque::from(vec![
            emit_plan_reply(plan_ast, Some("Echoed hi.")),
            prose("SHOULD NOT BE REACHED"),
        ]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "echo hi", &mut sink).await.unwrap();

        assert_eq!(sink.tools, vec!["echo"], "the plan still executed");
        let msgs = store.load_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 2, "user + the reply — one round");
        assert!(msgs[1].text().contains("Echoed hi."));
        assert!(
            !msgs[1].text().contains("SHOULD NOT"),
            "the second model round must not run"
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
}
