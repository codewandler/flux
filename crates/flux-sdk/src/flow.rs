//! The Flux-Lang **lifecycle surface** — the SDK front door for "the LLM plans, the runtime runs".
//!
//! Where [`crate::Client`] runs a conversational turn through the self-hosted Flux-Lang
//! [`FlowEngine`](flux_flow::engine::FlowEngine), [`FlowClient`] exposes one flow's lifecycle
//! directly: parse or `compile` an instruction into a typed execution graph, `analyze` it against
//! the op catalog, optionally `optimize` it, and `execute` it through the real safety envelope.
//! None of that machinery is reimplemented here — every method delegates to `flux-flow` (the
//! engine) and `flux-lang` (the language), so the envelope, store, and analyzer are reused, not
//! forked.
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
//! // Runnable hermetic version: `cargo run -p codewandler-flux-sdk --example flow_compile`.
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
use flux_core::{Error, Result, Usage};
use flux_flow::ast::SymbolName;
use flux_flow::compile::{compile as compile_flow, CompileOptions};
use flux_flow::registry::{analyze_composites, OpRegistry};
use flux_flow::runtime::{execute_flow, execute_flow_with_composites, FlowOutcome};
use flux_flow::state::FlowStore;
use flux_flow::{tool_defs_from_registry, AgentSink, VoiceSessionDriver, VoiceSink};
use flux_lang::analyze::analyze_flow;
use flux_lang::prelude;
use flux_lang::program::{CompositeOpDecl, Module};
use flux_orchestrate::{SubAgents, TaskTool};
use flux_provider::{Provider, RealtimeConfig, RealtimeProvider};
use flux_runtime::{
    AllowApprover, Approver, DenyApprover, Executor, PermissionManager, Spawner, Tool, ToolContext,
    ToolRegistry,
};
use flux_system::sandbox::Sandbox;
use flux_system::{System, Workspace};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::events::{ChannelSink, FlowStream, TeeSink};
use crate::session::Collector;

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

/// Builder for a [`FlowClient`]. Shares the envelope knobs (permission rules, approval policy,
/// sandbox posture) with [`crate::ClientBuilder`] via one internal type so the two front doors
/// cannot drift; the model + workspace root are supplied at [`build`](Self::build).
pub struct FlowClientBuilder {
    model: String,
    envelope: crate::envelope::Envelope,
    seed_prelude: bool,
    compile_opts: CompileOptions,
    storage: Option<crate::Storage>,
}

impl Default for FlowClientBuilder {
    fn default() -> Self {
        Self {
            model: "unknown".to_string(),
            // Reads pre-allowed; everything else denied unless `auto_approve` (no UI in a library).
            envelope: crate::envelope::Envelope::with_default_allow(&["read", "glob", "grep"]),
            // Seed the planner catalog `$defs` with the v1-core artifact ontology by default.
            seed_prelude: true,
            compile_opts: CompileOptions::default(),
            // Unset ⇒ an in-memory flow store, the pre-0.16 behavior.
            storage: None,
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
        self.envelope.allow.push(rule.into());
        self
    }
    /// Add a permission deny rule (takes precedence over allow rules).
    pub fn deny(mut self, rule: impl Into<String>) -> Self {
        self.envelope.deny.push(rule.into());
        self
    }
    /// Approve every tool call automatically (no human in the loop). Use with care — model-backed
    /// cognition ops egress over the network, so they gate by default.
    pub fn auto_approve(mut self, yes: bool) -> Self {
        self.envelope.auto_approve = yes;
        self
    }
    /// Inject a custom [`Approver`] the executor consults per op — a policy between the blanket
    /// allow of [`auto_approve`](Self::auto_approve) and the headless default deny (e.g. a
    /// risk-aware confirm gate). Overrides `auto_approve`. Mirrors flux-orchestrate's
    /// `LocalSpawner::with_approver`, so the flow path and the sub-agent path take the same policy.
    pub fn approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.envelope.approver = Some(approver);
        self
    }
    /// Skip seeding the planner catalog `$defs` with the prelude artifact ontology (default: seed).
    pub fn without_prelude(mut self) -> Self {
        self.seed_prelude = false;
        self
    }
    /// Inject an explicit OS-sandbox [`Sandbox`] that the built client's guarded `System` enforces on
    /// every spawn. When left unset (the default), the posture is resolved from the environment at
    /// [`build`](Self::build) via `Sandbox::resolve(SandboxSettings::from_env())` — so a consumer that
    /// exports `FLUX_SANDBOX=require` gets confinement without calling this (off ⇒ disabled, safe).
    /// Pass one only to pin a posture independent of ambient env.
    pub fn with_sandbox(mut self, sandbox: Sandbox) -> Self {
        self.envelope.sandbox = Some(sandbox);
        self
    }
    /// Override the compile front-end's attempt/step/token budgets.
    pub fn compile_options(mut self, opts: CompileOptions) -> Self {
        self.compile_opts = opts;
        self
    }
    /// Choose where flow state lives ([`crate::Storage::in_memory`] by default).
    /// [`crate::Storage::dir`] persists durable-construct state (`once`/`checkpoint`) across
    /// processes. Note the one-shot `execute_with` path still uses a fresh per-run store — that
    /// isolation is its contract.
    pub fn storage(mut self, storage: crate::Storage) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Build the client with `provider` and a workspace rooted at `root`. The registry is assembled
    /// via [`assemble_registry`] (built-ins + cognition pack); flow state lives in the configured
    /// [`crate::Storage`] (in-memory unless set).
    pub fn build(
        self,
        provider: Arc<dyn Provider>,
        root: impl Into<PathBuf>,
    ) -> Result<FlowClient> {
        // Attach the OS-sandbox posture so a consumer's `FLUX_SANDBOX=require` is honored on this
        // client's spawns; a bare `System::new` defaults to `Sandbox::disabled()` (no confinement,
        // no `require` enforcement). Unset ⇒ resolve from env (off ⇒ disabled, safe default).
        let sandbox = self.envelope.resolve_sandbox();
        let system = Arc::new(System::new(Workspace::new(root.into())?).with_sandbox(sandbox));
        let registry = assemble_registry(provider.clone(), self.model.clone());
        let store = Arc::new(self.storage.unwrap_or_default().into_flow_store()?);
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
            allow: self.envelope.allow,
            deny: self.envelope.deny,
            auto_approve: self.envelope.auto_approve,
            approver: self.envelope.approver,
            compile_opts: self.compile_opts,
            prelude_defs,
            session_id: "flux-sdk".to_string(),
            spawner: None,
            composites: Vec::new(),
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
    // `Arc` so a spawned `execute_streamed` can share the same store (`FlowStore` isn't `Clone`);
    // `&self.store` still deref-coerces to `&FlowStore` for every direct execute path.
    store: Arc<FlowStore>,
    allow: Vec<String>,
    deny: Vec<String>,
    auto_approve: bool,
    /// Custom per-op approval policy (see [`FlowClientBuilder::approver`]); overrides `auto_approve`.
    approver: Option<Arc<dyn Approver>>,
    compile_opts: CompileOptions,
    /// The merged `$defs` artifact map, seeded from `prelude_schema()` and extended by
    /// [`register_prelude`](Self::register_prelude); available for catalog enrichment / inspection.
    prelude_defs: Value,
    session_id: String,
    /// Optional sub-agent spawner (installed by [`with_sub_agents`](Self::with_sub_agents)): when set,
    /// `build_executor` threads it into the per-run `ToolContext` so a `task` call delegates to a role.
    spawner: Option<Arc<dyn Spawner>>,
    /// Module-local Flux-Lang composite ops installed on this client.
    composites: Vec<CompositeOpDecl>,
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
        OpRegistry::new(&self.registry)
            .with_composites(&self.composites)
            .op_names()
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

    /// Load an installed subprocess plugin (feature `plugins`) and register its operations as
    /// policy-gated tools in this client's catalog. Spawns the plugin over this client's guarded
    /// `System` with **manifest-scoped** host capabilities (nothing widened), so a flow that calls a
    /// plugin op (e.g. `gitlab.mr_list`) dispatches through the same authorization → approval →
    /// guarded-IO envelope as a built-in. The registered tools hold the subprocess connection alive.
    #[cfg(feature = "plugins")]
    pub async fn register_plugin(
        &mut self,
        name: &str,
        descriptor: &flux_plugin::PluginDescriptor,
    ) -> Result<&mut Self> {
        let tools = crate::plugins::load_tools(&self.system, name, descriptor).await?;
        for tool in tools {
            self.registry.register(tool);
        }
        Ok(self)
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

    /// Register module-local composite ops so stored flows can call them like ordinary ops.
    pub fn register_composites(&mut self, composites: Vec<CompositeOpDecl>) -> &mut Self {
        self.composites.extend(composites);
        self
    }

    // ----- the lifecycle: compile → analyze → execute -----

    /// Compile a natural-language `text` into a typed [`DraftAst`] via `flux-flow`'s NL→AST front-end
    /// (prompt-and-parse with a bounded repair loop). `view`, when present, lets the model reference
    /// existing session symbols instead of re-fetching.
    pub async fn compile(&self, text: &str, view: Option<&SessionView>) -> Result<DraftAst> {
        let ops = OpRegistry::new(&self.registry).with_composites(&self.composites);
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

    /// Deterministic native module parser. Composite ops live on [`Module::Program::ops`] and can be
    /// installed with [`register_composites`](Self::register_composites).
    pub fn parse_module(&self, text: &str) -> Result<Module> {
        Module::parse_str(text).map_err(|e| Error::Other(e.to_string()))
    }

    /// Analyze an AST against the assembled registry's op catalog. `Ok(())` means every referenced op
    /// resolves and every `$var` is defined; `Err` carries the [`Diagnostic`]s (e.g. unknown ops,
    /// unbound symbols). A symbol supplied at run time via [`execute_with`](Self::execute_with)
    /// seeding must be declared as a flow param (`flow(name: Type)`) to analyze clean — params count
    /// as bound; undeclared seed-only names are reported unbound (L-15). To analyze a flow *as it
    /// will actually run* under `execute_with` seeding, without a flow-param declaration, use
    /// [`analyze_seeded`](Self::analyze_seeded) instead.
    pub fn analyze(&self, ast: &DraftAst) -> std::result::Result<(), Vec<Diagnostic>> {
        analyze_composites(&self.composites, &self.registry)?;
        let ops = OpRegistry::new(&self.registry).with_composites(&self.composites);
        analyze_flow(ast, &ops, &std::collections::HashSet::new())
    }

    /// The seeded counterpart of [`analyze`](Self::analyze): `seed_names` are passed through as the
    /// prebound set, so a name the caller intends to inject via
    /// [`execute_with`](Self::execute_with) counts as bound without requiring a flow-param
    /// declaration or a hand-prepended `Bind` node. An empty `seed_names` behaves exactly like
    /// `analyze`; a seeded name the flow never references is harmless.
    pub fn analyze_seeded(
        &self,
        ast: &DraftAst,
        seed_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> std::result::Result<(), Vec<Diagnostic>> {
        analyze_composites(&self.composites, &self.registry)?;
        let ops = OpRegistry::new(&self.registry).with_composites(&self.composites);
        let prebound: std::collections::HashSet<String> =
            seed_names.into_iter().map(Into::into).collect();
        analyze_flow(ast, &ops, &prebound)
    }

    /// Execute a compiled [`DraftAst`] through the real safety envelope (`Executor::dispatch` under
    /// the client's permission rules + approver), returning an [`ExecutionResult`].
    pub async fn execute(&self, ast: &DraftAst) -> Result<ExecutionResult> {
        let executor = self.build_executor();
        let mut sink = ExecSink::default();
        // `execute_flow` returns `flux_flow::Result` (a `FlowError`); fold it into the SDK's
        // `flux_core::Error` so the surface speaks one error type.
        let outcome = if self.composites.is_empty() {
            execute_flow(&self.store, &executor, &self.session_id, ast, &mut sink).await
        } else {
            execute_flow_with_composites(
                &self.store,
                &executor,
                &self.session_id,
                ast,
                &self.composites,
                &mut sink,
            )
            .await
        }
        .map_err(|e| Error::Other(e.to_string()))?;
        finish_outcome(outcome, sink, cognition_usage(&executor))
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
    ///
    /// Its analysis partner is [`analyze_seeded`](Self::analyze_seeded) — pass the same input names
    /// so the flow analyzes clean without a flow-param declaration (plain [`analyze`](Self::analyze)
    /// reports a seed-only, undeclared `$name` as unbound).
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
        let outcome = if self.composites.is_empty() {
            execute_flow(&store, &executor, &self.session_id, ast, &mut sink).await
        } else {
            execute_flow_with_composites(
                &store,
                &executor,
                &self.session_id,
                ast,
                &self.composites,
                &mut sink,
            )
            .await
        }
        .map_err(|e| Error::Other(e.to_string()))?;
        finish_outcome(outcome, sink, cognition_usage(&executor))
    }

    /// Execute `ast` while **streaming** every dispatch to your own [`AgentSink`] as it happens — each
    /// op's `tool_call` **and** `tool_result`, text, and observations — and still returning the
    /// collected [`ExecutionResult`]. The observable counterpart of [`execute`](Self::execute), whose
    /// private collector drops everything but op names. Same envelope, same one-shot `await` handling.
    pub async fn execute_with_sink(
        &self,
        ast: &DraftAst,
        sink: &mut dyn AgentSink,
    ) -> Result<ExecutionResult> {
        let executor = self.build_executor();
        let mut tee = TeeSink {
            consumer: sink,
            collect: Collector::default(),
        };
        let outcome = if self.composites.is_empty() {
            execute_flow(&self.store, &executor, &self.session_id, ast, &mut tee).await
        } else {
            execute_flow_with_composites(
                &self.store,
                &executor,
                &self.session_id,
                ast,
                &self.composites,
                &mut tee,
            )
            .await
        }
        .map_err(|e| Error::Other(e.to_string()))?;
        // The tee's collector holds the op names (as `TurnOutput.tool_calls`) for the result.
        let names = std::mem::take(&mut tee.collect.0.tool_calls);
        finish_outcome(
            outcome,
            ExecSink { tool_calls: names },
            cognition_usage(&executor),
        )
    }

    /// Execute `ast` as a [`FlowStream`] — a live stream of owned [`AgentEvent`](crate::AgentEvent)s
    /// plus `finish() -> ExecutionResult`. The flow runs on a spawned task, so events arrive as they
    /// happen whether or not you are polling (unlike the fully-buffered [`execute`](Self::execute)).
    ///
    /// # Panics
    /// Spawns the flow eagerly, so it must be called from within a Tokio runtime.
    pub fn execute_streamed(&self, ast: &DraftAst) -> FlowStream {
        let store = self.store.clone();
        let executor = self.build_executor();
        let session_id = self.session_id.clone();
        let ast = ast.clone();
        let composites = self.composites.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            let mut sink = ChannelSink {
                tx,
                collect: Collector::default(),
            };
            let outcome = if composites.is_empty() {
                execute_flow(&store, &executor, &session_id, &ast, &mut sink).await
            } else {
                execute_flow_with_composites(
                    &store,
                    &executor,
                    &session_id,
                    &ast,
                    &composites,
                    &mut sink,
                )
                .await
            }
            .map_err(|e| Error::Other(e.to_string()))?;
            let names = std::mem::take(&mut sink.collect.0.tool_calls);
            finish_outcome(
                outcome,
                ExecSink { tool_calls: names },
                cognition_usage(&executor),
            )
        });
        FlowStream {
            rx,
            handle: Some(handle),
        }
    }

    /// Lower an AST to an optimizer [`PhysicalPlan`]: `analyze::lower` (validate + gather effects)
    /// then the scheduler, which batches independent read-only nodes into parallel stages and fences
    /// side-effects. `Err` carries the analysis [`Diagnostic`]s.
    pub fn optimize(
        &self,
        ast: &DraftAst,
    ) -> std::result::Result<flux_flow::ast::PhysicalPlan, Vec<Diagnostic>> {
        let ops = OpRegistry::new(&self.registry).with_composites(&self.composites);
        let hir = flux_flow::analyze::lower(ast, &ops, &std::collections::HashSet::new())?;
        Ok(flux_flow::optimize::optimize(&hir, &ops))
    }

    /// The seeded counterpart of [`optimize`](Self::optimize): `seed_names` are passed through to
    /// `analyze::lower` as the prebound set, mirroring [`analyze_seeded`](Self::analyze_seeded) for
    /// the optimizer path.
    pub fn optimize_seeded(
        &self,
        ast: &DraftAst,
        seed_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> std::result::Result<flux_flow::ast::PhysicalPlan, Vec<Diagnostic>> {
        let ops = OpRegistry::new(&self.registry).with_composites(&self.composites);
        let prebound: std::collections::HashSet<String> =
            seed_names.into_iter().map(Into::into).collect();
        let hir = flux_flow::analyze::lower(ast, &ops, &prebound)?;
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
        let outcome = if self.composites.is_empty() {
            flux_flow::runtime::execute_plan(
                &self.store,
                &executor,
                &self.session_id,
                &ast.body,
                &plan,
                &mut sink,
            )
            .await
        } else {
            flux_flow::runtime::execute_plan_with_composites(
                &self.store,
                &executor,
                &self.session_id,
                &ast.body,
                &plan,
                &self.composites,
                &mut sink,
            )
            .await
        }
        .map_err(|e| Error::Other(e.to_string()))?;
        Ok(ExecutionResult {
            result: outcome.result,
            transcript: outcome.transcript,
            steps: outcome.steps,
            tool_calls: sink.tool_calls,
            usage: cognition_usage(&executor),
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
    ///
    /// This is the **model-driven** mode: the model leads the conversation and calls the declared
    /// tools. For the **flow-driven** counterpart — an authored flow leads and the model is pure
    /// speech I/O — use [`Session::run_voice_flow`](crate::Session::run_voice_flow) (it needs the
    /// session's persistent engine, which `FlowClient` doesn't own).
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
        let approver: Arc<dyn Approver> = match &self.approver {
            Some(custom) => custom.clone(),
            None if self.auto_approve => Arc::new(AllowApprover),
            None => Arc::new(DenyApprover),
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
/// node's view), the dispatched op count, the op names invoked, and the token spend of any model
/// calls inside the flow.
///
/// `#[non_exhaustive]`: fields are added as the SDK grows (wave 2 added `usage`), so construct it
/// only via the SDK and match with a `..` rest pattern.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ExecutionResult {
    /// The flow's result rendered as text — the last node's view, or an explicit `return`'s value.
    pub result: String,
    /// Every read/call node's view, labeled and concatenated (what the engine feeds back to a model).
    pub transcript: String,
    /// How many operations were dispatched.
    pub steps: usize,
    /// The op names invoked, in dispatch order.
    pub tool_calls: Vec<String>,
    /// Summed token usage of the model-backed cognition ops (`ai.extract`/`rank`/`judge`/`reason`,
    /// `synth`, `ai.rewrite`) dispatched during the run — `None` if the flow made no model call, or
    /// the provider reported no usage (e.g. the `mock` provider). Each cognition op is an
    /// **independent** single-shot completion with its own prompt, so — unlike the agent loop's
    /// re-sent conversation (`Usage::accumulate`, input last-wins) — every field is **summed** here:
    /// two `ai.extract` calls billing 100 input each report 200 input.
    pub usage: Option<Usage>,
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
/// suspension as an error: the one-shot `FlowClient` path has no resume hook, so a half-run suspended
/// flow (its prefix's side effects fired, the remainder never will) is reported rather than silently
/// returned — durable cross-turn `await` flows belong on the resumable session door. Shared by
/// [`FlowClient::execute`] and [`FlowClient::execute_with`] so the two can't drift. `usage` is the
/// run's summed cognition spend (see [`cognition_usage`]).
fn finish_outcome(
    outcome: FlowOutcome,
    sink: ExecSink,
    usage: Option<Usage>,
) -> Result<ExecutionResult> {
    if let Some(susp) = &outcome.suspension {
        return Err(Error::Other(format!(
            "flow suspended on a top-level `await` (source `{}`); the one-shot `FlowClient::execute` \
             path does not resume across turns — drive durable await flows through \
             `Session::start_flow` instead",
            susp.source
        )));
    }
    Ok(ExecutionResult {
        result: outcome.result,
        transcript: outcome.transcript,
        steps: outcome.steps,
        tool_calls: sink.tool_calls,
        usage,
    })
}

/// Sum the token spend of the cognition ops dispatched during a run. Each `CognitionOp` records a
/// `cognition.usage` observation on the shared evidence log (D-150); this reads them back off the
/// executor's log and **sums every field** — cognition calls are independent single-shot completions
/// (distinct prompts), not a re-sent conversation, so `Usage::accumulate`'s input-last-wins would
/// undercount. `None` when no cognition op billed anything (a pure-ops flow, or a free provider).
fn cognition_usage(executor: &Executor) -> Option<Usage> {
    let mut total = Usage::default();
    let mut any = false;
    for obs in executor.evidence().by_kind("cognition.usage") {
        if let Some(u) = obs
            .data
            .get("usage")
            .and_then(|v| serde_json::from_value::<Usage>(v.clone()).ok())
        {
            total.input_tokens += u.input_tokens;
            total.output_tokens += u.output_tokens;
            total.cache_creation_input_tokens += u.cache_creation_input_tokens;
            total.cache_read_input_tokens += u.cache_read_input_tokens;
            total.reasoning_tokens += u.reasoning_tokens;
            total.audio_input_tokens += u.audio_input_tokens;
            total.audio_output_tokens += u.audio_output_tokens;
            if let Some(c) = u.reported_cost_usd {
                *total.reported_cost_usd.get_or_insert(0.0) += c;
            }
            any = true;
        }
    }
    any.then_some(total)
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
                RealtimeEvent::ResponseDone { usage: None },
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
        let ast = client
            .parse("flow(greeting: String)\n  return $greeting")
            .unwrap();
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

    /// D-67: a seeded object must marshal into an op **exactly** like a literal-bound one. The
    /// interpreter canonicalizes every `lit` bind to the JSON-as-string `Value::String` (the shape
    /// op results take), so a lone `$input` argument reaches arg marshaling as a *string* and
    /// wraps under the op's sole required param. `FlowStore::seed` used to store the raw
    /// structural value instead — the same flow then delivered the *bare object* as the op's whole
    /// input: a different input shape depending on how `$input` was bound (found adopting D-56 in
    /// ai-agents, where preset ops broke exactly this way).
    #[tokio::test]
    async fn a_seeded_object_marshals_exactly_like_a_literal_bound_one() {
        let mut client = FlowClient::builder()
            .auto_approve(true)
            .build(MockProvider::one("noop"), temp_root("seed-parity"))
            .unwrap();
        client.register_op(Arc::new(EchoArgsTool));

        // Baseline: the pre-D-56 workaround — the object arrives via a prepended `lit` bind.
        let lit_flow: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "bind", "name": "input",
                  "value": { "kind": "lit", "value": {"query": "flux", "limit": 3} } },
                { "kind": "return", "value": { "kind": "call", "op": "echo_args",
                    "args": [ { "kind": "var", "name": "input" } ] } }
            ]
        }))
        .unwrap();
        let lit_out = client
            .execute_with(&lit_flow, serde_json::Map::new())
            .await
            .unwrap();

        // The same call, but the object arrives via `execute_with` seeding (the D-56 seam).
        let seeded_flow: DraftAst = serde_json::from_value(json!({
            "body": [ { "kind": "return", "value": { "kind": "call", "op": "echo_args",
                "args": [ { "kind": "var", "name": "input" } ] } } ]
        }))
        .unwrap();
        let seeded_out = client
            .execute_with(
                &seeded_flow,
                one_input("input", json!({"query": "flux", "limit": 3})),
            )
            .await
            .unwrap();

        // The lit-bound lone object canonicalizes to text and string-wraps under the sole
        // required param (`value`) — and the seeded run is indistinguishable from it.
        assert!(
            lit_out.result.contains("\"value\""),
            "a lit-bound lone object string-wraps under the sole required param, got: {}",
            lit_out.result
        );
        assert_eq!(
            seeded_out.result, lit_out.result,
            "a seeded $input must reach the op exactly like a literal-bound one"
        );
    }

    /// A consumer-injected approver (`FlowClientBuilder::approver`) — not the `auto_approve`
    /// binary — decides per op: the policy allows the echo but denies the boom, with no permission
    /// rules involved. The seam a multi-tenant consumer needs for a risk-aware confirm gate
    /// (ai-agents R-10), mirroring `LocalSpawner::with_approver` on the sub-agent path.
    #[tokio::test]
    async fn an_injected_approver_policy_gates_per_op() {
        struct DenyBoom;
        #[async_trait]
        impl flux_runtime::Approver for DenyBoom {
            async fn request(
                &self,
                tool: &str,
                _subjects: &[String],
                _intents: &flux_spec::IntentSet,
            ) -> flux_runtime::ApprovalChoice {
                if tool == "boom" {
                    flux_runtime::ApprovalChoice::Deny
                } else {
                    flux_runtime::ApprovalChoice::Allow
                }
            }
        }

        let mut client = FlowClient::builder()
            .approver(Arc::new(DenyBoom))
            .build(MockProvider::one("noop"), temp_root("approver-policy"))
            .unwrap();
        client.register_op(Arc::new(EchoArgsTool));
        client.register_op(Arc::new(BoomTool));

        // The policy-allowed op dispatches (auto_approve was never set)...
        let echo: DraftAst = serde_json::from_value(json!({
            "body": [ { "kind": "return", "value": {
                "kind": "call", "op": "echo_args",
                "args": [ { "kind": "lit", "value": "ALLOWED" } ] } } ]
        }))
        .unwrap();
        let out = client
            .execute_with(&echo, serde_json::Map::new())
            .await
            .unwrap();
        assert!(
            out.result.contains("ALLOWED"),
            "the policy-allowed op must run, got: {}",
            out.result
        );

        // ...while the policy-denied op never executes and surfaces as a soft denial.
        let boom: DraftAst = serde_json::from_value(json!({
            "body": [ { "kind": "call", "op": "boom", "args": [] } ]
        }))
        .unwrap();
        let res = client.execute_with(&boom, serde_json::Map::new()).await;
        let ran = res
            .map(|r| r.result.contains("BOOM EXECUTED"))
            .unwrap_or(false);
        assert!(!ran, "the injected policy must gate the denied op");
    }

    #[tokio::test]
    async fn composite_op_executes_through_dispatch_and_keeps_locals_scoped() {
        let mut client = FlowClient::builder()
            .auto_approve(true)
            .build(MockProvider::one("noop"), temp_root("composite"))
            .unwrap();
        client.register_op(Arc::new(EchoArgsTool));
        let module = client
            .parse_module(
                r#"
op shout(value: String) -> String
  description "echo through a composite"
  risk "low"
  idempotency "idempotent"
  effects [read]
  expose true

  $local = echo_args($value)
  return $local

flow main
  $result = shout("HELLO-COMPOSITE")
  return $result
"#,
            )
            .unwrap();
        let Module::Program(program) = module else {
            panic!("op declaration makes this a program");
        };
        client.register_composites(program.ops.clone());
        let ast = &program.flows[0];

        client.analyze(ast).expect("composite flow analyzes");
        let out = client.execute(ast).await.unwrap();
        assert!(out.result.contains("HELLO-COMPOSITE"), "got {}", out.result);
        assert_eq!(out.tool_calls, vec!["shout", "echo_args"]);

        let view = client.store.view("flux-sdk").unwrap();
        assert!(
            view.symbols.iter().any(|s| s.name.0 == "result"),
            "caller bind should remain visible"
        );
        assert!(
            view.symbols.iter().all(|s| s.name.0 != "local"),
            "composite local must not leak into caller view: {:?}",
            view.symbols
        );
    }

    #[test]
    fn composite_validation_rejects_understated_effects_await_and_recursion() {
        let mut client = FlowClient::builder()
            .build(MockProvider::one("noop"), temp_root("composite-invalid"))
            .unwrap();
        client.register_op(Arc::new(EchoArgsTool));

        let module = client
            .parse_module(
                r#"
op bad_effect(value: String) -> String
  description "missing read effect"
  risk "low"
  idempotency "idempotent"
  effects []
  $x = echo_args($value)
  return $x

op waits
  description "not allowed"
  risk "low"
  idempotency "idempotent"
  effects []
  await "reply"

op a
  description "cycle"
  risk "low"
  idempotency "idempotent"
  effects []
  b()

op b
  description "cycle"
  risk "low"
  idempotency "idempotent"
  effects []
  a()

flow main
  return "unused"
"#,
            )
            .unwrap();
        let Module::Program(program) = module else {
            panic!("program")
        };
        let diags =
            flux_flow::registry::analyze_composites(&program.ops, client.registry()).unwrap_err();
        let joined = join_diags(&diags);
        assert!(joined.contains("missing declared effect"), "{joined}");
        assert!(joined.contains("cannot contain `await`"), "{joined}");
        assert!(joined.contains("recursive composite op cycle"), "{joined}");
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

    // ----- D-56: analyze with pre-bound seed names -----

    #[tokio::test]
    async fn analyze_seeded_accepts_an_undeclared_execute_with_seed() {
        // `$settings` is referenced but neither a flow param nor bound anywhere in the body — the
        // exact shape ai-agents had to work around with hand-prepended `Bind` nodes.
        let client = FlowClient::builder()
            .model("mock")
            .build(MockProvider::one("noop"), temp_root("seeded-analyze"))
            .unwrap();
        let ast = client.parse("flow\n  return $settings").unwrap();

        // (a) plain `analyze` reports it unbound.
        let diags = client.analyze(&ast).unwrap_err();
        assert!(
            !diags.is_empty(),
            "an undeclared, unseeded $settings must be reported unbound"
        );

        // (b) `analyze_seeded` with the intended seed name analyzes clean.
        client
            .analyze_seeded(&ast, ["settings"])
            .expect("a seeded name should count as bound, no flow-param declaration required");

        // (c) it actually executes end-to-end via `execute_with` seeding that same name.
        let out = client
            .execute_with(&ast, one_input("settings", json!("prod-config")))
            .await
            .unwrap();
        assert!(
            out.result.contains("prod-config"),
            "the seeded value should surface as the result, got: {}",
            out.result
        );
    }

    #[tokio::test]
    async fn analyze_seeded_with_unreferenced_name_is_harmless() {
        let client = FlowClient::builder()
            .model("mock")
            .build(MockProvider::one("noop"), temp_root("seeded-unused"))
            .unwrap();
        let ast = client.parse("flow\n  return \"ok\"").unwrap();
        client
            .analyze_seeded(&ast, ["never_referenced"])
            .expect("a seeded name the flow never references must not break analysis");
    }

    #[tokio::test]
    async fn analyze_seeded_with_empty_set_matches_analyze() {
        let client = FlowClient::builder()
            .model("mock")
            .build(MockProvider::one("noop"), temp_root("seeded-empty"))
            .unwrap();
        let ast = client.parse("flow\n  return $settings").unwrap();

        let plain = client.analyze(&ast).unwrap_err();
        let seeded_empty = client
            .analyze_seeded(&ast, std::iter::empty::<String>())
            .unwrap_err();
        assert_eq!(
            join_diags(&plain),
            join_diags(&seeded_empty),
            "an empty seed set must behave exactly like `analyze`"
        );
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

    /// A provider that bills a fixed [`Usage`] on every call — so a cognition op's model call has
    /// token spend to record. Reusable across calls (a two-`ai.extract` flow calls it twice).
    struct UsageMock(Usage);
    #[async_trait]
    impl Provider for UsageMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> CoreResult<ChunkStream> {
            let chunks = vec![
                Ok(Chunk::TextDelta("[]".into())),
                Ok(Chunk::Usage(self.0.clone())),
                Ok(Chunk::Done { stop_reason: None }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    /// D-150: a flow's model calls report their token spend. Two `ai.extract` calls each bill
    /// 100/20; `ExecutionResult.usage` sums them (independent calls → every field summed, so 200/40).
    /// A pure-ops flow (no model call) reports `None`.
    #[tokio::test]
    async fn execution_result_sums_cognition_usage_and_none_for_pure_ops() {
        let per_call = Usage {
            input_tokens: 100,
            output_tokens: 20,
            ..Default::default()
        };
        let client = FlowClient::builder()
            .model("test-model")
            .auto_approve(true) // cognition ops egress, so they gate by default
            .build(Arc::new(UsageMock(per_call)), temp_root("cog-usage"))
            .unwrap();

        // Two independent `ai.extract` calls.
        let ast: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "call", "op": "ai.extract",
                  "args": [{ "kind": "lit", "value": { "from": "Alice and Bob", "ask": "names" } }] },
                { "kind": "call", "op": "ai.extract",
                  "args": [{ "kind": "lit", "value": { "from": "Carol and Dave", "ask": "names" } }] }
            ]
        }))
        .unwrap();
        let out = client.execute(&ast).await.unwrap();
        assert_eq!(out.tool_calls, vec!["ai.extract", "ai.extract"]);
        let usage = out
            .usage
            .expect("a flow with model calls reports summed usage");
        assert_eq!(
            usage.input_tokens, 200,
            "two 100-token calls sum (independent prompts, not last-wins)"
        );
        assert_eq!(usage.output_tokens, 40, "two 20-token calls sum");

        // A pure-ops flow makes no model call → no usage.
        let root = temp_root("pure-ops");
        std::fs::write(root.join("note.txt"), "hi").unwrap();
        let pure = FlowClient::builder()
            .model("test-model")
            .auto_approve(true)
            .build(Arc::new(UsageMock(Usage::default())), &root)
            .unwrap();
        let read_ast: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "call", "op": "read",
                  "args": [{ "kind": "lit", "value": "note.txt" }] }
            ]
        }))
        .unwrap();
        let out = pure.execute(&read_ast).await.unwrap();
        assert_eq!(out.tool_calls, vec!["read"]);
        assert!(
            out.usage.is_none(),
            "a flow with no model call reports no usage: {:?}",
            out.usage
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A tool that sleeps briefly before returning — so a streamed run's events arrive while the op
    /// is still in flight.
    struct SlowTool;
    #[async_trait]
    impl Tool for SlowTool {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only("slow", "slow op", json!({"type": "object"}))
        }
        async fn execute(&self, _c: &ToolContext, _p: Value) -> CoreResult<ToolResult> {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            Ok(ToolResult::ok("slow-done"))
        }
    }

    fn slow_flow() -> DraftAst {
        serde_json::from_value(json!({
            "body": [ { "kind": "call", "op": "slow", "args": [] } ]
        }))
        .unwrap()
    }

    /// D-158: `execute_with_sink` streams each dispatched op's `tool_call` AND `tool_result` to a
    /// consumer sink — which the private collector behind `execute` drops (it keeps only op names).
    #[tokio::test]
    async fn execute_with_sink_streams_tool_results() {
        #[derive(Default)]
        struct RecordSink {
            calls: Vec<String>,
            results: Vec<String>,
        }
        impl AgentSink for RecordSink {
            fn tool_call(&mut self, name: &str, _input: &Value) {
                self.calls.push(name.to_string());
            }
            fn tool_result(&mut self, name: &str, _result: &ToolResult) {
                self.results.push(name.to_string());
            }
        }

        let mut client = FlowClient::builder()
            .auto_approve(true)
            .build(MockProvider::one("noop"), temp_root("with-sink"))
            .unwrap();
        client.register_op(Arc::new(SlowTool));

        let mut sink = RecordSink::default();
        let out = client
            .execute_with_sink(&slow_flow(), &mut sink)
            .await
            .unwrap();
        assert_eq!(
            out.tool_calls,
            vec!["slow"],
            "the result still carries op names"
        );
        assert_eq!(sink.calls, vec!["slow"], "the consumer saw the tool_call");
        assert_eq!(
            sink.results,
            vec!["slow"],
            "the consumer saw the tool_result (execute's collector drops it)"
        );
    }

    /// D-158: `execute_streamed` yields owned events live while a slow op runs, and `finish` returns
    /// the same `ExecutionResult` as `execute`.
    #[tokio::test]
    async fn execute_streamed_yields_events_then_finishes() {
        use crate::AgentEvent;

        let mut client = FlowClient::builder()
            .auto_approve(true)
            .build(MockProvider::one("noop"), temp_root("streamed"))
            .unwrap();
        client.register_op(Arc::new(SlowTool));

        let mut stream = client.execute_streamed(&slow_flow());
        let mut saw_call = false;
        let mut saw_result = false;
        while let Some(ev) = stream.next().await {
            match ev {
                AgentEvent::ToolCall { name, .. } if name == "slow" => saw_call = true,
                AgentEvent::ToolResult { name, .. } if name == "slow" => saw_result = true,
                _ => {}
            }
        }
        assert!(saw_call, "the stream delivered the op's tool_call live");
        assert!(saw_result, "the stream delivered the op's tool_result live");
        let out = stream.finish().await.unwrap();
        assert_eq!(out.tool_calls, vec!["slow"]);
    }
}
