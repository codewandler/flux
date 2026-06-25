//! The engine: flux-flow's own turn loop, and the seat of the "the LLM plans, the runtime runs"
//! model. It is a strict superset of the legacy `flux-agent` loop — the free-form "one tool at a
//! time" behavior is the degenerate single-op case — and additionally stores each result as an
//! immutable [`Value`](crate::ast::Value), binds a session symbol, projects `view(Session)` into the
//! system prompt, and records the run-event trace.
//!
//! M1 runs the single-op fast path: the provider round-trips are identical to the legacy loop (so
//! latency is unchanged), and the session message log keeps exactly the same shape invariants —
//! never an empty assistant message, a split tool_use/tool_result pair, or a user-after-user
//! sequence. Skill activation and context compaction match the legacy loop, so this can be the sole
//! engine without regression. Multi-op planned flows (one assistant summary per turn — the
//! don't-re-send win) build on this loop in later milestones.
//!
//! The engine reuses [`flux_agent::AgentSink`] so a surface (CLI/TUI) can drive it with the same
//! sink. Every op still executes through [`Executor::dispatch`](flux_runtime::Executor) — there is no
//! new bypass surface.

use std::sync::Arc;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use flux_agent::AgentSink;
use flux_core::{Chunk, ContentBlock, Message, Result, Usage};
use flux_provider::{Provider, Request, ToolDef};
use flux_runtime::{Executor, ToolResult};
use flux_session::SessionStore;

use crate::ast::{SymbolName, Visibility};
use crate::runtime::{execute_call, BindSpec};
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

    /// Run one user turn, abortable via `cancel`. Preserves the session message-shape invariant on
    /// every termination path (normal stop, no tool calls, cancel, max-iterations).
    pub async fn run_turn_cancellable(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        self.store
            .append_message(session_id, &Message::user_text(user_input))?;

        // Skill activation: any skill whose triggers match this turn's input contributes its body to
        // the system prompt for this turn (and is recorded/surfaced as evidence).
        let mut system_prompt = self.system_prompt.clone();
        for skill in &self.skills {
            if skill.matches(user_input) {
                system_prompt.push_str(&format!(
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

        // Compact the session if it has grown past the budget (summarize old turns).
        self.maybe_compact(session_id, sink, cancel).await?;

        // Project flux-flow's session symbols into the system prompt: the model references values by
        // symbol; only the runtime dereferences them.
        let view = self.flow.view(session_id)?;
        if !view.symbols.is_empty() {
            system_prompt.push_str(&format!(
                "\n\n<session_symbols>\n{}</session_symbols>",
                view.render()
            ));
        }

        let tools: Vec<ToolDef> = self
            .executor
            .registry()
            .specs()
            .into_iter()
            .map(|s| ToolDef {
                name: s.name,
                description: s.description,
                input_schema: s.input_schema,
            })
            .collect();

        for _ in 0..self.max_iterations {
            if cancel.is_cancelled() {
                return self.finish_cancelled(sink, None);
            }
            let messages = self.store.load_messages(session_id)?;
            let req = Request {
                model: self.model.clone(),
                system: Some(system_prompt.clone()),
                messages,
                tools: tools.clone(),
                max_tokens: self.max_tokens,
                temperature: None,
                top_p: None,
                stop_sequences: Vec::new(),
                thinking: false,
                effort: None,
                metadata: serde_json::Map::new(),
            };

            let mut stream = self.provider.stream(req).await?;
            let mut blocks: Vec<ContentBlock> = Vec::new();
            let mut usage: Option<Usage> = None;
            let mut text_acc = String::new();
            let mut cancelled = false;
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => { cancelled = true; break; }
                    chunk = stream.next() => {
                        let Some(chunk) = chunk else { break };
                        match chunk? {
                            Chunk::TextDelta(t) => {
                                sink.text_delta(&t);
                                text_acc.push_str(&t);
                            }
                            Chunk::ThinkingDelta(t) => sink.thinking_delta(&t),
                            Chunk::Block(b) => blocks.push(b),
                            Chunk::Usage(u) => usage = Some(u),
                            Chunk::Done { .. } | Chunk::MessageStart { .. } => {}
                        }
                    }
                }
            }

            // Never persist an empty assistant message; recover streamed-but-uncompleted text.
            if blocks.is_empty() && !text_acc.trim().is_empty() {
                blocks.push(ContentBlock::Text {
                    text: std::mem::take(&mut text_acc),
                });
            }
            let assistant = Message::assistant(blocks);
            let has_content = !assistant.content.is_empty();
            if has_content {
                self.store.append_message(session_id, &assistant)?;
            }

            if cancelled {
                // Answer any unanswered tool_use blocks so the session stays valid, then end.
                if has_content {
                    let pending = collect_tool_uses(&assistant);
                    if !pending.is_empty() {
                        let results = pending
                            .into_iter()
                            .map(|(id, _, _)| {
                                ContentBlock::tool_result_text(id, "cancelled".to_string(), true)
                            })
                            .collect();
                        self.store
                            .append_message(session_id, &Message::user(results))?;
                    }
                }
                return self.finish_cancelled(sink, usage);
            }

            let tool_uses = if has_content {
                collect_tool_uses(&assistant)
            } else {
                Vec::new()
            };
            if tool_uses.is_empty() {
                sink.turn_end(usage);
                return Ok(());
            }

            // Execute each op through the envelope via the interpreter, which also stores the result
            // as an immutable value, binds a session symbol, and records the run-event trace.
            let mut results = Vec::new();
            let mut seen = self.executor.evidence().all().len();
            let mut cancelled_tools = false;
            for (id, name, input) in tool_uses {
                if cancelled_tools || cancel.is_cancelled() {
                    cancelled_tools = true;
                    results.push(ContentBlock::tool_result_text(
                        id,
                        "cancelled".to_string(),
                        true,
                    ));
                    continue;
                }
                sink.tool_call(&name, &input);
                let sym = SymbolName(name.replace('.', "_"));
                let outcome = execute_call(
                    &self.flow,
                    &self.executor,
                    session_id,
                    &name,
                    input,
                    Some(BindSpec {
                        name: &sym,
                        ty: None,
                        visibility: Visibility::Visible,
                    }),
                )
                .await?;
                let ev = self.executor.evidence();
                for o in &ev.all()[seen..] {
                    sink.observation(o);
                }
                seen = ev.all().len();
                let result = ToolResult {
                    content: outcome.content.clone(),
                    is_error: outcome.is_error,
                };
                sink.tool_result(&name, &result);
                results.push(ContentBlock::tool_result_text(
                    id,
                    outcome.content,
                    outcome.is_error,
                ));
            }
            self.store
                .append_message(session_id, &Message::user(results))?;
            if cancelled_tools {
                return self.finish_cancelled(sink, None);
            }
        }

        // Reached the iteration cap mid-tool-use: append a final assistant message so the next turn's
        // user input doesn't produce an invalid user-after-user sequence.
        let note = format!(
            "Reached the maximum of {} tool-use iterations for this turn; stopping.",
            self.max_iterations
        );
        sink.text_delta(&note);
        self.store.append_message(
            session_id,
            &Message::assistant(vec![ContentBlock::Text { text: note }]),
        )?;
        sink.turn_end(None);
        Ok(())
    }

    /// Record + surface a `turn.cancelled` observation and end the turn.
    fn finish_cancelled(&self, sink: &mut dyn AgentSink, usage: Option<Usage>) -> Result<()> {
        let obs = flux_evidence::Observation::new(
            "turn.cancelled",
            flux_evidence::Phase::Turn,
            serde_json::json!({}),
        );
        self.executor.observe(obs.clone());
        sink.observation(&obs);
        sink.turn_end(usage);
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

/// Extract `(id, name, input)` for every tool_use block in a message.
fn collect_tool_uses(msg: &Message) -> Vec<(String, String, serde_json::Value)> {
    msg.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => {
                Some((id.clone(), name.clone(), input.clone()))
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;

    use flux_provider::ChunkStream;
    use flux_runtime::{AllowApprover, PermissionManager, Tool, ToolContext, ToolRegistry};
    use flux_spec::ToolSpec;
    use flux_system::{System, Workspace};

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

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("echo", "echo text", json!({"type": "object"}))
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
    async fn engine_runs_op_tracks_state_and_keeps_session_shape() {
        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();

        // Turn 1: the model calls `echo`. Turn 2: it returns text and stops.
        let responses = VecDeque::from(vec![
            vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "echo".into(),
                    input: json!({"text": "renewal follow-up"}),
                }),
                Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::ToolUse),
                },
            ],
            vec![
                Chunk::TextDelta("done".into()),
                Chunk::Block(ContentBlock::Text {
                    text: "done".into(),
                }),
                Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::EndTurn),
                },
            ],
        ]);

        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine
            .run_turn(&sid, "send the renewal", &mut sink)
            .await
            .unwrap();

        // flux-flow tracked the op: value stored, symbol bound, view shows it, trace recorded.
        assert_eq!(sink.tools, vec!["echo"]);
        assert!(engine
            .flow
            .resolve(&sid, &SymbolName("echo".into()))
            .unwrap()
            .is_some());
        let fview = engine.flow.view(&sid).unwrap();
        assert!(fview.symbols.iter().any(|s| s.name.0 == "echo"));
        assert!(!engine.flow.events(&sid).unwrap().is_empty());

        // Session message shape: user, assistant(tool_use), user(tool_result), assistant(text).
        let msgs = store.load_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 4);
        assert!(msgs.iter().all(|m| !m.content.is_empty()));
        for w in msgs.windows(2) {
            assert!(
                !(w[0].role == flux_core::Role::User && w[1].role == flux_core::Role::User),
                "user-after-user is invalid"
            );
        }
    }

    #[tokio::test]
    async fn text_only_turn_ends_cleanly() {
        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let responses = VecDeque::from(vec![vec![
            Chunk::TextDelta("hello".into()),
            Chunk::Block(ContentBlock::Text {
                text: "hello".into(),
            }),
            Chunk::Done {
                stop_reason: Some(flux_core::StopReason::EndTurn),
            },
        ]]);
        let engine = engine_with(responses, store.clone());
        let mut sink = CollectSink::default();
        engine.run_turn(&sid, "hi", &mut sink).await.unwrap();

        assert!(sink.tools.is_empty());
        assert!(sink.text.contains("hello"));
        // user + assistant(text) = 2 messages, valid shape.
        let msgs = store.load_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs.iter().all(|m| !m.content.is_empty()));
    }
}
