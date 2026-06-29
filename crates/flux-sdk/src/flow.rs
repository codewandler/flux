//! The Flux-Lang **lifecycle surface** — the SDK front door for "the LLM plans, the runtime runs".
//!
//! Where [`crate::Client`] wraps the classic agent loop, [`FlowClient`] exposes the Flux-Lang
//! lifecycle directly: `compile` an instruction into a typed execution graph, `analyze` it against
//! the op catalog, and `execute` it through the real safety envelope. None of that machinery is
//! reimplemented here — every method delegates to `flux-flow` (the engine) and `flux-lang` (the
//! language), so the envelope, store, and analyzer are reused, not forked.
//!
//! The wiring that earns its keep is [`assemble_registry`]: it takes the pure built-ins
//! (`flux_tools::register_builtins`) **and** the provider-backed [`CognitionPack`] and registers
//! both into one [`ToolRegistry`]. That is what makes the model-op pack (`ai.extract`, `ai.rank`,
//! `ai.judge`, `ai.reason`, `synth`, `ai.rewrite`) reachable as named ops a plan can call — the
//! pack stops being a dead crate the moment a registry is assembled this way.
//!
//! To author the plan in Rust instead of compiling it from natural language, build a
//! [`flux_lang::ast::DraftAst`] with the [`crate::dsl`], then [`FlowClient::analyze`] and
//! [`FlowClient::execute`] it directly (skipping `compile`). Runnable, no-API-key examples for both
//! paths: `examples/flow_compile.rs` (NL→AST) and `examples/dsl_loops.rs` (Rust DSL).
//!
//! ```ignore
//! // Runnable hermetic version: `cargo run -p flux-sdk --example flow_compile`.
//! # async fn ex() -> flux_core::Result<()> {
//! use std::sync::Arc;
//! use flux_sdk::flow::FlowClient;
//!
//! // Any `Arc<dyn flux_provider::Provider>` (a real provider, or a hermetic mock in tests).
//! let provider: Arc<dyn flux_provider::Provider> = Arc::new(my_mock_provider());
//! let client = FlowClient::builder()
//!     .model("claude-sonnet-4-6")
//!     .allow("read")
//!     .build(provider, ".")?;
//!
//! // The cognition ops are advertised alongside the built-ins.
//! assert!(client.op_names().iter().any(|n| n == "ai.extract"));
//!
//! // compile → analyze → execute (or the `run` convenience that chains all three).
//! let out = client.run("read the README and summarize it").await?;
//! println!("{}", out.result);
//! # Ok(()) }
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use flux_cognition::CognitionPack;
use flux_core::{Error, Result};
use flux_flow::ast::SymbolName;
use flux_flow::compile::{compile as compile_flow, CompileOptions};
use flux_flow::registry::OpRegistry;
use flux_flow::runtime::{execute_flow, FlowOutcome};
use flux_flow::state::FlowStore;
use flux_flow::{tool_defs_from_registry, AgentSink, VoiceSessionDriver, VoiceSink};
use flux_lang::analyze::analyze_flow;
use flux_lang::prelude;
use flux_orchestrate::{SubAgents, TaskTool};
use flux_provider::{Provider, RealtimeConfig, RealtimeProvider};
use flux_runtime::{
    AllowApprover, Approver, DenyApprover, Executor, PermissionManager, Spawner, Tool, ToolContext,
    ToolRegistry,
};
use flux_system::{System, Workspace};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

// Re-export the lifecycle's public language types so a consumer can stay in `flux_sdk::flow`.
pub use flux_flow::analyze::Diagnostic;
pub use flux_flow::ast::DraftAst;
pub use flux_flow::state::SessionView;
// Artifact helpers: the typed v1-core prelude (builders/readers re-exported verbatim).
pub use flux_lang::prelude::{
    Answer, Blocked, Claim, Ctx, Evidence, Need, Patch, Query, Span, TestResult, Verdict,
};

/// Assemble a [`ToolRegistry`] = the pure built-ins (`flux_tools::register_builtins`) **plus** the
/// provider-backed [`CognitionPack`]. This single call is the wiring that makes the model-op pack
/// (`ai.extract`/`rank`/`judge`/`reason`, `synth`, `ai.rewrite`) reachable as named ops: without it
/// the pack is never installed and the planner can't call it.
pub fn assemble_registry(provider: Arc<dyn Provider>, model: impl Into<String>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut registry);
    CognitionPack::new(provider, model).register(&mut registry);
    registry
}

/// Builder for a [`FlowClient`]. Mirrors [`crate::ClientBuilder`]'s permission/approval knobs so the
/// two front doors feel the same; the model + workspace root are supplied at [`build`](Self::build).
pub struct FlowClientBuilder {
    model: String,
    allow: Vec<String>,
    deny: Vec<String>,
    auto_approve: bool,
    seed_prelude: bool,
    compile_opts: CompileOptions,
}

impl Default for FlowClientBuilder {
    fn default() -> Self {
        Self {
            model: "unknown".to_string(),
            // Reads pre-allowed; everything else denied unless `auto_approve` (no UI in a library).
            allow: vec!["read".to_string(), "glob".to_string(), "grep".to_string()],
            deny: Vec::new(),
            auto_approve: false,
            // Seed the planner catalog `$defs` with the v1-core artifact ontology by default.
            seed_prelude: true,
            compile_opts: CompileOptions::default(),
        }
    }
}

impl FlowClientBuilder {
    /// Set the model id the planner + every op call uses.
    pub fn model(mut self, m: impl Into<String>) -> Self {
        self.model = m.into();
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
    /// Approve every tool call automatically (no human in the loop). Use with care — model-backed
    /// cognition ops egress over the network, so they gate by default.
    pub fn auto_approve(mut self, yes: bool) -> Self {
        self.auto_approve = yes;
        self
    }
    /// Skip seeding the planner catalog `$defs` with the prelude artifact ontology (default: seed).
    pub fn without_prelude(mut self) -> Self {
        self.seed_prelude = false;
        self
    }
    /// Override the compile front-end's attempt/step/token budgets.
    pub fn compile_options(mut self, opts: CompileOptions) -> Self {
        self.compile_opts = opts;
        self
    }

    /// Build the client with `provider` and a workspace rooted at `root`. The registry is assembled
    /// via [`assemble_registry`] (built-ins + cognition pack); the flow store is in-memory.
    pub fn build(
        self,
        provider: Arc<dyn Provider>,
        root: impl Into<PathBuf>,
    ) -> Result<FlowClient> {
        let system = Arc::new(System::new(Workspace::new(root.into())?));
        let registry = assemble_registry(provider.clone(), self.model.clone());
        let store = FlowStore::in_memory()?;
        let prelude_defs = if self.seed_prelude {
            prelude::prelude_schema()
        } else {
            Value::Object(serde_json::Map::new())
        };
        Ok(FlowClient {
            provider,
            model: self.model,
            registry,
            system,
            store,
            allow: self.allow,
            deny: self.deny,
            auto_approve: self.auto_approve,
            compile_opts: self.compile_opts,
            prelude_defs,
            session_id: "flux-sdk".to_string(),
            spawner: None,
        })
    }
}

/// A Flux-Lang lifecycle façade: holds the provider + model + the assembled registry, and exposes
/// the `compile → analyze → execute` surface plus a registration surface for extra ops/packs.
pub struct FlowClient {
    provider: Arc<dyn Provider>,
    model: String,
    registry: ToolRegistry,
    system: Arc<System>,
    store: FlowStore,
    allow: Vec<String>,
    deny: Vec<String>,
    auto_approve: bool,
    compile_opts: CompileOptions,
    /// The merged `$defs` artifact map, seeded from `prelude_schema()` and extended by
    /// [`register_prelude`](Self::register_prelude); available for catalog enrichment / inspection.
    prelude_defs: Value,
    session_id: String,
    /// Optional sub-agent spawner (installed by [`with_sub_agents`](Self::with_sub_agents)): when set,
    /// `build_executor` threads it into the per-run `ToolContext` so a `task` call delegates to a role.
    spawner: Option<Arc<dyn Spawner>>,
}

impl FlowClient {
    /// Start building a [`FlowClient`].
    pub fn builder() -> FlowClientBuilder {
        FlowClientBuilder::default()
    }

    /// The resolved model id every op + planner call uses.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The assembled registry (built-ins + cognition pack + anything later registered).
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// The names of every registered op — handy to prove the cognition pack is wired (`ai.extract`,
    /// `synth`, …) alongside the built-ins (`read`, `grep`, …).
    pub fn op_names(&self) -> Vec<String> {
        self.registry.names()
    }

    /// The merged artifact `$defs` map (the planner catalog `$defs`), for inspection or merging into
    /// a downstream schema.
    pub fn prelude_defs(&self) -> &Value {
        &self.prelude_defs
    }

    // ----- registration surface -----

    /// Register a single extra op (any [`Tool`]) into the assembled registry.
    pub fn register_op(&mut self, tool: Arc<dyn Tool>) -> &mut Self {
        self.registry.register(tool);
        self
    }

    /// Register a *pack* — any `FnOnce(&mut ToolRegistry)`, e.g. another
    /// `CognitionPack::register`-style installer or `flux_tools::register_dev_builtins`.
    pub fn register_pack<F: FnOnce(&mut ToolRegistry)>(&mut self, pack: F) -> &mut Self {
        pack(&mut self.registry);
        self
    }

    /// Attach named sub-agents: register the `task` tool into this client's catalog and build the
    /// spawner from `sub_agents` over the client's guarded `System`. After this, a flow that calls
    /// `task(role, …)` delegates to a role's sub-agent through the same safety envelope. The single
    /// seam — a consumer (e.g. a multi-tenant service) drives sub-agents without re-assembling the
    /// spawner, executor, and context by hand.
    ///
    /// The one-shot `execute`/`run` path installs **no** cancellation token (`ToolContext::cancel` is
    /// `None`), so a sub-agent's only lifecycle bound here is its wall-clock deadline. To guarantee a
    /// hung child can't run forever, this applies a generous **default `wall_clock` (10 min)** when the
    /// bundle sets none; a consumer with longer-running work overrides it via [`SubAgents::with_limits`].
    pub fn with_sub_agents(&mut self, mut sub_agents: SubAgents) -> &mut Self {
        if sub_agents.limits.wall_clock.is_none() {
            sub_agents.limits.wall_clock = Some(std::time::Duration::from_secs(600));
        }
        self.registry.register(Arc::new(TaskTool));
        self.spawner = Some(sub_agents.into_spawner(self.system.clone()));
        self
    }

    /// Merge an artifact `$defs` map (e.g. [`flux_lang::prelude::prelude_schema`]) into the stashed
    /// planner catalog `$defs`. Existing keys are overwritten by `defs`.
    pub fn register_prelude(&mut self, defs: Value) -> &mut Self {
        if let (Some(into), Value::Object(from)) = (self.prelude_defs.as_object_mut(), defs) {
            for (k, v) in from {
                into.insert(k, v);
            }
        }
        self
    }

    // ----- the lifecycle: compile → analyze → execute -----

    /// Compile a natural-language `text` into a typed [`DraftAst`] via `flux-flow`'s NL→AST front-end
    /// (prompt-and-parse with a bounded repair loop). `view`, when present, lets the model reference
    /// existing session symbols instead of re-fetching.
    pub async fn compile(&self, text: &str, view: Option<&SessionView>) -> Result<DraftAst> {
        let ops = OpRegistry::new(&self.registry);
        let compiled = compile_flow(
            self.provider.as_ref(),
            &self.model,
            text,
            &ops,
            view,
            self.compile_opts.clone(),
        )
        .await?;
        Ok(compiled.ast)
    }

    /// Deterministic text → AST for a stored / already-validated flow — the non-NL partner of
    /// [`compile`](Self::compile), with **no** provider round-trip. Wraps `flux_lang`'s parser so a
    /// behaviour runner can re-hydrate a stored flow without a model call. Malformed input is a parse
    /// error folded into the SDK's error type (the parser is total — never a panic).
    pub fn parse(&self, text: &str) -> Result<DraftAst> {
        flux_lang::parse::parse(text).map_err(|e| Error::Other(e.to_string()))
    }

    /// Analyze an AST against the assembled registry's op catalog. `Ok(())` means every referenced op
    /// resolves; `Err` carries the [`Diagnostic`]s (e.g. unknown ops).
    pub fn analyze(&self, ast: &DraftAst) -> std::result::Result<(), Vec<Diagnostic>> {
        let ops = OpRegistry::new(&self.registry);
        analyze_flow(ast, &ops)
    }

    /// Execute a compiled [`DraftAst`] through the real safety envelope (`Executor::dispatch` under
    /// the client's permission rules + approver), returning an [`ExecutionResult`].
    pub async fn execute(&self, ast: &DraftAst) -> Result<ExecutionResult> {
        let executor = self.build_executor();
        let mut sink = ExecSink::default();
        // `execute_flow` returns `flux_flow::Result` (a `FlowError`); fold it into the SDK's
        // `flux_core::Error` so the surface speaks one error type.
        let outcome = execute_flow(&self.store, &executor, &self.session_id, ast, &mut sink)
            .await
            .map_err(|e| Error::Other(e.to_string()))?;
        finish_outcome(outcome, sink)
    }

    /// Execute `ast` with `inputs` seeded as flow variables (`$name`) **before** the run — the
    /// per-invocation value-injection seam: run a stored, validated flow with these settings without
    /// baking them into the AST as `lit` nodes (what a behaviour runner / preset framework needs).
    /// Same safety envelope as [`execute`](Self::execute) — ops still dispatch through
    /// `Executor::dispatch`; seeding injects *data*, never a capability.
    ///
    /// Each call runs against a **fresh store**, so repeated runs of the same stored AST with different
    /// inputs never leak symbols between them. A flow-local `bind` to a seeded name shadows the seed
    /// (ordinary lexical shadowing); a referenced-but-unseeded `$name` fails at runtime exactly like any
    /// unbound var; extra inputs the flow never references are ignored.
    pub async fn execute_with(
        &self,
        ast: &DraftAst,
        inputs: serde_json::Map<String, Value>,
    ) -> Result<ExecutionResult> {
        // A fresh per-run store is the isolation boundary: seeds (and anything the flow binds) live and
        // die with this call, so concurrent/successive runs of the same AST can't see each other's vars.
        let store = FlowStore::in_memory()?;
        for (name, value) in &inputs {
            store
                .seed(&self.session_id, &SymbolName(name.clone()), value)
                .map_err(|e| Error::Other(e.to_string()))?;
        }
        let executor = self.build_executor();
        let mut sink = ExecSink::default();
        let outcome = execute_flow(&store, &executor, &self.session_id, ast, &mut sink)
            .await
            .map_err(|e| Error::Other(e.to_string()))?;
        finish_outcome(outcome, sink)
    }

    /// Lower an AST to an optimizer [`PhysicalPlan`]: `analyze::lower` (validate + gather effects)
    /// then the scheduler, which batches independent read-only nodes into parallel stages and fences
    /// side-effects. `Err` carries the analysis [`Diagnostic`]s.
    pub fn optimize(
        &self,
        ast: &DraftAst,
    ) -> std::result::Result<flux_flow::ast::PhysicalPlan, Vec<Diagnostic>> {
        let ops = OpRegistry::new(&self.registry);
        let hir = flux_flow::analyze::lower(ast, &ops)?;
        Ok(flux_flow::optimize::optimize(&hir, &ops))
    }

    /// Execute an AST through the optimizer: `optimize` then run the resulting [`PhysicalPlan`] (so
    /// independent read-only nodes run concurrently). Equivalent results to [`Self::execute`], same
    /// envelope.
    pub async fn execute_optimized(&self, ast: &DraftAst) -> Result<ExecutionResult> {
        let plan = self
            .optimize(ast)
            .map_err(|d| Error::Other(format!("analyze: {}", join_diags(&d))))?;
        let executor = self.build_executor();
        let mut sink = ExecSink::default();
        let outcome = flux_flow::runtime::execute_plan(
            &self.store,
            &executor,
            &self.session_id,
            &ast.body,
            &plan,
            &mut sink,
        )
        .await
        .map_err(|e| Error::Other(e.to_string()))?;
        Ok(ExecutionResult {
            result: outcome.result,
            transcript: outcome.transcript,
            steps: outcome.steps,
            tool_calls: sink.tool_calls,
        })
    }

    /// The convenience pipeline: `compile` → `analyze` → `execute`. A failed analysis aborts before
    /// any side effect (the AST referenced an op the registry doesn't have).
    pub async fn run(&self, text: &str) -> Result<ExecutionResult> {
        let ast = self.compile(text, None).await?;
        if let Err(diags) = self.analyze(&ast) {
            return Err(Error::Other(format!("analyze: {}", join_diags(&diags))));
        }
        self.execute(&ast).await
    }

    /// The deterministic counterpart of [`run`](Self::run): `parse` → `analyze` → `execute_with`. Runs
    /// a **stored** flow per invocation with injected `inputs` and no model round-trip. A failed
    /// analysis aborts before any side effect (the AST referenced an op the registry doesn't have).
    pub async fn run_flow(
        &self,
        text: &str,
        inputs: serde_json::Map<String, Value>,
    ) -> Result<ExecutionResult> {
        let ast = self.parse(text)?;
        if let Err(diags) = self.analyze(&ast) {
            return Err(Error::Other(format!("analyze: {}", join_diags(&diags))));
        }
        self.execute_with(&ast, inputs).await
    }

    /// Build a fresh [`Executor`] over a clone of the assembled registry (the safety envelope every
    /// op dispatches through). The registry's `Arc<dyn Tool>` entries clone cheaply.
    /// Run a full-duplex **voice** session: connect `provider` (any [`RealtimeProvider`] — e.g.
    /// `flux_providers::realtime::openai_realtime`), declare this client's registered ops to the model
    /// **once** via [`tool_defs_from_registry`], and drive it through a [`VoiceSessionDriver`] so the
    /// model's tool calls run through the same `Executor` envelope as a text turn. `sink` receives
    /// audio / transcripts / tool events; `cancel` ends the session (e.g. a caller hangup).
    ///
    /// The single seam: a consumer (e.g. a telephony channel) drives a voice agent without assembling
    /// an `Executor`, the driver, or the model-facing tool declarations by hand — the same "don't
    /// re-implement the wiring" shape as [`with_sub_agents`](Self::with_sub_agents).
    pub async fn run_voice_session(
        &self,
        provider: &dyn RealtimeProvider,
        mut config: RealtimeConfig,
        sink: &mut dyn VoiceSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        config.tools = tool_defs_from_registry(&self.registry);
        let conn = provider.connect(config).await?;
        let executor = Arc::new(self.build_executor());
        VoiceSessionDriver::new(executor)
            .run(conn, sink, cancel)
            .await;
        Ok(())
    }

    fn build_executor(&self) -> Executor {
        let perms = PermissionManager::from_rules(&self.allow, &self.deny);
        let approver: Arc<dyn Approver> = if self.auto_approve {
            Arc::new(AllowApprover)
        } else {
            Arc::new(DenyApprover)
        };
        // Thread the sub-agent spawner into the per-run context when one is attached, so a `task` call
        // can delegate. `None` (the common case) leaves the context exactly as before.
        let mut ctx = ToolContext::new(self.system.clone());
        if let Some(spawner) = &self.spawner {
            ctx = ctx.with_spawner(spawner.clone());
        }
        Executor::new(self.registry.clone(), perms, approver, ctx)
    }
}

/// The outcome of [`FlowClient::execute`]: the rendered result, the model-facing transcript (every
/// node's view), the dispatched op count, and the op names invoked.
#[derive(Debug, Clone, Default)]
pub struct ExecutionResult {
    /// The flow's result rendered as text — the last node's view, or an explicit `return`'s value.
    pub result: String,
    /// Every read/call node's view, labeled and concatenated (what the engine feeds back to a model).
    pub transcript: String,
    /// How many operations were dispatched.
    pub steps: usize,
    /// The op names invoked, in dispatch order.
    pub tool_calls: Vec<String>,
}

impl ExecutionResult {
    /// Parse the result as a structured [`Answer`] (the `synth` op's shape), or `None` if it is not
    /// valid `Answer` JSON.
    pub fn answer(&self) -> Option<Answer> {
        self.parse()
    }

    /// Parse the result as any [`DeserializeOwned`] artifact type (e.g. [`Verdict`], [`TestResult`]),
    /// or `None` if it does not deserialize.
    pub fn parse<T: DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_str(&self.result).ok()
    }
}

/// A minimal [`AgentSink`] that records the op names dispatched during a flow run.
#[derive(Default)]
struct ExecSink {
    tool_calls: Vec<String>,
}

impl AgentSink for ExecSink {
    fn tool_call(&mut self, name: &str, _input: &Value) {
        self.tool_calls.push(name.to_string());
    }
}

fn join_diags(diags: &[Diagnostic]) -> String {
    diags
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Fold a finished `execute_flow` outcome into an [`ExecutionResult`], surfacing a top-level `await`
/// suspension as an error: the one-shot SDK path has no resume hook, so a half-run suspended flow (its
/// prefix's side effects fired, the remainder never will) is reported rather than silently returned —
/// cross-turn `await` flows belong on the engine. Shared by [`FlowClient::execute`] and
/// [`FlowClient::execute_with`] so the two can't drift.
fn finish_outcome(outcome: FlowOutcome, sink: ExecSink) -> Result<ExecutionResult> {
    if let Some(susp) = &outcome.suspension {
        return Err(Error::Other(format!(
            "flow suspended on a top-level `await` (source `{}`); the one-shot SDK `execute` path does \
             not support cross-turn resume — drive await flows through the engine instead",
            susp.source
        )));
    }
    Ok(ExecutionResult {
        result: outcome.result,
        transcript: outcome.transcript,
        steps: outcome.steps,
        tool_calls: sink.tool_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use flux_core::{Chunk, Result as CoreResult};
    use flux_provider::{ChunkStream, Request};
    use serde_json::json;
    use std::sync::Mutex;

    /// A hermetic provider that replays canned text, one `stream()` call at a time. Used to exercise
    /// the compile front-end (which prompt-and-parses) without any network.
    struct MockProvider {
        replies: Mutex<Vec<String>>,
    }

    impl MockProvider {
        fn new(replies: impl IntoIterator<Item = String>) -> Self {
            // Stored reversed so `stream()` can cheaply `pop()` replies in call order.
            let mut v: Vec<String> = replies.into_iter().collect();
            v.reverse();
            Self {
                replies: Mutex::new(v),
            }
        }
        fn one(reply: &str) -> Arc<dyn Provider> {
            Arc::new(Self::new([reply.to_string()]))
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> CoreResult<ChunkStream> {
            let reply = self.replies.lock().unwrap().pop().unwrap_or_default();
            let chunks = vec![
                Ok(Chunk::TextDelta(reply)),
                Ok(Chunk::Done { stop_reason: None }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flux-sdk-flow-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ----- voice session seam -----

    use flux_provider::{RealtimeConnection, RealtimeEvent, RealtimeEventStream, RealtimeSession};
    use flux_runtime::ToolResult;
    use futures::StreamExt;

    struct VoiceLog {
        tool_output: Option<String>,
    }

    struct MockRtSession {
        log: Arc<Mutex<VoiceLog>>,
    }

    #[async_trait]
    impl RealtimeSession for MockRtSession {
        async fn send_audio(&self, _f: &[u8]) -> CoreResult<()> {
            Ok(())
        }
        async fn commit_audio(&self) -> CoreResult<()> {
            Ok(())
        }
        async fn send_text(&self, _t: &str) -> CoreResult<()> {
            Ok(())
        }
        async fn create_response(&self) -> CoreResult<()> {
            Ok(())
        }
        async fn cancel_response(&self) -> CoreResult<()> {
            Ok(())
        }
        async fn send_tool_result(&self, _call_id: &str, output: &str) -> CoreResult<()> {
            self.log.lock().unwrap().tool_output = Some(output.to_string());
            Ok(())
        }
        fn close(&self) {}
    }

    /// A scripted realtime provider: the model "decides" to call the first declared tool.
    struct MockRealtime {
        log: Arc<Mutex<VoiceLog>>,
    }

    #[async_trait]
    impl RealtimeProvider for MockRealtime {
        fn name(&self) -> &str {
            "mock-realtime"
        }
        async fn connect(&self, config: RealtimeConfig) -> CoreResult<RealtimeConnection> {
            // The registry op is among the model-facing declarations (declared once) — call it.
            let name = config
                .tools
                .iter()
                .find(|t| t.name == "lookup")
                .map(|t| t.name.clone())
                .expect("registered op `lookup` is declared to the model");
            let events = vec![
                RealtimeEvent::ResponseStarted,
                RealtimeEvent::ToolCall {
                    call_id: "c1".into(),
                    name,
                    arguments: json!({"day": "fri"}).to_string(),
                },
                RealtimeEvent::ResponseDone,
            ];
            let head = futures::stream::iter(events.into_iter().map(Ok::<_, flux_core::Error>));
            let events: RealtimeEventStream =
                Box::pin(head.chain(futures::stream::pending::<CoreResult<RealtimeEvent>>()));
            Ok(RealtimeConnection {
                session: Arc::new(MockRtSession {
                    log: self.log.clone(),
                }),
                events,
            })
        }
    }

    struct LookupTool;

    #[async_trait]
    impl Tool for LookupTool {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only("lookup", "look up slots", json!({"type": "object"}))
        }
        async fn execute(&self, _ctx: &ToolContext, params: Value) -> CoreResult<ToolResult> {
            Ok(ToolResult::ok(format!(
                "free on {}",
                params["day"].as_str().unwrap_or("?")
            )))
        }
    }

    struct NoopSink;
    impl VoiceSink for NoopSink {}

    #[tokio::test]
    async fn run_voice_session_routes_a_tool_call_through_the_envelope() {
        let mut client = FlowClient::builder()
            .auto_approve(true)
            .build(MockProvider::one("noop"), temp_root("voice"))
            .unwrap();
        client.register_op(Arc::new(LookupTool));

        let log = Arc::new(Mutex::new(VoiceLog { tool_output: None }));
        let provider = MockRealtime { log: log.clone() };
        let config = RealtimeConfig::voice_agent("mock", "be a booking agent");
        let cancel = CancellationToken::new();
        let mut sink = NoopSink;

        // End the (otherwise open) session once the tool result is back.
        let controller = {
            let cancel = cancel.clone();
            let log = log.clone();
            async move {
                for _ in 0..400 {
                    if log.lock().unwrap().tool_output.is_some() {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                cancel.cancel();
            }
        };

        let (res, _) = tokio::join!(
            client.run_voice_session(&provider, config, &mut sink, &cancel),
            controller,
        );
        res.unwrap();

        // The model's tool call ran through `Executor::dispatch` (declared once from the registry)
        // and the op's output went back to the model.
        assert_eq!(
            log.lock().unwrap().tool_output.as_deref(),
            Some("free on fri")
        );
    }

    #[test]
    fn assembled_registry_wires_builtins_and_the_cognition_pack() {
        // The headline reachability proof: one registry carries BOTH the pure built-ins AND the
        // provider-backed model-op pack — so the pack is no longer a dead crate.
        let reg = assemble_registry(MockProvider::one("noop"), "test-model");
        let names = reg.names();
        // Built-ins.
        for builtin in ["read", "grep", "write", "bash"] {
            assert!(
                names.iter().any(|n| n == builtin),
                "missing builtin {builtin}"
            );
        }
        // Pure cognition ops (built-ins too, but worth pinning).
        for pure in ["need", "gaps"] {
            assert!(names.iter().any(|n| n == pure), "missing pure op {pure}");
        }
        // The model-backed cognition pack — the wiring this whole module exists for.
        for cog in [
            "ai.extract",
            "ai.rank",
            "ai.judge",
            "ai.reason",
            "synth",
            "ai.rewrite",
        ] {
            assert!(names.iter().any(|n| n == cog), "missing cognition op {cog}");
        }
    }

    #[test]
    fn flow_client_op_names_include_the_cognition_pack() {
        let client = FlowClient::builder()
            .model("test-model")
            .build(MockProvider::one("noop"), temp_root("names"))
            .unwrap();
        let names = client.op_names();
        assert!(names.iter().any(|n| n == "read"));
        assert!(names.iter().any(|n| n == "synth"));
        assert!(names.iter().any(|n| n == "ai.extract"));
    }

    #[tokio::test]
    async fn round_trip_analyze_then_execute_on_a_json_flow() {
        // A hand-authored DraftAst (no NL compile): read a file we control, then return it. Exercises
        // analyze (catalog resolution) + execute (real dispatch through the envelope).
        let root = temp_root("roundtrip");
        std::fs::write(root.join("note.txt"), "lifecycle surface works").unwrap();

        let client = FlowClient::builder()
            .model("test-model")
            .build(MockProvider::one("noop"), &root)
            .unwrap();

        let ast: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "bind", "name": "r",
                  "value": { "kind": "call", "op": "read",
                             "args": [ { "kind": "lit", "value": "note.txt" } ] } },
                { "kind": "return", "value": { "kind": "var", "name": "r" } }
            ]
        }))
        .unwrap();

        // analyze accepts it: every op (`read`) resolves in the assembled catalog.
        client
            .analyze(&ast)
            .expect("analyze should accept a read flow");

        let out = client.execute(&ast).await.unwrap();
        assert_eq!(out.steps, 1, "exactly one op dispatched");
        assert_eq!(out.tool_calls, vec!["read"]);
        assert!(
            out.result.contains("lifecycle surface works"),
            "result should carry the file content, got: {}",
            out.result
        );
    }

    // ----- D-01: parameterized flow execution (deterministic parse + per-run seeding) -----

    /// A read-only op that echoes the params it received — lets a test assert a *seeded* value reached
    /// the op through `Executor::dispatch`, whatever the arg→params shape.
    struct EchoArgsTool;
    #[async_trait]
    impl Tool for EchoArgsTool {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only(
                "echo_args",
                "echo args",
                json!({"type": "object", "properties": {"value": {"type": "string"}},
                       "required": ["value"]}),
            )
        }
        async fn execute(&self, _ctx: &ToolContext, params: Value) -> CoreResult<ToolResult> {
            Ok(ToolResult::ok(format!("args={params}")))
        }
    }

    /// A destructive op: if the envelope ever lets it run it shouts, so a test can prove it was gated.
    struct BoomTool;
    #[async_trait]
    impl Tool for BoomTool {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only("boom", "destructive", json!({"type": "object"}))
                .with_risk(flux_spec::Risk::Destructive)
        }
        async fn execute(&self, _ctx: &ToolContext, _params: Value) -> CoreResult<ToolResult> {
            Ok(ToolResult::ok("BOOM EXECUTED"))
        }
    }

    fn one_input(key: &str, value: Value) -> serde_json::Map<String, Value> {
        let mut m = serde_json::Map::new();
        m.insert(key.to_string(), value);
        m
    }

    #[tokio::test]
    async fn parse_is_deterministic_no_provider_call() {
        // A stored flow is already valid — `parse`/`analyze` must never touch the provider.
        let mock = Arc::new(MockProvider::new(["UNUSED".to_string()]));
        let client = FlowClient::builder()
            .model("mock")
            .build(mock.clone(), temp_root("parse"))
            .unwrap();
        let ast = client.parse("flow\n  return $greeting").unwrap();
        client.analyze(&ast).expect("a parsed flow analyzes clean");
        assert_eq!(
            mock.replies.lock().unwrap().len(),
            1,
            "parse + analyze must not call the provider (no reply was consumed)"
        );
    }

    #[tokio::test]
    async fn execute_with_seeds_a_var_no_literal() {
        let client = FlowClient::builder()
            .model("mock")
            .build(MockProvider::one("noop"), temp_root("seed"))
            .unwrap();
        let ast = client.parse("flow\n  return $greeting").unwrap();
        // Proof there is no baked-in value: the AST carries no `lit` node anywhere.
        let astr = serde_json::to_string(&ast).unwrap();
        assert!(
            !astr.contains("\"lit\""),
            "the flow must hold no literal: {astr}"
        );

        let out = client
            .execute_with(&ast, one_input("greeting", json!("hello from settings")))
            .await
            .unwrap();
        assert!(
            out.result.contains("hello from settings"),
            "the seeded value should surface as the result, got: {}",
            out.result
        );
    }

    #[tokio::test]
    async fn execute_with_isolates_runs() {
        let client = FlowClient::builder()
            .model("mock")
            .build(MockProvider::one("noop"), temp_root("isolate"))
            .unwrap();
        let ast = client.parse("flow\n  return $greeting").unwrap();

        let first = client
            .execute_with(&ast, one_input("greeting", json!("first")))
            .await
            .unwrap();
        assert!(first.result.contains("first"));

        // The same stored AST, run again with NO seed: a fresh per-run store means run 1's seed must
        // not leak in, so this references an unbound var and fails — proving per-run isolation.
        let leaked = client.execute_with(&ast, serde_json::Map::new()).await;
        assert!(
            leaked.is_err(),
            "run 1's seed must not leak into run 2 (got {leaked:?})"
        );
    }

    #[tokio::test]
    async fn a_flow_bind_shadows_a_seed() {
        // The flow re-binds `$greeting` from a file before returning it; the seed must lose.
        let root = temp_root("shadow");
        std::fs::write(root.join("note.txt"), "bound").unwrap();
        let client = FlowClient::builder()
            .model("mock")
            .build(MockProvider::one("noop"), &root)
            .unwrap();
        let ast: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "bind", "name": "greeting",
                  "value": { "kind": "call", "op": "read",
                             "args": [ { "kind": "lit", "value": "note.txt" } ] } },
                { "kind": "return", "value": { "kind": "var", "name": "greeting" } }
            ]
        }))
        .unwrap();
        let out = client
            .execute_with(&ast, one_input("greeting", json!("seeded")))
            .await
            .unwrap();
        assert!(
            out.result.contains("bound") && !out.result.contains("seeded"),
            "a flow-local bind shadows the seed, got: {}",
            out.result
        );
    }

    #[tokio::test]
    async fn custom_op_still_dispatches_through_the_envelope() {
        // A seeded value flows into a custom op via `Executor::dispatch`...
        let mut client = FlowClient::builder()
            .auto_approve(true)
            .build(MockProvider::one("noop"), temp_root("envelope"))
            .unwrap();
        client.register_op(Arc::new(EchoArgsTool));
        let call: DraftAst = serde_json::from_value(json!({
            "body": [ { "kind": "return", "value": {
                "kind": "call", "op": "echo_args",
                "args": [ { "kind": "var", "name": "greeting" } ] } } ]
        }))
        .unwrap();
        let out = client
            .execute_with(&call, one_input("greeting", json!("HELLO-SEED")))
            .await
            .unwrap();
        assert!(
            out.result.contains("HELLO-SEED"),
            "the seeded value should reach the op, got: {}",
            out.result
        );

        // ...and the envelope is not bypassed: a destructive op under the default DenyApprover is gated.
        let mut denied = FlowClient::builder()
            .build(MockProvider::one("noop"), temp_root("gated"))
            .unwrap();
        denied.register_op(Arc::new(BoomTool));
        let boom: DraftAst = serde_json::from_value(json!({
            "body": [ { "kind": "call", "op": "boom", "args": [] } ]
        }))
        .unwrap();
        let res = denied.execute_with(&boom, serde_json::Map::new()).await;
        let ran = res
            .map(|r| r.result.contains("BOOM EXECUTED"))
            .unwrap_or(false);
        assert!(
            !ran,
            "a destructive op must be gated by the default approver"
        );
    }

    #[tokio::test]
    async fn analyze_rejects_an_unknown_op() {
        let client = FlowClient::builder()
            .build(MockProvider::one("noop"), temp_root("unknown"))
            .unwrap();
        let ast: DraftAst = serde_json::from_value(json!({
            "body": [ { "kind": "call", "op": "no.such.op", "args": [] } ]
        }))
        .unwrap();
        let diags = client.analyze(&ast).unwrap_err();
        assert!(!diags.is_empty());
        assert!(diags[0].message.contains("no.such.op"));
    }

    #[tokio::test]
    async fn compile_then_execute_round_trips_through_the_mock_provider() {
        // The mock provider returns a fenced AST; compile parses it; execute runs it. Proves the
        // full `compile → execute` path is wired without a real model.
        let root = temp_root("compile");
        std::fs::write(root.join("doc.md"), "hello compile").unwrap();
        let ast_json = "```json\n{\"body\":[{\"kind\":\"call\",\"op\":\"read\",\"args\":[{\"kind\":\"lit\",\"value\":\"doc.md\"}]}]}\n```";

        let client = FlowClient::builder()
            .model("mock")
            .build(MockProvider::one(ast_json), &root)
            .unwrap();

        let ast = client.compile("read the doc", None).await.unwrap();
        client.analyze(&ast).expect("compiled flow analyzes clean");
        let out = client.execute(&ast).await.unwrap();
        assert_eq!(out.tool_calls, vec!["read"]);
        assert!(out.result.contains("hello compile"));
    }

    #[test]
    fn execution_result_parses_a_structured_answer() {
        // The artifact reader: a `synth`-shaped JSON result deserializes back into a typed `Answer`.
        let res = ExecutionResult {
            result: r#"{"status":"answered","summary":"done","evidence":[],"gaps":[],"risks":["minor"]}"#
                .to_string(),
            ..Default::default()
        };
        let answer = res.answer().expect("valid Answer JSON parses");
        assert_eq!(answer.status, "answered");
        assert_eq!(answer.summary, "done");
        assert_eq!(answer.risks, vec!["minor".to_string()]);
        // Non-Answer text yields None rather than panicking.
        let plain = ExecutionResult {
            result: "not json".to_string(),
            ..Default::default()
        };
        assert!(plain.answer().is_none());
    }

    #[test]
    fn register_op_and_pack_and_prelude_extend_the_surface() {
        let mut client = FlowClient::builder()
            .without_prelude()
            .build(MockProvider::one("noop"), temp_root("register"))
            .unwrap();
        // A pack installer (here: the pure cognition ops) folds in via register_pack.
        client.register_pack(flux_tools::register_dev_builtins);
        // Stash a prelude `$defs` map (was empty due to without_prelude).
        assert!(client.prelude_defs().as_object().unwrap().is_empty());
        client.register_prelude(prelude::prelude_schema());
        assert!(client.prelude_defs().get("Answer").is_some());
    }
}
