//! `flux-sdk` — the high-level library API.
//!
//! Wraps the flux-flow engine, built-in tools, the safety envelope, and a session into a small
//! [`Client`]. You supply a [`Provider`] (from `flux-providers`) and a workspace
//! root; the SDK wires the rest.
//!
//! There are three front doors: [`Client`] (an agentic turn — the model plans, the runtime runs the
//! flux-lang agent loop — returning a [`TurnOutput`]), [`FlowClient`] (the Flux-Lang
//! `compile → analyze → execute` lifecycle, NL→AST), and the [`dsl`] (author the AST in Rust). Both
//! `Client` and `FlowClient` run on the same engine ([`flux_flow::engine::FlowEngine`]). Each door
//! has a runnable, no-API-key example: `examples/client_basic.rs`, `examples/flow_compile.rs`, and
//! `examples/dsl_loops.rs` respectively. On top of the DSL, [`recipes`] is a cookbook of reusable,
//! parameterized flow builders (routing, lookup, the loop family, resilience).
//!
//! ```ignore
//! // Runnable hermetic version: `cargo run -p flux-sdk --example client_basic`.
//! # async fn ex() -> flux_core::Result<()> {
//! use flux_sdk::Client;
//! let provider = Box::new(flux_providers::anthropic::anthropic_from_env()?);
//! let client = Client::builder().auto_approve(true).build(provider, ".")?;
//! let out = client.run("Summarize the README").await?;
//! println!("{}", out.text);
//! # Ok(()) }
//! ```
#![warn(missing_docs)]

pub mod flow;

pub use flow::{assemble_registry, ExecutionResult, FlowClient, FlowClientBuilder};

/// The Rust **embedded DSL** for authoring flows — builder primitives that compile to the Flux-Lang
/// AST. Build a [`flux_lang::ast::DraftAst`] with `dsl::Flow`/`dsl::Block` (loops and control-flow are
/// first-class), then drive it through [`FlowClient::analyze`] + [`FlowClient::execute`]. Re-exported
/// from `flux-lang` so consumers can stay inside `flux_sdk`. See `examples/dsl_loops.rs`.
pub use flux_lang::dsl;

pub mod recipes;

use std::path::PathBuf;
use std::sync::Arc;

use flux_agent::{AgentSpec, Permissions, DEFAULT_SYSTEM_PROMPT};
use flux_core::{Result, Usage};
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_flow::state::FlowStore;
use flux_flow::AgentSink;
use flux_provider::Provider;
use flux_runtime::{AllowApprover, Approver, DenyApprover, ToolContext, ToolRegistry, ToolResult};
use flux_system::{System, Workspace};

/// The result of one `Client::run` turn.
#[derive(Debug, Default, Clone)]
pub struct TurnOutput {
    /// The assistant's final text for the turn.
    pub text: String,
    /// The names of the tools invoked during the turn, in call order.
    pub tool_calls: Vec<String>,
    /// Token usage for the turn, if the provider reported it.
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
    /// Set the model id every turn uses.
    pub fn model(mut self, m: impl Into<String>) -> Self {
        self.model = m.into();
        self
    }
    /// Override the system prompt (defaults to the agent's built-in prompt).
    pub fn system_prompt(mut self, s: impl Into<String>) -> Self {
        self.system_prompt = Some(s.into());
        self
    }
    /// Cap the max output tokens per model call.
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
    /// Cap the agent loop's tool-calling iterations per turn.
    pub fn max_iterations(mut self, n: usize) -> Self {
        self.max_iterations = n;
        self
    }
    /// Add a permission allow rule (e.g. `"write"`, `"Bash(git:*)"`).
    pub fn allow(mut self, rule: impl Into<String>) -> Self {
        self.allow.push(rule.into());
        self
    }
    /// Add a permission deny rule (takes precedence over allow rules).
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
    /// The turn runs on [`FlowEngine`] (the model plans, the runtime runs the flux-lang agent loop).
    pub fn build(self, provider: Box<dyn Provider>, root: impl Into<PathBuf>) -> Result<Client> {
        let root = root.into();
        let system = Arc::new(System::new(Workspace::new(root.clone())?));
        let mut registry = ToolRegistry::new();
        flux_tools::register_builtins(&mut registry);
        let approver: Arc<dyn Approver> = if self.auto_approve {
            Arc::new(AllowApprover)
        } else {
            Arc::new(DenyApprover)
        };

        let events = Arc::new(EventStore::in_memory()?);
        let session_id = events.create_session(&self.model)?;
        let flow = FlowStore::in_memory()?;

        // The agent's definition; `assemble` selects the tool subset (all, here), applies the
        // permissions, registers the reflexive ops, and ties the engine⇄loop-host cycle.
        let spec = AgentSpec {
            model: self.model,
            system_prompt: self
                .system_prompt
                .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string()),
            permissions: Permissions {
                allow: self.allow,
                deny: self.deny,
            },
            max_tokens: self.max_tokens,
            max_iterations: self.max_iterations,
            cwd: root,
            ..AgentSpec::default()
        };
        let engine = spec.assemble(
            Arc::from(provider),
            registry,
            approver,
            ToolContext::new(system),
            events,
            flow,
        )?;
        Ok(Client { engine, session_id })
    }
}

/// A configured, session-bound agent (runs on [`FlowEngine`]).
pub struct Client {
    engine: FlowEngine,
    session_id: String,
}

impl Client {
    /// Start building a [`Client`].
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// The id of the in-memory session this client's turns are recorded against.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Run one turn to completion, collecting the final text and the tools invoked.
    pub async fn run(&self, input: &str) -> Result<TurnOutput> {
        let mut sink = Collector::default();
        self.engine
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
        // The model answers in prose (no `emit_plan`) → the flux-lang loop takes the `chat` path:
        // it returns that text as the turn's answer and runs no ops.
        let provider = Box::new(OneShotMock {
            chunks: Mutex::new(Some(vec![
                Chunk::TextDelta("hello from sdk".into()),
                Chunk::Block(ContentBlock::Text {
                    text: "hello from sdk".into(),
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
        // Token usage is not surfaced through the unified flux-lang loop (true for every FlowEngine
        // surface today); the `TurnOutput::usage` field stays for API compatibility.
        assert!(out.usage.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two-call mock: the planner emits a one-op plan (call 0), the engine runs it (which also calls
    /// the loop-machinery `observe`), then the model answers in prose (call 1). Proves the SDK drives
    /// the *full* flux-lang loop end-to-end — `plan`/`run_plan`/`observe` are all registered (the
    /// `register_agent_ops` path) and a real op dispatches and surfaces to the sink.
    struct PlanThenProseMock {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl Provider for PlanThenProseMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunks = if n == 0 {
                // A one-op plan with no `complete` ⇒ the engine runs it and loops back to plan again.
                let ast = serde_json::json!({
                    "body": [{
                        "kind": "call", "op": "write",
                        "args": [
                            { "kind": "lit", "value": "sdk-plan.txt" },
                            { "kind": "lit", "value": "from the sdk plan\n" }
                        ]
                    }]
                });
                vec![
                    Chunk::Block(ContentBlock::ToolUse {
                        id: "p1".into(),
                        name: "emit_plan".into(),
                        input: serde_json::json!({ "ast": ast }),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::ToolUse),
                    },
                ]
            } else {
                vec![
                    Chunk::Block(ContentBlock::Text {
                        text: "Wrote the file.".into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    #[tokio::test]
    async fn client_runs_a_plan_then_answers() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let provider = Box::new(PlanThenProseMock {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let client = Client::builder()
            .model("mock")
            .auto_approve(true) // no human in the loop: the plan's `write` is allowed
            .build(provider, &dir)
            .unwrap();
        let out = client.run("write a file").await.unwrap();
        assert_eq!(out.text, "Wrote the file.");
        // The real op surfaced to the sink; loop machinery (plan/run_plan/observe) is filtered out.
        assert_eq!(out.tool_calls, vec!["write"]);
        // The plan actually executed through the guarded envelope.
        assert!(dir.join("sdk-plan.txt").exists(), "the plan's write ran");
        std::fs::remove_dir_all(&dir).ok();
    }
}
