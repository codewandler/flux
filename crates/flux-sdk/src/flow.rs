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
//! ```ignore
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

use flux_agent::AgentSink;
use flux_cognition::CognitionPack;
use flux_core::{Error, Result};
use flux_flow::compile::{compile as compile_flow, CompileOptions};
use flux_flow::registry::OpRegistry;
use flux_flow::runtime::execute_flow;
use flux_flow::state::FlowStore;
use flux_lang::analyze::analyze_flow;
use flux_lang::prelude;
use flux_provider::Provider;
use flux_runtime::{
    AllowApprover, Approver, DenyApprover, Executor, PermissionManager, Tool, ToolContext,
    ToolRegistry,
};
use flux_system::{System, Workspace};
use serde::de::DeserializeOwned;
use serde_json::Value;

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
    pub fn model(mut self, m: impl Into<String>) -> Self {
        self.model = m.into();
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
}

impl FlowClient {
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
        // A top-level `await` suspends the flow mid-execution. This one-shot path has no resume hook,
        // so surface it as an error rather than silently returning a half-run flow (the prefix's side
        // effects fired, the remainder never will) — cross-turn `await` flows belong on the engine.
        if let Some(susp) = &outcome.suspension {
            return Err(Error::Other(format!(
                "flow suspended on a top-level `await` (source `{}`); the one-shot SDK `execute` path \
                 does not support cross-turn resume — drive await flows through the engine instead",
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

    /// Build a fresh [`Executor`] over a clone of the assembled registry (the safety envelope every
    /// op dispatches through). The registry's `Arc<dyn Tool>` entries clone cheaply.
    fn build_executor(&self) -> Executor {
        let perms = PermissionManager::from_rules(&self.allow, &self.deny);
        let approver: Arc<dyn Approver> = if self.auto_approve {
            Arc::new(AllowApprover)
        } else {
            Arc::new(DenyApprover)
        };
        Executor::new(
            self.registry.clone(),
            perms,
            approver,
            ToolContext::new(self.system.clone()),
        )
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
