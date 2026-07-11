//! `flux-sdk` — the high-level library API.
//!
//! Wraps the flux-flow engine, built-in tools, the safety envelope, and a session into a small
//! [`Client`]. You supply a [`Provider`] (from `flux-providers`) and a workspace
//! root; the SDK wires the rest.
//!
//! There are three front doors: [`Client`] (an agentic turn — the model plans, the runtime runs the
//! flux-lang agent loop — returning a [`TurnOutput`]), [`FlowClient`] (the Flux-Lang
//! `compile → analyze → execute` lifecycle, NL→AST), and the [`dsl`] (author the AST in Rust).
//! `Client` assembles [`flux_flow::engine::FlowEngine`]; `FlowClient` delegates directly to the same
//! `flux-flow` compiler, runtime adapter, store, and safety envelope for one-flow execution. Each
//! door has a runnable, no-API-key example: `examples/client_basic.rs`,
//! `examples/flow_compile.rs`, and `examples/dsl_loops.rs` respectively. On top of the DSL,
//! [`recipes`] is a cookbook of reusable, parameterized flow builders (routing, lookup, the loop
//! family, resilience).
//!
//! ```ignore
//! // Runnable hermetic version: `cargo run -p codewandler-flux-sdk --example client_basic`.
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
pub mod session;
pub mod storage;

pub use flow::{assemble_registry, ExecutionResult, FlowClient, FlowClientBuilder};
pub use session::Session;
pub use storage::Storage;

/// The OS-sandbox posture types, re-exported so a consumer can inject an explicit sandbox into a
/// builder via [`ClientBuilder::with_sandbox`]/[`flow::FlowClientBuilder::with_sandbox`] without
/// taking a direct `flux-system` dependency.
pub use flux_system::sandbox::{Sandbox, SandboxSettings};

/// The Rust **embedded DSL** for authoring flows — builder primitives that compile to the Flux-Lang
/// AST. Build a [`flux_lang::ast::DraftAst`] with `dsl::Flow`/`dsl::Block` (loops and control-flow are
/// first-class), then drive it through [`FlowClient::analyze`] + [`FlowClient::execute`]. Re-exported
/// from `flux-lang` so consumers can stay inside `flux_sdk`. See `examples/dsl_loops.rs`.
pub use flux_lang::dsl;

pub mod recipes;

use std::path::PathBuf;
use std::sync::Arc;

use flux_agent::{AgentSpec, Permissions, DEFAULT_SYSTEM_PROMPT};
use flux_core::ContextBlock;
use flux_core::{Result, Usage};
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_provider::Provider;
use flux_runtime::{AllowApprover, Approver, DenyApprover, ToolContext, ToolRegistry};
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
    context: Vec<ContextBlock>,
    sandbox: Option<Sandbox>,
    storage: Option<Storage>,
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
            context: Vec::new(),
            // Unset ⇒ resolve the posture from the environment at `build` (off ⇒ disabled).
            sandbox: None,
            // Unset ⇒ in-memory (ephemeral) stores, the pre-0.16 behavior.
            storage: None,
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
    /// Inject an explicit OS-sandbox [`Sandbox`] that the built client's guarded `System` enforces on
    /// every spawn. When left unset (the default), the posture is resolved from the environment at
    /// [`build`](Self::build) via `Sandbox::resolve(SandboxSettings::from_env())` — so a consumer that
    /// exports `FLUX_SANDBOX=require` gets confinement without calling this (off ⇒ disabled, safe).
    /// Pass one only to pin a posture independent of ambient env.
    pub fn with_sandbox(mut self, sandbox: Sandbox) -> Self {
        self.sandbox = Some(sandbox);
        self
    }
    /// Choose where sessions live ([`Storage::in_memory`] by default). [`Storage::dir`] makes the
    /// client's sessions — turn history, suspended flows, projections — survive the process, and
    /// is what makes [`Client::open_session`] useful across restarts.
    pub fn storage(mut self, storage: Storage) -> Self {
        self.storage = Some(storage);
        self
    }
    /// Inject a knowledge block into the agent's system prompt as a `<knowledge-base>` section (A-19):
    /// grounds the agent on a small KB inline, with no retrieval round-trip. Chainable.
    pub fn add_context(
        mut self,
        id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        self.context.push(ContextBlock::new(id, title, body));
        self
    }

    /// Build the client with `provider` and a workspace rooted at `root`. Sessions live in the
    /// configured [`Storage`] (in-memory unless set). The turn runs on [`FlowEngine`] (the model
    /// plans, the runtime runs the flux-lang agent loop).
    pub fn build(self, provider: Box<dyn Provider>, root: impl Into<PathBuf>) -> Result<Client> {
        let root = root.into();
        // Attach the OS-sandbox posture so a consumer's `FLUX_SANDBOX=require` is honored on this
        // client's spawns; a bare `System::new` defaults to `Sandbox::disabled()` (no confinement,
        // no `require` enforcement). Unset ⇒ resolve from env (off ⇒ disabled, safe default).
        let sandbox = self
            .sandbox
            .unwrap_or_else(|| Sandbox::resolve(SandboxSettings::from_env()));
        let system = Arc::new(System::new(Workspace::new(root.clone())?).with_sandbox(sandbox));
        let mut registry = ToolRegistry::new();
        flux_tools::register_builtins(&mut registry);
        let approver: Arc<dyn Approver> = if self.auto_approve {
            Arc::new(AllowApprover)
        } else {
            Arc::new(DenyApprover)
        };

        let (events, flow) = self.storage.unwrap_or_default().resolve()?;
        let model = self.model.clone();
        let session_id = events.create_session(&self.model)?;

        // The agent's definition; `assemble` selects the tool subset (all, here), applies the
        // permissions, registers the reflexive ops, and ties the engine⇄loop-host cycle. Skills
        // come from the default skill dirs (project `.flux/skills`/`.claude/skills` + the user
        // globals, L-02) — discovery is progressive (metadata now, bodies on activation), so this
        // costs a frontmatter head-read per skill, not the bodies.
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
            context: self.context,
            ..AgentSpec::default()
        }
        .with_default_skills();
        let engine = spec.assemble(
            Arc::from(provider),
            registry,
            approver,
            ToolContext::new(system),
            events,
            flow,
        )?;
        Ok(Client {
            engine: Arc::new(engine),
            model,
            session_id,
            turn_guard: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
}

/// A configured agent (runs on [`FlowEngine`]): the expensive, long-lived half of the SDK's
/// conversational door. Conversations are [`Session`] handles — a fresh default one is created at
/// build (so [`Client::run`] works out of the box), and [`Client::create_session`] /
/// [`Client::open_session`] / [`Client::latest_session`] manage the rest. With persistent
/// [`Storage`], sessions — and their suspended flows — survive the process.
pub struct Client {
    engine: Arc<FlowEngine>,
    model: String,
    session_id: String,
    // One engine runs one turn at a time (the planner loop is armed per turn); every Session
    // created by this client shares this guard so concurrent sends serialize instead of racing.
    turn_guard: Arc<tokio::sync::Mutex<()>>,
}

impl Client {
    /// Start building a [`Client`].
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// The id of the default session this client's [`run`](Self::run) turns are recorded against
    /// (created at build).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Run one turn on the default session, collecting the final text and the tools invoked.
    /// Equivalent to `client.default_session().send(input)`.
    pub async fn run(&self, input: &str) -> Result<TurnOutput> {
        self.session(self.session_id.clone()).send(input).await
    }

    /// The default session (created at build) as a [`Session`] handle.
    pub fn default_session(&self) -> Session {
        self.session(self.session_id.clone())
    }

    /// Create a fresh session and return its handle.
    pub fn create_session(&self) -> Result<Session> {
        let id = self.engine.events.create_session(&self.model)?;
        Ok(self.session(id))
    }

    /// Open an existing session by id — the resume seam. Errors if the id is unknown to this
    /// client's [`Storage`]. A session parked on a top-level `await` resumes on the next
    /// [`Session::send`].
    pub fn open_session(&self, id: &str) -> Result<Session> {
        self.engine.events.info(id)?;
        Ok(self.session(id.to_string()))
    }

    /// The most recently updated session in this client's [`Storage`], if any. Note the client's
    /// own default session exists from build time; to resume an earlier process's conversation,
    /// prefer persisting its id and calling [`open_session`](Self::open_session).
    pub fn latest_session(&self) -> Result<Option<Session>> {
        Ok(self
            .engine
            .events
            .latest_session()?
            .map(|id| self.session(id)))
    }

    /// The client's event store — the escape hatch for projections and integrations the typed
    /// surface doesn't cover yet.
    pub fn event_store(&self) -> Arc<EventStore> {
        self.engine.events.clone()
    }

    /// The assembled engine — the documented advanced escape hatch. Everything reachable from
    /// here still dispatches through the same authorization → approval → guarded-IO envelope.
    pub fn engine(&self) -> &Arc<FlowEngine> {
        &self.engine
    }

    fn session(&self, id: String) -> Session {
        Session {
            engine: self.engine.clone(),
            id,
            turn_guard: self.turn_guard.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use flux_core::{Chunk, ContentBlock, StopReason, Usage};
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
                Chunk::Usage(Usage {
                    input_tokens: 64,
                    output_tokens: 8,
                    cache_read_input_tokens: 16,
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
        // Token usage now rides back out through the unified flux-lang loop: the planner call's
        // `Usage` is accumulated by the loop host and handed to `turn_end` at turn completion.
        let usage = out
            .usage
            .expect("usage surfaced through the FlowEngine loop");
        assert_eq!(usage.input_tokens, 64);
        assert_eq!(usage.output_tokens, 8);
        assert_eq!(usage.cache_read_input_tokens, 16);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A mock that records every request's system prompt (segments + legacy `system`) so a test can
    /// assert what the engine actually sent to the model.
    struct SystemCaptureMock {
        systems: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl Provider for SystemCaptureMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let mut sys = String::new();
            for seg in &req.system_segments {
                sys.push_str(&seg.text);
                sys.push('\n');
            }
            if let Some(s) = &req.system {
                sys.push_str(s);
            }
            self.systems.lock().unwrap().push(sys);
            Ok(Box::pin(futures::stream::iter(
                vec![
                    Chunk::Block(ContentBlock::Text { text: "ok".into() }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
                .into_iter()
                .map(Ok),
            )))
        }
    }

    /// L-02: the SDK populates skills from `flux_skill::default_skill_dirs` (previously only the
    /// CLI did) — a project skill under `<root>/.flux/skills` whose trigger matches the turn's
    /// input must be injected into the system prompt.
    #[tokio::test]
    async fn sdk_populates_skills_from_default_dirs() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-skills-{}", std::process::id()));
        let skills = dir.join(".flux").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("greeting.md"),
            "---\nname: greeting\ndescription: how to greet\ntriggers: [zorblefrazz]\n---\nAlways greet with ahoy.",
        )
        .unwrap();

        let systems = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(SystemCaptureMock {
            systems: systems.clone(),
        });
        let client = Client::builder()
            .model("mock")
            .build(provider, &dir)
            .unwrap();
        client.run("please zorblefrazz me").await.unwrap();

        let sys = systems.lock().unwrap().join("\n---\n");
        assert!(
            sys.contains("<skill name=\"greeting\">") && sys.contains("Always greet with ahoy."),
            "the matching project skill must be injected into the system prompt; got:\n{sys}"
        );
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
                            { "kind": "lit", "value": { "path": "sdk-plan.txt", "content": "from the sdk plan\n" } }
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

    /// A reusable prose mock: every call answers with the same text (no `take()` — it survives
    /// multiple turns and multiple client builds).
    struct ProseMock {
        text: &'static str,
    }
    #[async_trait]
    impl Provider for ProseMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Ok(Box::pin(futures::stream::iter(
                vec![
                    Chunk::TextDelta(self.text.into()),
                    Chunk::Block(ContentBlock::Text {
                        text: self.text.into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
                .into_iter()
                .map(Ok),
            )))
        }
    }

    /// D-142: `Storage::dir` makes sessions durable — a second client over the same directory
    /// resumes the first client's session by id and reads its history.
    #[tokio::test]
    async fn storage_dir_persists_and_resumes_a_session() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-store-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let store_dir = dir.join("state");

        let client = Client::builder()
            .model("mock")
            .storage(Storage::dir(&store_dir))
            .build(Box::new(ProseMock { text: "first" }), &dir)
            .unwrap();
        let out = client.run("hello").await.unwrap();
        assert_eq!(out.text, "first");
        let id = client.session_id().to_string();
        drop(client);

        // A "new process": a fresh client over the same storage dir resumes the session.
        let client = Client::builder()
            .model("mock")
            .storage(Storage::dir(&store_dir))
            .build(Box::new(ProseMock { text: "second" }), &dir)
            .unwrap();
        let session = client.open_session(&id).unwrap();
        let history = session.history().unwrap();
        assert!(
            history.len() >= 2,
            "expected the prior turn's user+assistant messages, got {}",
            history.len()
        );
        let out = session.send("again").await.unwrap();
        assert_eq!(out.text, "second");
        assert!(session.history().unwrap().len() > history.len());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-142: opening an unknown session id errors instead of silently minting a new stream.
    #[tokio::test]
    async fn open_session_unknown_id_errors() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let client = Client::builder()
            .model("mock")
            .build(Box::new(ProseMock { text: "x" }), &dir)
            .unwrap();
        assert!(client.open_session("no-such-session").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A prose mock that records each provider call's (start, end) interval; the body sleeps so
    /// overlapping turns would produce overlapping intervals.
    struct SlowRecordingMock {
        calls: Arc<Mutex<Vec<(std::time::Instant, std::time::Instant)>>>,
    }
    #[async_trait]
    impl Provider for SlowRecordingMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let start = std::time::Instant::now();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.calls
                .lock()
                .unwrap()
                .push((start, std::time::Instant::now()));
            Ok(Box::pin(futures::stream::iter(
                vec![
                    Chunk::Block(ContentBlock::Text { text: "ok".into() }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
                .into_iter()
                .map(Ok),
            )))
        }
    }

    /// D-142: one engine runs one turn at a time — concurrent `send`s on two sessions of the same
    /// client serialize on the turn guard instead of interleaving provider calls.
    #[tokio::test]
    async fn concurrent_sends_serialize_on_the_turn_guard() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let client = Client::builder()
            .model("mock")
            .build(
                Box::new(SlowRecordingMock {
                    calls: calls.clone(),
                }),
                &dir,
            )
            .unwrap();
        let a = client.create_session().unwrap();
        let b = client.create_session().unwrap();
        let (ra, rb) = tokio::join!(a.send("one"), b.send("two"));
        ra.unwrap();
        rb.unwrap();

        let mut intervals = calls.lock().unwrap().clone();
        intervals.sort_by_key(|(s, _)| *s);
        assert_eq!(intervals.len(), 2);
        assert!(
            intervals[1].0 >= intervals[0].1,
            "provider calls overlapped: the turn guard failed to serialize the turns"
        );
        std::fs::remove_dir_all(&dir).ok();
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
