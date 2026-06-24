//! `flux-agent` — the agent loop.
//!
//! One turn: append the user message, then repeatedly call the provider with the full session +
//! tool specs, persist the assistant message, and — if it contains `tool_use` blocks — dispatch
//! each through the [`Executor`] (which enforces the safety envelope), append the tool results,
//! and loop. Stops when the model returns no tool calls (or `max_iterations` is hit). Streaming
//! and tool activity are reported through an [`AgentSink`].

use std::sync::Arc;

use futures::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use flux_core::{Chunk, ContentBlock, Message, Result, Usage};
use flux_provider::{Provider, Request, ToolDef};
use flux_runtime::{Executor, ToolResult};
use flux_session::SessionStore;

/// A concise default system prompt for the coding agent.
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are flux, a precise coding agent. Use the provided \
tools to inspect and modify the workspace and to run commands. Prefer the smallest change that \
satisfies the request. When the task is complete, reply with a short summary and stop.";

/// Receives streaming output and tool activity from a turn (the CLI/TUI implements this).
pub trait AgentSink: Send {
    fn text_delta(&mut self, _text: &str) {}
    fn thinking_delta(&mut self, _text: &str) {}
    fn tool_call(&mut self, _name: &str, _input: &Value) {}
    fn tool_result(&mut self, _name: &str, _result: &ToolResult) {}
    /// An audit observation made during dispatch (e.g. a destructive-command marker).
    fn observation(&mut self, _o: &flux_evidence::Observation) {}
    fn turn_end(&mut self, _usage: Option<Usage>) {}
}

/// The agent: a provider, a tool executor (safety envelope), and a session store.
pub struct Agent {
    pub provider: Box<dyn Provider>,
    pub executor: Executor,
    pub store: Arc<SessionStore>,
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub max_iterations: usize,
    /// Skills whose triggers, when matched against a turn's input, inject their body into that
    /// turn's system prompt (and record a `skill.activated` observation).
    pub skills: Vec<flux_skill::Skill>,
    /// When the persisted session exceeds this many (serialized) chars, older turns are summarized
    /// into one synthetic message before the next request. `0` disables compaction.
    pub compact_threshold_chars: usize,
}

impl Agent {
    /// Run one user turn to completion (through any number of tool round-trips), uninterruptible.
    pub async fn run_turn(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
    ) -> Result<()> {
        self.run_turn_cancellable(session_id, user_input, sink, &CancellationToken::new())
            .await
    }

    /// Run one user turn, abortable via `cancel`. On cancellation the partial assistant message is
    /// persisted, every outstanding `tool_use` is answered with a synthetic "cancelled" result (so
    /// the session stays valid for the next turn), a `turn.cancelled` observation is emitted, and the
    /// turn returns `Ok`.
    pub async fn run_turn_cancellable(
        &self,
        session_id: &str,
        user_input: &str,
        sink: &mut dyn AgentSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        self.store
            .append_message(session_id, &Message::user_text(user_input))?;

        // Skill activation: any skill whose triggers match this turn's input contributes its body
        // to the system prompt for this turn (and is recorded/surfaced as evidence).
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
                return self.finish_cancelled(session_id, sink, None);
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

            // On early cancellation the completed `blocks` may be empty even though text streamed;
            // recover the partial reply as a text block, and never persist an empty assistant
            // message (providers reject an empty content array → a 400 on the next request).
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
                return self.finish_cancelled(session_id, sink, usage);
            }

            // Collect tool calls from the assistant message (none if it had no content).
            let tool_uses = if has_content {
                collect_tool_uses(&assistant)
            } else {
                Vec::new()
            };

            if tool_uses.is_empty() {
                sink.turn_end(usage);
                return Ok(());
            }

            // Execute each tool through the safety envelope; collect tool_result blocks. New
            // evidence observations made during dispatch are surfaced to the sink as they appear.
            // A cancellation mid-loop answers the remaining tool_uses synthetically and ends the turn.
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
                let result = self.executor.dispatch(&name, input).await;
                let ev = self.executor.evidence();
                for o in &ev.all()[seen..] {
                    sink.observation(o);
                }
                seen = ev.all().len();
                sink.tool_result(&name, &result);
                results.push(ContentBlock::tool_result_text(
                    id,
                    result.content,
                    result.is_error,
                ));
            }
            self.store
                .append_message(session_id, &Message::user(results))?;
            if cancelled_tools {
                return self.finish_cancelled(session_id, sink, None);
            }
        }

        sink.turn_end(None);
        Ok(())
    }

    /// Record + surface a `turn.cancelled` observation and end the turn.
    fn finish_cancelled(
        &self,
        _session_id: &str,
        sink: &mut dyn AgentSink,
        usage: Option<Usage>,
    ) -> Result<()> {
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
    /// recent messages into a single synthetic message and rewrite the session log. Emits a
    /// `context.compacted` observation. A no-op when compaction is disabled or the session is small.
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

        // Keep the most recent messages; summarize everything older. Snap the boundary back so
        // `recent` never *starts* on a tool_result whose `tool_use` would be summarized away — that
        // would leave a dangling tool_result and the next request would 400.
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
                // Abandon compaction on cancel — don't rewrite the log from a partial summary.
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
fn collect_tool_uses(msg: &Message) -> Vec<(String, String, Value)> {
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
    use async_trait::async_trait;
    use flux_provider::ChunkStream;
    use flux_runtime::{DenyApprover, PermissionManager, ToolContext, ToolRegistry};
    use flux_system::{System, Workspace};
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::Mutex;

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

    #[derive(Default)]
    struct CollectSink {
        text: String,
        tools: Vec<String>,
    }
    impl AgentSink for CollectSink {
        fn text_delta(&mut self, t: &str) {
            self.text.push_str(t);
        }
        fn tool_call(&mut self, name: &str, _input: &Value) {
            self.tools.push(name.to_string());
        }
    }

    #[tokio::test]
    async fn loop_executes_a_tool_then_finishes() {
        let dir = std::env::temp_dir().join(format!("flux-agent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        let mut registry = ToolRegistry::new();
        flux_tools::register_builtins(&mut registry);
        // pre-allow the tools so the loop runs without prompting.
        let perms = PermissionManager::from_rules(
            &["write".into(), "read".into(), "bash".into(), "edit".into()],
            &[],
        );
        let executor = Executor::new(
            registry,
            perms,
            Arc::new(DenyApprover),
            ToolContext::new(system.clone()),
        );

        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();

        // Turn 1: model calls `write`. Turn 2: model returns text and stops.
        let responses = VecDeque::from(vec![
            vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "write".into(),
                    input: json!({"path": "hello.txt", "content": "hi from flux"}),
                }),
                Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::ToolUse),
                },
            ],
            vec![
                Chunk::TextDelta("Created hello.txt.".into()),
                Chunk::Block(ContentBlock::Text {
                    text: "Created hello.txt.".into(),
                }),
                Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::EndTurn),
                },
            ],
        ]);

        let agent = Agent {
            provider: Box::new(MockProvider {
                responses: Mutex::new(responses),
            }),
            executor,
            store: store.clone(),
            model: "mock".into(),
            system_prompt: "test".into(),
            max_tokens: 1024,
            max_iterations: 5,
            skills: Vec::new(),
            compact_threshold_chars: 0,
        };

        let mut sink = CollectSink::default();
        agent
            .run_turn(&sid, "create hello.txt", &mut sink)
            .await
            .unwrap();

        // The tool actually wrote the file through the guarded system.
        assert_eq!(system.read_file("hello.txt").await.unwrap(), "hi from flux");
        assert_eq!(sink.tools, vec!["write"]);
        assert!(sink.text.contains("Created hello.txt"));
        // Persisted: user, assistant(tool_use), user(tool_result), assistant(text) = 4 messages.
        assert_eq!(store.load_messages(&sid).unwrap().len(), 4);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn skill_activates_on_matching_trigger() {
        let dir = std::env::temp_dir().join(format!("flux-agent-skill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let executor = Executor::new(
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            ToolContext::new(system),
        );
        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();

        // One text-only turn.
        let responses = VecDeque::from(vec![vec![
            Chunk::TextDelta("ok".into()),
            Chunk::Block(ContentBlock::Text { text: "ok".into() }),
            Chunk::Done {
                stop_reason: Some(flux_core::StopReason::EndTurn),
            },
        ]]);

        let agent = Agent {
            provider: Box::new(MockProvider {
                responses: Mutex::new(responses),
            }),
            executor,
            store,
            model: "mock".into(),
            system_prompt: "base".into(),
            max_tokens: 256,
            max_iterations: 3,
            skills: vec![flux_skill::Skill {
                name: "deploy-runbook".into(),
                description: "deploy steps".into(),
                triggers: vec!["deploy".into()],
                body: "Run the canary first.".into(),
                source: None,
            }],
            compact_threshold_chars: 0,
        };

        let mut sink = CollectSink::default();
        agent
            .run_turn(&sid, "please deploy the service", &mut sink)
            .await
            .unwrap();
        // The matching skill was recorded as an observation.
        assert_eq!(
            agent.executor.evidence().by_kind("skill.activated").count(),
            1
        );

        // A non-matching turn does not re-activate it.
        agent.run_turn(&sid, "say hello", &mut sink).await.unwrap();
        assert_eq!(
            agent.executor.evidence().by_kind("skill.activated").count(),
            1
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn long_session_is_compacted_under_budget() {
        // Provider that always returns a fixed summary/text and never calls tools.
        struct TextProvider;
        #[async_trait]
        impl Provider for TextProvider {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, _r: Request) -> Result<ChunkStream> {
                let chunks = vec![
                    Chunk::TextDelta("SUMMARY".into()),
                    Chunk::Block(ContentBlock::Text {
                        text: "SUMMARY".into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(flux_core::StopReason::EndTurn),
                    },
                ];
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        let dir = std::env::temp_dir().join(format!("flux-agent-compact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let executor = Executor::new(
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            ToolContext::new(system),
        );
        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        // Seed a long history so the budget is exceeded.
        for i in 0..20 {
            store
                .append_message(
                    &sid,
                    &Message::user_text(format!("a fairly long message number {i} with padding")),
                )
                .unwrap();
        }
        let before = store.load_messages(&sid).unwrap().len();
        assert!(before >= 20);

        let agent = Agent {
            provider: Box::new(TextProvider),
            executor,
            store: store.clone(),
            model: "mock".into(),
            system_prompt: "base".into(),
            max_tokens: 64,
            max_iterations: 2,
            skills: Vec::new(),
            compact_threshold_chars: 200, // tiny → compaction fires
        };

        let mut sink = CollectSink::default();
        agent.run_turn(&sid, "continue", &mut sink).await.unwrap();

        // The session was compacted (summary + recent + this turn's messages ≪ the original 20+).
        let after = store.load_messages(&sid).unwrap().len();
        assert!(
            after < before,
            "expected compaction to shrink the log ({after} !< {before})"
        );
        assert_eq!(
            agent
                .executor
                .evidence()
                .by_kind("context.compacted")
                .count(),
            1
        );
        // The synthetic summary message is present.
        assert!(store
            .load_messages(&sid)
            .unwrap()
            .iter()
            .any(|m| m.text().contains("summary of earlier conversation")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn compaction_does_not_orphan_tool_results() {
        struct TextProvider;
        #[async_trait]
        impl Provider for TextProvider {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, _r: Request) -> Result<ChunkStream> {
                let chunks = vec![
                    Chunk::TextDelta("SUMMARY".into()),
                    Chunk::Block(ContentBlock::Text {
                        text: "SUMMARY".into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(flux_core::StopReason::EndTurn),
                    },
                ];
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        let dir = std::env::temp_dir().join(format!("flux-agent-orphan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let executor = Executor::new(
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            ToolContext::new(system),
        );
        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        // Padding to exceed the budget, then a tool_use/tool_result pair as the most recent turn —
        // the boundary that would orphan the tool_result if compaction split it.
        for i in 0..10 {
            store
                .append_message(
                    &sid,
                    &Message::user_text(format!("padding message number {i} ......")),
                )
                .unwrap();
        }
        store
            .append_message(
                &sid,
                &Message::assistant(vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "read".into(),
                    input: json!({}),
                }]),
            )
            .unwrap();
        store
            .append_message(
                &sid,
                &Message::user(vec![ContentBlock::tool_result_text(
                    "t1".to_string(),
                    "ok".to_string(),
                    false,
                )]),
            )
            .unwrap();

        let agent = Agent {
            provider: Box::new(TextProvider),
            executor,
            store: store.clone(),
            model: "mock".into(),
            system_prompt: "base".into(),
            max_tokens: 64,
            max_iterations: 2,
            skills: Vec::new(),
            compact_threshold_chars: 100, // tiny → compaction fires this turn
        };

        let mut sink = CollectSink::default();
        agent.run_turn(&sid, "next", &mut sink).await.unwrap();
        assert_eq!(
            agent
                .executor
                .evidence()
                .by_kind("context.compacted")
                .count(),
            1,
            "compaction should have fired"
        );

        // No tool_result may reference a tool_use that compaction summarized away.
        let msgs = store.load_messages(&sid).unwrap();
        let tool_use_ids: std::collections::HashSet<String> = msgs
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        for m in &msgs {
            for b in &m.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                    assert!(
                        tool_use_ids.contains(tool_use_id),
                        "orphaned tool_result {tool_use_id} after compaction"
                    );
                }
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn cancellation_aborts_an_in_flight_turn() {
        use std::time::Duration;

        // Emits one delta, then a stream that never completes (so only cancellation can end it).
        struct BlockingProvider;
        #[async_trait]
        impl Provider for BlockingProvider {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, _r: Request) -> Result<ChunkStream> {
                let s = futures::stream::once(async { Ok(Chunk::TextDelta("partial".into())) })
                    .chain(futures::stream::pending::<Result<Chunk>>());
                Ok(Box::pin(s))
            }
        }

        let dir = std::env::temp_dir().join(format!("flux-agent-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let executor = Executor::new(
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            ToolContext::new(system),
        );
        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let agent = Agent {
            provider: Box::new(BlockingProvider),
            executor,
            store,
            model: "mock".into(),
            system_prompt: "base".into(),
            max_tokens: 64,
            max_iterations: 3,
            skills: Vec::new(),
            compact_threshold_chars: 0,
        };

        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c2.cancel();
        });

        let mut sink = CollectSink::default();
        // Must return promptly after cancellation rather than hanging forever.
        tokio::time::timeout(
            Duration::from_secs(5),
            agent.run_turn_cancellable(&sid, "go", &mut sink, &cancel),
        )
        .await
        .expect("turn did not return after cancellation")
        .unwrap();

        assert!(sink.text.contains("partial"));
        assert_eq!(
            agent.executor.evidence().by_kind("turn.cancelled").count(),
            1
        );
        // R1: the streamed-but-uncompleted text is persisted as a non-empty assistant message —
        // no empty content array (which would 400 the next request).
        let msgs = agent.store.load_messages(&sid).unwrap();
        assert!(
            msgs.iter().all(|m| !m.content.is_empty()),
            "no empty message may be persisted"
        );
        assert!(
            msgs.last().unwrap().text().contains("partial"),
            "partial reply preserved"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn cancel_before_any_output_persists_no_empty_assistant() {
        use std::time::Duration;

        // Never yields a chunk; only cancellation can end the turn (no text, no blocks).
        struct PendProvider;
        #[async_trait]
        impl Provider for PendProvider {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, _r: Request) -> Result<ChunkStream> {
                Ok(Box::pin(futures::stream::pending::<Result<Chunk>>()))
            }
        }

        let dir = std::env::temp_dir().join(format!("flux-agent-cancel0-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let executor = Executor::new(
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            ToolContext::new(system),
        );
        let store = Arc::new(SessionStore::in_memory().unwrap());
        let sid = store.create_session("mock").unwrap();
        let agent = Agent {
            provider: Box::new(PendProvider),
            executor,
            store,
            model: "mock".into(),
            system_prompt: "base".into(),
            max_tokens: 64,
            max_iterations: 3,
            skills: Vec::new(),
            compact_threshold_chars: 0,
        };

        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            c2.cancel();
        });

        let mut sink = CollectSink::default();
        tokio::time::timeout(
            Duration::from_secs(5),
            agent.run_turn_cancellable(&sid, "go", &mut sink, &cancel),
        )
        .await
        .expect("turn did not return after cancellation")
        .unwrap();

        // Only the user message persisted — no empty assistant message.
        let msgs = agent.store.load_messages(&sid).unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "only the user message; no empty assistant persisted"
        );
        assert!(msgs.iter().all(|m| !m.content.is_empty()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
