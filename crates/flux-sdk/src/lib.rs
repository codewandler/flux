//! `flux-sdk` — the high-level library API.
//!
//! Wraps the agent loop, built-in tools, the safety envelope, and a session into a small
//! [`Client`]. You supply a [`Provider`] (from `flux-anthropic`/`flux-openai`) and a workspace
//! root; the SDK wires the rest.
//!
//! ```ignore
//! # async fn ex() -> flux_core::Result<()> {
//! use flux_sdk::Client;
//! let provider = Box::new(flux_anthropic::anthropic_from_env()?);
//! let client = Client::builder().auto_approve(true).build(provider, ".")?;
//! let out = client.run("Summarize the README").await?;
//! println!("{}", out.text);
//! # Ok(()) }
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use flux_agent::{Agent, AgentSink, DEFAULT_SYSTEM_PROMPT};
use flux_core::{Result, Usage};
use flux_provider::Provider;
use flux_runtime::{
    AllowApprover, Approver, DenyApprover, Executor, PermissionManager, ToolContext, ToolRegistry,
    ToolResult,
};
use flux_session::SessionStore;
use flux_system::{System, Workspace};

/// The result of one `Client::run` turn.
#[derive(Debug, Default, Clone)]
pub struct TurnOutput {
    pub text: String,
    pub tool_calls: Vec<String>,
    pub usage: Option<Usage>,
}

/// Builder for a [`Client`].
pub struct ClientBuilder {
    model: String,
    system_prompt: Option<String>,
    max_tokens: u32,
    max_iterations: usize,
    allow: Vec<String>,
    deny: Vec<String>,
    auto_approve: bool,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            model: "unknown".to_string(),
            system_prompt: None,
            max_tokens: 4096,
            max_iterations: 25,
            // Reads pre-allowed; everything else denied unless `auto_approve` (no UI in a library).
            allow: vec!["read".to_string()],
            deny: Vec::new(),
            auto_approve: false,
        }
    }
}

impl ClientBuilder {
    pub fn model(mut self, m: impl Into<String>) -> Self {
        self.model = m.into();
        self
    }
    pub fn system_prompt(mut self, s: impl Into<String>) -> Self {
        self.system_prompt = Some(s.into());
        self
    }
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
    pub fn max_iterations(mut self, n: usize) -> Self {
        self.max_iterations = n;
        self
    }
    /// Add a permission allow rule (e.g. `"write"`, `"Bash(git:*)"`).
    pub fn allow(mut self, rule: impl Into<String>) -> Self {
        self.allow.push(rule.into());
        self
    }
    pub fn deny(mut self, rule: impl Into<String>) -> Self {
        self.deny.push(rule.into());
        self
    }
    /// Approve every tool call automatically (no human in the loop). Use with care.
    pub fn auto_approve(mut self, yes: bool) -> Self {
        self.auto_approve = yes;
        self
    }

    /// Build the client with `provider` and a workspace rooted at `root`. Sessions are in-memory.
    pub fn build(self, provider: Box<dyn Provider>, root: impl Into<PathBuf>) -> Result<Client> {
        let system = Arc::new(System::new(Workspace::new(root.into())?));
        let mut registry = ToolRegistry::new();
        flux_tools::register_builtins(&mut registry);
        let perms = PermissionManager::from_rules(&self.allow, &self.deny);
        let approver: Arc<dyn Approver> = if self.auto_approve {
            Arc::new(AllowApprover)
        } else {
            Arc::new(DenyApprover)
        };
        let executor = Executor::new(registry, perms, approver, ToolContext::new(system));

        let store = Arc::new(SessionStore::in_memory()?);
        let session_id = store.create_session(&self.model)?;

        let agent = Agent {
            provider,
            executor,
            store,
            model: self.model,
            system_prompt: self
                .system_prompt
                .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string()),
            max_tokens: self.max_tokens,
            max_iterations: self.max_iterations,
            skills: Vec::new(),
            compact_threshold_chars: 0,
            groups: Vec::new(),
            cwd: std::path::PathBuf::from("."),
        };
        Ok(Client { agent, session_id })
    }
}

/// A configured, session-bound agent.
pub struct Client {
    agent: Agent,
    session_id: String,
}

impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Run one turn to completion, collecting the final text and the tools invoked.
    pub async fn run(&self, input: &str) -> Result<TurnOutput> {
        let mut sink = Collector::default();
        self.agent
            .run_turn(&self.session_id, input, &mut sink)
            .await?;
        Ok(sink.0)
    }
}

#[derive(Default)]
struct Collector(TurnOutput);

impl AgentSink for Collector {
    fn text_delta(&mut self, t: &str) {
        self.0.text.push_str(t);
    }
    fn tool_call(&mut self, name: &str, _input: &serde_json::Value) {
        self.0.tool_calls.push(name.to_string());
    }
    fn tool_result(&mut self, _name: &str, _result: &ToolResult) {}
    fn turn_end(&mut self, usage: Option<Usage>) {
        self.0.usage = usage;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use flux_core::{Chunk, ContentBlock, StopReason};
    use flux_provider::{ChunkStream, Request};
    use std::sync::Mutex;

    struct OneShotMock {
        chunks: Mutex<Option<Vec<Chunk>>>,
    }
    #[async_trait]
    impl Provider for OneShotMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let chunks = self.chunks.lock().unwrap().take().unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    #[tokio::test]
    async fn client_runs_a_text_turn() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let provider = Box::new(OneShotMock {
            chunks: Mutex::new(Some(vec![
                Chunk::TextDelta("hello from sdk".into()),
                Chunk::Block(ContentBlock::Text {
                    text: "hello from sdk".into(),
                }),
                Chunk::Usage(Usage {
                    input_tokens: 5,
                    output_tokens: 3,
                    ..Default::default()
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ])),
        });
        let client = Client::builder()
            .model("mock")
            .build(provider, &dir)
            .unwrap();
        let out = client.run("hi").await.unwrap();
        assert_eq!(out.text, "hello from sdk");
        assert!(out.tool_calls.is_empty());
        assert_eq!(out.usage.unwrap().output_tokens, 3);
        std::fs::remove_dir_all(&dir).ok();
    }
}
