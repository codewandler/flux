//! The Flux-Lang **lifecycle surface** — the SDK front door for authored deterministic flows.
//!
//! Where [`crate::Client`] runs a conversational turn through the self-hosted Flux-Lang
//! [`FlowEngine`](flux_flow::engine::FlowEngine), [`FlowClient`] exposes one flow's lifecycle
//! directly: parse an authored flow, `analyze` it against the live operation catalog, optionally
//! `optimize` it, and `execute` it through the real safety envelope.
//! None of that machinery is reimplemented here — every method delegates to `flux-flow` (the
//! engine) and `flux-lang` (the language), so the envelope, store, and analyzer are reused, not
//! forked.
//!
//! The wiring that earns its keep is [`assemble_registry`]: it takes the pure built-ins
//! (`flux_tools::register_builtins`) **and** the provider-backed [`CognitionPack`] and registers
//! both into one [`ToolRegistry`]. That is what makes the model-op pack (`ai.extract`, `ai.rank`,
//! `ai.judge`, `ai.reason`, `synth`, `ai.rewrite`) reachable as named ops an authored flow can call — the
//! pack stops being a dead crate the moment a registry is assembled this way.
//!
//! Author with native `.flux` text or build a [`flux_lang::ast::DraftAst`] with the [`crate::dsl`],
//! then [`FlowClient::analyze`] and [`FlowClient::execute`] it. Runnable, no-API-key examples:
//! `examples/parameterized_flow.rs` (text) and `examples/dsl_loops.rs` (Rust DSL).
//!
//! ```ignore
//! // Runnable hermetic version: `cargo run -p codewandler-flux-sdk --example parameterized_flow`.
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
//! // parse → analyze → execute.
//! let flow = client.parse("flow readme -> string\n  $text = read(\"README.md\")\n  return $text\n")?;
//! client.analyze(&flow).map_err(|d| flux_core::Error::Other(format!("{d:?}")))?;
//! let out = client.execute(&flow).await?;
//! println!("{}", out.result);
//! # Ok(()) }
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use flux_cognition::{recorded_usage, CognitionPack};
use flux_core::{Error, Result, Usage};
use flux_flow::ast::SymbolName;
use flux_flow::registry::{analyze_composites, OpRegistry};
use flux_flow::runtime::{
    execute_flow, execute_flow_with_composites, execute_plan, execute_plan_with_composites,
    FlowOutcome,
};
use flux_flow::state::FlowStore;
use flux_flow::{tool_defs_from_registry, AgentSink, VoiceSessionDriver, VoiceSink};
use flux_lang::analyze::analyze_flow;
use flux_lang::prelude;
use flux_lang::program::{CompositeOpDecl, Module};
use flux_orchestrate::{SubAgents, TaskTool};
use flux_provider::{Provider, RealtimeConfig, RealtimeProvider};
#[cfg(test)]
use flux_runtime::ToolContext;
use flux_runtime::{
    AllowApprover, Approver, DenyApprover, ExecutionAuthorization, ExecutionEnvironment, Executor,
    PermissionManager, ResourceLimits, Spawner, Tool, ToolRegistry,
};
use flux_secret::Redactor;
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
/// the pack is never installed and authored flows cannot call it.
pub fn try_assemble_registry(
    provider: Arc<dyn Provider>,
    model: impl Into<String>,
) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    flux_tools::try_register_builtins(&mut registry)?;
    CognitionPack::new(provider, model).try_register(&mut registry)?;
    Ok(registry)
}

/// Compatibility wrapper for callers that cannot yet propagate registry assembly errors.
///
/// # Deprecated
///
/// Use [`try_assemble_registry`]; it preserves both registration sources in duplicate diagnostics.
pub fn assemble_registry(provider: Arc<dyn Provider>, model: impl Into<String>) -> ToolRegistry {
    try_assemble_registry(provider, model).expect("FlowClient registry assembly failed")
}

/// Builder for a [`FlowClient`]. Shares the envelope knobs (permission rules, approval policy,
/// sandbox posture) with [`crate::ClientBuilder`] via one internal type so the two front doors
/// cannot drift; the model + workspace root are supplied at [`build`](Self::build).
pub struct FlowClientBuilder {
    model: String,
    envelope: crate::envelope::Envelope,
    seed_prelude: bool,
    storage: Option<crate::Storage>,
}

impl Default for FlowClientBuilder {
    fn default() -> Self {
        Self {
            model: "unknown".to_string(),
            // Reads pre-allowed; everything else denied unless `auto_approve` (no UI in a library).
            envelope: crate::envelope::Envelope::with_default_allow(&["read", "glob", "grep"]),
            // Seed the operation catalog `$defs` with the v1-core artifact ontology by default.
            seed_prelude: true,
            // Unset ⇒ an in-memory flow store, the pre-0.16 behavior.
            storage: None,
        }
    }
}

impl FlowClientBuilder {
    /// Set the model id every model-backed operation uses.
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
    /// Approve every tool call automatically — an **autonomous posture**. Note that model-backed
    /// cognition ops egress over the network, so they gate by default and this ungates them.
    ///
    /// Choosing it brings its confinement and its ceiling with it (C-444): unless you state otherwise,
    /// the built client resolves to sandbox `require` with the sandbox network closed and to
    /// [`ResourceLimits::autonomous`](crate::ResourceLimits::autonomous) ceilings — the same pairing the
    /// CLI applies to its own auto-approved and headless surfaces (C-262 / C-410). Both are floors over
    /// silence: [`with_sandbox`](Self::with_sandbox) and [`resource_limits`](Self::resource_limits) win
    /// outright. An injected [`Approver`] receives the same conservative floor because the SDK cannot
    /// distinguish an interactive policy from a blanket allow. See
    /// [`ClientBuilder::auto_approve`](crate::ClientBuilder::auto_approve) for the full contract.
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
    /// Install the mandatory authorization policy and resolved caller identity. It remains the
    /// floor even when every approval prompt is auto-approved.
    pub fn with_authorization(
        mut self,
        policy: flux_policy::AuthorizationPolicy,
        caller: flux_policy::Caller,
        trust: flux_policy::Trust,
    ) -> Self {
        self.envelope.authorization = ExecutionAuthorization::new(policy, caller, trust);
        self
    }
    /// Install the shared secret redactor used by every per-run executor.
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.envelope.redactor = redactor;
        self
    }
    /// Bound what this runtime **uses** (C-290): a ceiling on simultaneously executing tool calls
    /// and on retained result bytes. Enforced inside the safety envelope, so it binds every op this
    /// client's own executors run — including the branches of a `parallel` block, which is where an
    /// authored flow actually produces concurrency. It does **not** descend into sub-agents
    /// attached via [`with_sub_agents`](FlowClient::with_sub_agents): a `task`-delegated child gets
    /// a fresh, unbounded executor.
    ///
    /// The default follows the posture (C-444): unbounded under supervision, and
    /// [`ResourceLimits::autonomous`](crate::ResourceLimits::autonomous) under
    /// [`auto_approve`](Self::auto_approve). Calling this wins outright either way. See
    /// [`ClientBuilder::resource_limits`](crate::ClientBuilder::resource_limits) for the full
    /// contract — why the queue timeout is not clamped, and why bounding a delegated tree needs
    /// [`with_max_live_agents`](crate::ResourceLimits::with_max_live_agents) rather than a shared
    /// semaphore.
    pub fn resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.envelope.resource_limits = Some(limits);
        self
    }
    /// Skip seeding the operation catalog `$defs` with the prelude artifact ontology (default: seed).
    pub fn without_prelude(mut self) -> Self {
        self.seed_prelude = false;
        self
    }
    /// Inject an explicit OS-sandbox [`Sandbox`] that the built client's guarded `System` enforces on
    /// every spawn. When left unset (the default), the posture is resolved from the environment at
    /// [`build`](Self::build) via `Sandbox::resolve(SandboxSettings::from_env())` — so a consumer that
    /// exports `FLUX_SANDBOX=require` gets confinement without calling this (off ⇒ disabled, safe) —
    /// except under an autonomous posture, where [`auto_approve`](Self::auto_approve) raises the floor
    /// to fail-closed `require` with the network closed (C-444).
    ///
    /// **This call wins outright**, in either direction: it pins a posture independent of ambient env,
    /// and it is also how an autonomous embedder whose isolation comes from elsewhere declines the
    /// raise in one visible line. See [`Sandbox`](crate::Sandbox).
    pub fn with_sandbox(mut self, sandbox: Sandbox) -> Self {
        self.envelope.sandbox = Some(sandbox);
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
        // C-444: resolved before the envelope is destructured below — silence means the approval
        // posture decides (autonomous ⇒ bounded), an explicit call is honored verbatim.
        let resource_limits = self.envelope.resolve_resource_limits();
        let system = Arc::new(System::new(Workspace::new(root.into())?).with_sandbox(sandbox));
        let registry = try_assemble_registry(provider.clone(), self.model.clone())?;
        let store = Arc::new(self.storage.unwrap_or_default().into_flow_store()?);
        let prelude_defs = if self.seed_prelude {
            prelude::prelude_schema()
        } else {
            Value::Object(serde_json::Map::new())
        };
        Ok(FlowClient {
            model: self.model,
            registry,
            system,
            store,
            allow: self.envelope.allow,
            deny: self.envelope.deny,
            auto_approve: self.envelope.auto_approve,
            approver: self.envelope.approver,
            authorization: self.envelope.authorization,
            redactor: self.envelope.redactor,
            resource_limits,
            prelude_defs,
            session_id: "flux-sdk".to_string(),
            spawner: None,
            composites: Vec::new(),
        })
    }
}

/// A Flux-Lang lifecycle façade: holds the provider + model + the assembled registry, and exposes
/// the `parse → analyze → execute` surface plus a registration surface for extra ops/packs.
///
/// # Static analysis is yours to run (L-123)
///
/// **No `execute*` method on this type calls [`analyze`](Self::analyze) for you.** Inside flux, a
/// flow body the engine did not author is gated before it runs — the agent loop's AST at assembly,
/// the model's `flow_run` AST inside `lower`, a `fork --edit` plan, an app journey. `FlowClient` is
/// the one door that deliberately sits on the other side of that line, and the invariant is
/// recorded in `docs/designs/flux-lang-hardening.md` so the next entry point knows which side it is
/// on. **As an embedder you own the check**: call `analyze` (or
/// [`analyze_seeded`](Self::analyze_seeded)) before `execute`, as the crate-level example does.
///
/// It is opt-in rather than automatic for two reasons that are properties of an SDK, not of the
/// analyzer:
///
/// - **Seeding would break.** [`execute_with`](Self::execute_with) injects `$name` values that the
///   AST never declares; plain `analyze` reports exactly those as unbound (L-15). Forcing it would
///   reject the seeding pattern this type documents, and only `analyze_seeded` — which needs the
///   input names the *caller* holds — gets it right.
/// - **Repeated execution should not re-pay for it.** An embedder running one stored, already
///   validated AST in a loop analyzes once at load, not once per call.
///
/// What still holds if you skip it: analysis is a *static contract* check, never the authorization
/// boundary. Every op dispatches through `Executor::dispatch` under this client's permission rules
/// and approver, and L-116's per-execution loop budget bounds iteration at run time. Skipping
/// `analyze` costs you a clean pre-flight diagnostic, not the envelope.
pub struct FlowClient {
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
    /// Mandatory policy and identity profile cloned into every per-run executor.
    authorization: ExecutionAuthorization,
    /// Shared secret scrubber installed on every per-run execution environment.
    redactor: Redactor,
    /// C-290: the host's resource ceilings. Installed on every per-run executor; because the
    /// concurrency ceiling rides a shared handle, the per-run executors this client mints all count
    /// against one budget instead of each getting a private one.
    resource_limits: ResourceLimits,
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

    /// The resolved model id every model-backed operation uses.
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

    /// The merged artifact `$defs` map (the operation catalog `$defs`), for inspection or merging into
    /// a downstream schema.
    pub fn prelude_defs(&self) -> &Value {
        &self.prelude_defs
    }

    // ----- registration surface -----

    /// Register a single extra op (any [`Tool`]) into the assembled registry.
    pub fn register_op(&mut self, tool: Arc<dyn Tool>) -> &mut Self {
        self.try_register_op(tool)
            .expect("FlowClient custom operation registration failed")
    }

    /// Fallible counterpart to [`register_op`](Self::register_op). New integrations should use
    /// this form so a collision with a built-in or another custom pack is returned to the caller
    /// with both registration sources instead of becoming a startup panic.
    pub fn try_register_op(&mut self, tool: Arc<dyn Tool>) -> Result<&mut Self> {
        self.registry
            .try_register_from("sdk FlowClient custom operation", tool)?;
        Ok(self)
    }

    /// Register a *pack* — any `FnOnce(&mut ToolRegistry)`, e.g. another
    /// `CognitionPack::register`-style installer or `flux_tools::register_dev_builtins`.
    pub fn register_pack<F: FnOnce(&mut ToolRegistry)>(&mut self, pack: F) -> &mut Self {
        self.try_register_pack(|registry| {
            pack(registry);
            Ok(())
        })
        .expect("FlowClient custom pack registration failed")
    }

    /// Fallible pack-registration seam. Pack installers should use source-labelled `try_*`
    /// registry methods so collisions identify both independently assembled contributors.
    pub fn try_register_pack<F>(&mut self, pack: F) -> Result<&mut Self>
    where
        F: FnOnce(&mut ToolRegistry) -> Result<()>,
    {
        pack(&mut self.registry)?;
        Ok(self)
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
            self.registry
                .try_register_from(format!("plugin:{name}"), tool)?;
        }
        Ok(self)
    }

    /// Attach named sub-agents: register the `task` tool into this client's catalog and build the
    /// spawner from `sub_agents` over the client's guarded `System`. After this, a flow that calls
    /// `task(role, …)` delegates to a role's sub-agent through the same safety envelope. The single
    /// seam — a consumer (e.g. a multi-tenant service) drives sub-agents without re-assembling the
    /// spawner, executor, and context by hand.
    ///
    /// A top-level one-shot `execute`/`run` has no cancellation token or parent lineage, so a
    /// sub-agent's only lifecycle bound there is its wall-clock deadline. When an adapter constructs
    /// this client inside a live guarded turn, the fresh per-run context inherits that turn's cancel,
    /// session lineage and reporter; streamed execution pins the snapshot before `tokio::spawn`.
    /// A generous **default `wall_clock` (10 min)** remains the safety backstop in both cases; a
    /// consumer with longer-running work overrides it via [`SubAgents::with_limits`].
    pub fn with_sub_agents(&mut self, sub_agents: SubAgents) -> &mut Self {
        self.try_with_sub_agents(sub_agents)
            .expect("FlowClient sub-agent operation registration failed")
    }

    /// Fallible counterpart to [`with_sub_agents`](Self::with_sub_agents).
    pub fn try_with_sub_agents(&mut self, mut sub_agents: SubAgents) -> Result<&mut Self> {
        if sub_agents.limits.wall_clock.is_none() {
            sub_agents.limits.wall_clock = Some(std::time::Duration::from_secs(600));
        }
        sub_agents = sub_agents
            .with_authorization_cell(
                self.authorization.policy().clone(),
                self.authorization.identity(),
            )
            // C-299: children inherit this client's ceilings, per agent (own concurrency budget) —
            // see `ClientBuilder::resource_limits`.
            .with_resource_limits(self.resource_limits.clone());
        self.registry.try_register_from(
            "sdk FlowClient sub-agent task operation",
            Arc::new(TaskTool),
        )?;
        self.spawner = Some(sub_agents.into_spawner(self.system.clone()));
        Ok(self)
    }

    /// Attach named sub-agents with an explicit adaptive intent/explore policy for every child.
    /// Stage model, reasoning effort, output-token and call ceilings are resolved on the child's
    /// provider before its first request. The policy is independent of [`SubAgents::with_limits`],
    /// which bounds the whole child/outer loop. Existing [`with_sub_agents`](Self::with_sub_agents)
    /// callers retain [`flux_agent::AdaptiveLoopPolicy::default`].
    pub fn with_sub_agents_policy(
        &mut self,
        sub_agents: SubAgents,
        adaptive_policy: flux_agent::AdaptiveLoopPolicy,
    ) -> &mut Self {
        self.try_with_sub_agents_policy(sub_agents, adaptive_policy)
            .expect("FlowClient sub-agent operation registration failed")
    }

    /// Fallible counterpart to [`with_sub_agents_policy`](Self::with_sub_agents_policy).
    pub fn try_with_sub_agents_policy(
        &mut self,
        mut sub_agents: SubAgents,
        adaptive_policy: flux_agent::AdaptiveLoopPolicy,
    ) -> Result<&mut Self> {
        if sub_agents.limits.wall_clock.is_none() {
            sub_agents.limits.wall_clock = Some(std::time::Duration::from_secs(600));
        }
        sub_agents = sub_agents
            .with_authorization_cell(
                self.authorization.policy().clone(),
                self.authorization.identity(),
            )
            // C-299: as in `try_with_sub_agents` — children inherit this client's ceilings.
            .with_resource_limits(self.resource_limits.clone());
        self.registry.try_register_from(
            "sdk FlowClient sub-agent task operation",
            Arc::new(TaskTool),
        )?;
        self.spawner = Some(
            sub_agents.into_spawner_with_adaptive_policy(self.system.clone(), adaptive_policy),
        );
        Ok(self)
    }

    /// Merge an artifact `$defs` map (e.g. [`flux_lang::prelude::prelude_schema`]) into the stashed
    /// operation catalog `$defs`. Existing keys are overwritten by `defs`.
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

    // ----- the deterministic lifecycle: parse → analyze → execute -----

    /// Deterministic text → AST for a stored / already-validated flow — the non-NL partner of
    /// model-independent parser with **no** provider round-trip. Wraps `flux_lang`'s parser so a
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
    ///
    /// Runs `ast` **as given** — [`analyze`](Self::analyze) is not called for you. See the
    /// type-level *"Static analysis is yours to run"* note for why, and what still holds if you
    /// skip it.
    pub async fn execute(&self, ast: &DraftAst) -> Result<ExecutionResult> {
        let executor = self.build_executor();
        let mut sink = ExecSink::default();
        let outcome = execute_kernel(
            &self.store,
            &executor,
            &self.session_id,
            ExecutionProgram::Flow(ast),
            &self.composites,
            &mut sink,
        )
        .await?;
        finish_outcome(outcome, sink, recorded_usage(&executor.evidence()))
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
    /// reports a seed-only, undeclared `$name` as unbound). Calling it is **your** step: this method
    /// runs `ast` as given, and that pairing is precisely why the check cannot be made automatic
    /// here — see the type-level *"Static analysis is yours to run"* note.
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
        let outcome = execute_kernel(
            &store,
            &executor,
            &self.session_id,
            ExecutionProgram::Flow(ast),
            &self.composites,
            &mut sink,
        )
        .await?;
        finish_outcome(outcome, sink, recorded_usage(&executor.evidence()))
    }

    /// Execute `ast` while **streaming** every dispatch to your own [`AgentSink`] as it happens — each
    /// op's `tool_call` **and** `tool_result`, text, and observations — and still returning the
    /// collected [`ExecutionResult`]. The observable counterpart of [`execute`](Self::execute), whose
    /// private collector drops everything but op names. Same envelope, same one-shot `await` handling
    /// — and, like every `execute*` door here, it runs `ast` as given: see the type-level
    /// *"Static analysis is yours to run"* note.
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
        let outcome = execute_kernel(
            &self.store,
            &executor,
            &self.session_id,
            ExecutionProgram::Flow(ast),
            &self.composites,
            &mut tee,
        )
        .await?;
        // The tee's collector holds the op names (as `TurnOutput.tool_calls`) for the result.
        let names = std::mem::take(&mut tee.collect.0.tool_calls);
        finish_outcome(
            outcome,
            ExecSink { tool_calls: names },
            recorded_usage(&executor.evidence()),
        )
    }

    /// Execute `ast` as a [`FlowStream`] — a live stream of owned [`AgentEvent`](crate::AgentEvent)s
    /// plus `finish() -> ExecutionResult`. The flow runs on a spawned task, so events arrive as they
    /// happen whether or not you are polling (unlike the fully-buffered [`execute`](Self::execute)).
    ///
    /// Runs `ast` as given — [`analyze`](Self::analyze) is not called for you; see the type-level
    /// *"Static analysis is yours to run"* note. Worth a second look on this door specifically: it
    /// returns before the flow finishes, so a diagnostic `analyze` would have raised synchronously
    /// instead surfaces as a mid-stream failure.
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
            let outcome = execute_kernel(
                &store,
                &executor,
                &session_id,
                ExecutionProgram::Flow(&ast),
                &composites,
                &mut sink,
            )
            .await?;
            let names = std::mem::take(&mut sink.collect.0.tool_calls);
            finish_outcome(
                outcome,
                ExecSink { tool_calls: names },
                recorded_usage(&executor.evidence()),
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
        let outcome = execute_kernel(
            &self.store,
            &executor,
            &self.session_id,
            ExecutionProgram::Plan {
                body: &ast.body,
                plan: &plan,
            },
            &self.composites,
            &mut sink,
        )
        .await?;
        Ok(ExecutionResult {
            result: outcome.result,
            transcript: outcome.transcript,
            steps: outcome.steps,
            tool_calls: sink.tool_calls,
            usage: recorded_usage(&executor.evidence()),
        })
    }

    /// `parse` → `analyze` → `execute_with`. Runs
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
        let mut environment = ExecutionEnvironment::new(
            self.system.clone(),
            self.registry.clone(),
            perms,
            approver,
            self.authorization.clone(),
        )
        .with_redactor(self.redactor.clone())
        .with_resource_limits(self.resource_limits.clone());
        // Thread the sub-agent spawner into the per-run context when one is attached, so a `task`
        // call can delegate. `None` (the common case) leaves the context exactly as before.
        if let Some(spawner) = &self.spawner {
            environment = environment.with_spawner(spawner.clone());
        }
        // A guarded adapter may build this one-shot client while its parent runtime-turn context is
        // available only through a lexical task-local. Pin the COMPLETE snapshot (cancellation,
        // parent session and child reporter) onto this fresh per-run context before
        // `execute_streamed` moves it into `tokio::spawn`; Tokio task-locals do not propagate to a
        // new task. Outside a parent turn the snapshot is empty, preserving one-shot behavior.
        environment.inherit_runtime_turn().into_executor()
    }
}

/// The two interpreter entry shapes accepted by the SDK execution kernel. Keeping the shape here,
/// rather than branching in every public method, makes plain/composite dispatch selection one
/// auditable decision for direct, seeded, sink-backed, streamed, and optimized runs.
enum ExecutionProgram<'a> {
    Flow(&'a DraftAst),
    Plan {
        body: &'a [flux_flow::ast::Node],
        plan: &'a flux_flow::ast::PhysicalPlan,
    },
}

/// Run one flow or physical plan through the matching plain/composite interpreter adapter and fold
/// the engine error into the SDK's single error type. Result collection stays with each public
/// surface because streamed and consumer-sink runs own different collectors.
async fn execute_kernel(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    program: ExecutionProgram<'_>,
    composites: &[CompositeOpDecl],
    sink: &mut dyn AgentSink,
) -> Result<FlowOutcome> {
    let outcome = match (program, composites.is_empty()) {
        (ExecutionProgram::Flow(ast), true) => {
            execute_flow(store, executor, session_id, ast, sink).await
        }
        (ExecutionProgram::Flow(ast), false) => {
            execute_flow_with_composites(store, executor, session_id, ast, composites, sink).await
        }
        (ExecutionProgram::Plan { body, plan }, true) => {
            execute_plan(store, executor, session_id, body, plan, sink).await
        }
        (ExecutionProgram::Plan { body, plan }, false) => {
            execute_plan_with_composites(store, executor, session_id, body, plan, composites, sink)
                .await
        }
    };
    outcome.map_err(|e| Error::Other(e.to_string()))
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
/// run's summed cognition spend (see [`recorded_usage`]).
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use flux_core::{Chunk, Result as CoreResult};
    use flux_provider::{ChunkStream, Request};
    use serde_json::json;
    use std::sync::Mutex;

    /// A hermetic provider that replays canned text, one `stream()` call at a time. Most authored
    /// flow tests never call it; cognition-operation tests use it without network access.
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

    /// C-470: the two public SDK doors resolve the same autonomous envelope. This lives inside the
    /// crate so the proof observes the binding `System` and `ResourceLimits` without publishing a
    /// new inspection API solely for a test.
    #[test]
    fn both_sdk_doors_resolve_the_same_autonomous_posture() {
        let client = crate::Client::builder()
            .model("mock")
            .auto_approve(true)
            .build(
                Box::new(MockProvider::new(["noop".to_string()])),
                temp_root("client-posture"),
            )
            .unwrap();
        let flow = FlowClient::builder()
            .model("mock")
            .auto_approve(true)
            .build(MockProvider::one("noop"), temp_root("flow-posture"))
            .unwrap();

        let client_system = client.engine().executor.context().system();
        let client_sandbox = client_system.sandbox().settings();
        let flow_sandbox = flow.system.sandbox().settings();
        assert_eq!(flow_sandbox.mode, client_sandbox.mode);
        assert_eq!(flow_sandbox.network, client_sandbox.network);
        assert_eq!(flow_sandbox.extra_writable, client_sandbox.extra_writable);

        let client_limits = client.resource_limits();
        let flow_limits = &flow.resource_limits;
        assert_eq!(
            flow_limits.max_concurrent_tool_calls(),
            client_limits.max_concurrent_tool_calls()
        );
        assert_eq!(
            flow_limits.max_retained_result_bytes(),
            client_limits.max_retained_result_bytes()
        );
        assert_eq!(
            flow_limits.max_evidence_payload_bytes(),
            client_limits.max_evidence_payload_bytes()
        );
        assert_eq!(
            flow_limits.max_live_agents(),
            client_limits.max_live_agents()
        );
        assert_eq!(
            flow_limits.tool_call_queue_timeout(),
            client_limits.tool_call_queue_timeout()
        );
    }

    #[tokio::test]
    async fn round_trip_analyze_then_execute_on_a_json_flow() {
        // A hand-authored DraftAst: read a file we control, then return it. Exercises
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

    /// C-60: authored-flow auto-approval cannot widen the explicitly installed policy floor.
    #[tokio::test]
    async fn flow_client_auto_approval_cannot_widen_authorization() {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool_hits = hits.clone();
        let (caller, trust) = flux_policy::local_identity("flow-sdk-test");
        let mut client = FlowClient::builder()
            .auto_approve(true)
            .with_authorization(flux_policy::AuthorizationPolicy::default(), caller, trust)
            .build(MockProvider::one("noop"), temp_root("policy-floor"))
            .unwrap();
        client.register_op(flux_runtime::tool_fn(
            flux_spec::ToolSpec::read_only(
                "policy_probe",
                "must not run",
                json!({"type": "object"}),
            )
            .with_access(vec![flux_spec::AccessKind::Filesystem]),
            move |_input| {
                let hits = tool_hits.clone();
                async move {
                    hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(json!("ran"))
                }
            },
        ));
        let ast: DraftAst = serde_json::from_value(json!({
            "body": [{"kind": "call", "op": "policy_probe", "args": []}]
        }))
        .unwrap();

        let result = client.execute(&ast).await;
        assert!(result.is_err(), "policy denial must halt the flow");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
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
        assert_eq!(
            client.system.sandbox().settings().mode,
            flux_system::sandbox::SandboxMode::Require,
            "an opaque flow approver must receive the conservative confinement floor"
        );
        assert!(
            !client.resource_limits.is_unbounded(),
            "an opaque flow approver must receive the delegated-tree ceiling"
        );
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

    async fn execution_variants(client: &FlowClient, ast: &DraftAst) -> Vec<ExecutionResult> {
        let direct = client.execute(ast).await.unwrap();
        let seeded = client
            .execute_with(ast, serde_json::Map::new())
            .await
            .unwrap();
        let mut consumer = ExecSink::default();
        let sink_backed = client.execute_with_sink(ast, &mut consumer).await.unwrap();
        let streamed = client.execute_streamed(ast).finish().await.unwrap();
        let optimized = client.execute_optimized(ast).await.unwrap();
        vec![direct, seeded, sink_backed, streamed, optimized]
    }

    fn assert_execution_variant_parity(
        results: &[ExecutionResult],
        marker: &str,
        expected_calls: &[&str],
    ) {
        assert_eq!(results.len(), 5, "all public execution variants ran");
        let baseline = &results[0];
        assert!(
            baseline.result.contains(marker),
            "baseline result must contain {marker:?}: {}",
            baseline.result
        );
        assert_eq!(
            baseline.tool_calls,
            expected_calls
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>()
        );
        for (index, result) in results.iter().enumerate().skip(1) {
            assert_eq!(
                result.result, baseline.result,
                "result drift at variant {index}"
            );
            assert_eq!(
                result.transcript, baseline.transcript,
                "transcript drift at variant {index}"
            );
            assert_eq!(
                result.steps, baseline.steps,
                "step drift at variant {index}"
            );
            assert_eq!(
                result.tool_calls, baseline.tool_calls,
                "tool-call drift at variant {index}"
            );
            assert_eq!(
                result.usage, baseline.usage,
                "usage drift at variant {index}"
            );
        }
    }

    /// C-71: every public one-shot path delegates the same plain/composite selection to the SDK
    /// kernel. Pin both sides so a future execution surface cannot quietly choose a different
    /// interpreter adapter or fold a different result shape.
    #[tokio::test]
    async fn execution_kernel_preserves_plain_and_composite_result_parity() {
        let mut plain = FlowClient::builder()
            .auto_approve(true)
            .build(MockProvider::one("noop"), temp_root("kernel-plain"))
            .unwrap();
        plain.register_op(Arc::new(EchoArgsTool));
        let plain_ast: DraftAst = serde_json::from_value(json!({
            "body": [{
                "kind": "return",
                "value": {
                    "kind": "call",
                    "op": "echo_args",
                    "args": [{ "kind": "lit", "value": "KERNEL-PLAIN" }]
                }
            }]
        }))
        .unwrap();
        let plain_results = execution_variants(&plain, &plain_ast).await;
        assert_execution_variant_parity(&plain_results, "KERNEL-PLAIN", &["echo_args"]);

        let mut composite = FlowClient::builder()
            .auto_approve(true)
            .build(MockProvider::one("noop"), temp_root("kernel-composite"))
            .unwrap();
        composite.register_op(Arc::new(EchoArgsTool));
        let module = composite
            .parse_module(
                r#"
op shout(value: String) -> String
  description "kernel parity composite"
  risk "low"
  idempotency "idempotent"
  effects [read]
  expose true

  return echo_args($value)

flow main
  return shout("KERNEL-COMPOSITE")
"#,
            )
            .unwrap();
        let Module::Program(program) = module else {
            panic!("op declaration makes this a program");
        };
        composite.register_composites(program.ops);
        let composite_results = execution_variants(&composite, &program.flows[0]).await;
        assert_execution_variant_parity(
            &composite_results,
            "KERNEL-COMPOSITE",
            &["shout", "echo_args"],
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

    #[test]
    fn fallible_flow_registration_rejects_duplicate_custom_operations() {
        let mut client = FlowClient::builder()
            .build(MockProvider::one("noop"), temp_root("duplicate-register"))
            .unwrap();
        client.try_register_op(Arc::new(LookupTool)).unwrap();
        let error = client
            .try_register_op(Arc::new(LookupTool))
            .err()
            .expect("duplicate operation must fail flow client registration")
            .to_string();

        assert!(error.contains("duplicate operation `lookup`"));
        assert!(error.contains("sdk FlowClient custom operation"));
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

    struct NoopSpawnActivity;
    impl flux_runtime::SpawnActivitySink for NoopSpawnActivity {
        fn emit(&self, _activity: flux_runtime::SpawnActivity) {}
    }

    struct ReporterProbe;
    #[async_trait]
    impl Tool for ReporterProbe {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only(
                "reporter_probe",
                "report whether child activity is attached",
                json!({"type": "object"}),
            )
        }
        async fn execute(&self, ctx: &ToolContext, _params: Value) -> CoreResult<ToolResult> {
            Ok(ToolResult::ok(if ctx.spawn_activity_sink().is_some() {
                "inherited"
            } else {
                "missing"
            }))
        }
    }

    struct StreamedNestedAdapter;
    #[async_trait]
    impl Tool for StreamedNestedAdapter {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only(
                "streamed_nested_adapter",
                "open a streamed nested runtime",
                json!({"type": "object"}),
            )
        }
        async fn execute(&self, _ctx: &ToolContext, _params: Value) -> CoreResult<ToolResult> {
            let mut client = FlowClient::builder().auto_approve(true).build(
                MockProvider::one("noop"),
                temp_root("nested-stream-reporter"),
            )?;
            client.register_op(Arc::new(ReporterProbe));
            let ast: DraftAst = serde_json::from_value(json!({
                "body": [{ "kind": "call", "op": "reporter_probe", "args": [] }]
            }))
            .unwrap();
            let stream = client.execute_streamed(&ast);
            let outcome = stream.finish().await?;
            Ok(ToolResult::ok(outcome.result))
        }
    }

    #[tokio::test]
    async fn streamed_nested_runtime_pins_the_lexical_spawn_reporter() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(StreamedNestedAdapter));
        let ctx = ToolContext::new(Arc::new(System::new(
            Workspace::new(temp_root("outer-stream-reporter")).unwrap(),
        )));
        ctx.set_spawn_activity_sink(Arc::new(NoopSpawnActivity));
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["streamed_nested_adapter".into()], &[]),
            Arc::new(AllowApprover),
            ctx,
        );

        let result = executor
            .dispatch("streamed_nested_adapter", json!({}))
            .await;

        assert_eq!(result.content, "inherited");
    }

    #[derive(Default)]
    struct OneShotContextSpawner {
        seen: Mutex<Vec<(Option<String>, bool)>>,
    }

    #[async_trait]
    impl Spawner for OneShotContextSpawner {
        async fn spawn(
            &self,
            request: flux_runtime::SpawnRequest,
            cancel: &CancellationToken,
        ) -> CoreResult<flux_runtime::SpawnOutcome> {
            self.seen
                .lock()
                .unwrap()
                .push((request.parent_session, cancel.is_cancelled()));
            Ok(flux_runtime::SpawnOutcome {
                text: "direct child".into(),
                model: "mock".into(),
                session_id: "child-direct".into(),
                ..Default::default()
            })
        }
    }

    /// A-80 negative boundary: a top-level one-shot FlowClient has no served-request cancellation
    /// or parent lineage to inherit. Pinning the complete snapshot before `tokio::spawn` must not
    /// invent either one.
    #[tokio::test]
    async fn direct_one_shot_task_keeps_no_cancel_and_no_parent_behavior() {
        let spawner = Arc::new(OneShotContextSpawner::default());
        let mut client = FlowClient::builder()
            .auto_approve(true)
            .build(
                MockProvider::one("unused"),
                temp_root("direct-task-context"),
            )
            .unwrap();
        client.registry.register(Arc::new(TaskTool));
        client.spawner = Some(spawner.clone());
        assert!(
            client
                .build_executor()
                .context()
                .runtime_turn_context()
                .is_empty(),
            "a direct one-shot executor must start without live turn capabilities"
        );
        let ast: DraftAst = serde_json::from_value(json!({
            "body": [{
                "kind": "call",
                "op": "task",
                "args": [{
                    "kind": "lit",
                    "value": { "role": "worker", "task": "do it" }
                }]
            }]
        }))
        .unwrap();

        let result = client.execute_streamed(&ast).finish().await.unwrap();

        assert_eq!(result.result, "direct child");
        assert_eq!(spawner.seen.lock().unwrap().as_slice(), &[(None, false)]);
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
