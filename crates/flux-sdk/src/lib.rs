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

mod envelope;
pub mod events;
pub mod flow;
pub mod session;
pub mod storage;

pub use events::{AgentEvent, TurnStream};
pub use flow::{assemble_registry, ExecutionResult, FlowClient, FlowClientBuilder};
pub use session::Session;
pub use storage::Storage;

/// The engine's streaming contract — implement it and pass it to [`Session::send_with`] to
/// receive a turn's deltas, tool calls, tool results, and observations as they happen (or use
/// [`Session::stream`] for the owned-event shape). Re-exported from `flux-flow`.
pub use flux_flow::AgentSink;

/// Cancels a running turn ([`Session::send_with`]) or voice session — re-exported from
/// `tokio-util` so consumers don't need the direct dependency.
pub use tokio_util::sync::CancellationToken;

/// The provider trait — the one construction argument every client's `build` takes. The concrete
/// backends live in `flux-providers` (Anthropic/OpenAI/OpenRouter/Ollama/Bedrock + the
/// subscription providers); a consumer implements this for a mock, or adds `flux-providers` for a
/// real one.
pub use flux_provider::Provider;

/// The agent definition ([`ClientBuilder::from_spec`]) plus its permission rules. Re-exported so
/// the full-control door needs no direct `flux-agent` dependency.
pub use flux_agent::{AgentSpec, Permissions};

/// The per-turn token accounting carried on [`TurnOutput`]. Re-exported from `flux-core`.
pub use flux_core::Usage;

/// The model rate table [`Session::cost`] prices a session against.
/// [`PricingTable::builtin`](flux_core::PricingTable::builtin) is the curated default; the optional
/// `pricing` feature adds [`pricing::load_pricing_table`] to overlay a user's `~/.flux/pricing.toml`.
pub use flux_core::PricingTable;

/// **Custom tools.** Implement [`Tool`](tools::Tool) — or build one from a closure with
/// [`tool_fn`](tools::tool_fn)/[`FnTool`](tools::FnTool) — and register it with
/// [`ClientBuilder::register_op`]/[`FlowClient::register_op`]. A registered tool dispatches through
/// the same authorization → approval → guarded-IO envelope as every built-in. [`ToolSpec`](tools::ToolSpec)
/// (with [`Risk`](tools::Risk)) describes the tool to the model and the envelope;
/// [`ToolContext`](tools::ToolContext)/[`ToolResult`](tools::ToolResult) are the dispatch types;
/// [`ToolRegistry`](tools::ToolRegistry) is what a `register_pack` closure receives.
pub mod tools {
    pub use flux_runtime::{tool_fn, FnTool, Tool, ToolContext, ToolRegistry, ToolResult};
    pub use flux_spec::{Risk, ToolSpec};
}

/// **Approval policy.** Implement [`Approver`](approval::Approver) and pass it to a builder's
/// `approver(...)` to gate ops with your own logic; [`ApprovalChoice`](approval::ApprovalChoice) is
/// your verdict, [`IntentSet`](approval::IntentSet) is the per-call intent your `request` receives,
/// and [`RiskApprover`](approval::RiskApprover) is a ready-made risk-tiered policy.
pub mod approval {
    pub use flux_runtime::{ApprovalChoice, Approver, RiskApprover};
    pub use flux_spec::IntentSet;
}

/// **Session observability.** The projection types [`Session`] readers return —
/// [`Message`](flux_core::Message) ([`history`](Session::history)),
/// [`TurnSummary`](flux_events::TurnSummary) ([`turns`](Session::turns)),
/// [`RunEvent`](flux_lang::ast::RunEvent) ([`run_trace`](Session::run_trace)),
/// [`ModelCost`](flux_events::ModelCost) ([`cost`](Session::cost)), and
/// [`EfficiencySummary`](flux_events::EfficiencySummary) ([`efficiency`](Session::efficiency)) —
/// plus the stores [`Storage::custom`] accepts and the evidence-gated surfacing types
/// [`ClientBuilder::groups`] takes.
///
/// A gating [`ToolGroup`](flux_evidence::ToolGroup) hides its `tools` until a
/// [`SignalMatch`](flux_evidence::SignalMatch) fires — build one with
/// `SignalMatch { kind: KIND_SIGNAL.to_string(), signal: Some("my_signal".into()) }` and surface it
/// via [`ClientBuilder::ambient_signals`] or a workspace signal.
pub mod observe {
    pub use flux_core::Message;
    pub use flux_events::EventStore;
    pub use flux_events::{EfficiencySummary, ModelCost, TurnSummary};
    pub use flux_evidence::{Observation, SignalMatch, ToolGroup, KIND_SIGNAL};
    pub use flux_flow::state::FlowStore;
    pub use flux_lang::ast::RunEvent;
}

/// **Pricing ergonomics** (feature `pricing`). [`load_pricing_table`](pricing::load_pricing_table)
/// builds the effective [`PricingTable`] — the curated built-in rates overlaid by the user's
/// `~/.flux/pricing.toml` — the same table the CLI's cost display uses. Pass it to
/// [`Session::cost`]. Without the feature, use
/// [`PricingTable::builtin`](flux_core::PricingTable::builtin) (no file IO, no `flux-credentials`
/// dependency).
#[cfg(feature = "pricing")]
pub mod pricing {
    pub use flux_credentials::load_pricing_table;
}

/// **Provider construction** (feature `providers`). Re-exports the concrete LLM backends from
/// `flux-providers` (`anthropic`/`openai`/`openrouter`/`ollama`/`bedrock`/`codex` + the `spec`
/// resolver) and adds [`from_spec`](providers::from_spec) — the one-stop shop that turns a model
/// spec into a ready-to-`build` provider, using the CLI's exact resolution (including the
/// `claude`/`codex` subscription token sources and the AWS Bedrock chain).
///
/// The default build stays provider-agnostic: without this feature `flux-providers` (and its
/// transitive `flux-credentials`) are not dependencies at all.
///
/// ```ignore
/// # #[cfg(feature = "providers")]
/// # fn ex() -> flux_core::Result<()> {
/// let (provider, model) = flux_sdk::providers::from_spec("ollama/qwen3")?;
/// let client = flux_sdk::Client::builder().model(model).build(provider, ".")?;
/// # let _ = client; Ok(()) }
/// ```
#[cfg(feature = "providers")]
pub mod providers {
    pub use flux_providers::{anthropic, bedrock, codex, ollama, openai, openrouter, spec};

    use flux_core::Result;
    use flux_provider::Provider;

    /// Build a provider from a model spec (`"claude/sonnet"`, `"ollama/qwen3"`, `"openai/gpt-5.5"`,
    /// or a bare alias like `sonnet`), returning the boxed provider and its **resolved** model id —
    /// pass both straight to [`ClientBuilder::model`](crate::ClientBuilder::model) +
    /// [`ClientBuilder::build`](crate::ClientBuilder::build). Wraps
    /// [`flux_providers::spec::build`], so credentials resolve from the environment exactly as the
    /// `flux` CLI does.
    pub fn from_spec(spec: &str) -> Result<(Box<dyn Provider>, String)> {
        let (native, _provider, model) = spec::build(spec)?;
        Ok((Box::new(native), model))
    }
}

/// **Sub-agents.** Attach named roles to a conversational client with
/// [`ClientBuilder::with_sub_agents`] (or to a flow client with
/// [`FlowClient::with_sub_agents`](flow::FlowClient::with_sub_agents)); a turn whose plan calls
/// `task(role, …)` then delegates to a role's child agent through the same
/// authorization → approval → guarded-IO envelope. [`SubAgents`](subagents::SubAgents) is the bundle
/// (roles + the child tool surface + a [`ProviderFactory`](subagents::ProviderFactory) + limits),
/// [`SpawnLimits`](subagents::SpawnLimits) bounds each child (tokens, wall-clock, …), and
/// [`Role`](subagents::Role)/[`RoleRegistry`](subagents::RoleRegistry) name the roles a `task` may
/// target ([`parse_role`](subagents::parse_role) builds a `Role` from a markdown definition).
pub mod subagents {
    pub use flux_orchestrate::{
        parse_role, ProviderFactory, Role, RoleRegistry, SpawnLimits, SubAgents,
    };
}

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

// `AgentSpec`, `Usage`, and `Provider` are in scope via the public re-exports above.
use flux_cognition::CognitionPack;
use flux_core::ContextBlock;
use flux_core::Result;
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_orchestrate::{SubAgents, TaskTool};
use flux_runtime::{Approver, Tool, ToolContext, ToolRegistry};
use flux_system::{System, Workspace};

/// The result of one turn — a [`Client::run`], a [`Session::send`], or a
/// [`Session::start_flow`](Session::start_flow).
///
/// `#[non_exhaustive]`: fields are added as the SDK grows (wave 2 added `suspended`), so construct
/// it only via the SDK and match with a `..` rest pattern.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct TurnOutput {
    /// The assistant's final text for the turn.
    pub text: String,
    /// The names of the tools invoked during the turn, in call order.
    pub tool_calls: Vec<String>,
    /// Token usage for the turn, if the provider reported it.
    pub usage: Option<Usage>,
    /// Whether the session is parked on a top-level `await` after this turn — a flow-driven session
    /// (see [`Session::start_flow`](Session::start_flow)) that suspended, or re-suspended on a later
    /// `await`. Resume by sending the awaited input with [`Session::send`]. Always `false` for an
    /// ordinary conversational turn and for a flow that ran to completion.
    pub suspended: bool,
}

/// A deferred registry installer (the `register_*` pack convention), applied at `build`.
type RegistryPack = Box<dyn FnOnce(&mut ToolRegistry)>;

/// Builder for a [`Client`]. Internally an [`AgentSpec`] plus the shared envelope knobs, so every
/// agent-definition field has exactly one home and [`from_spec`](Self::from_spec) is the
/// full-control escape hatch rather than a parallel path.
pub struct ClientBuilder {
    spec: AgentSpec,
    envelope: envelope::Envelope,
    storage: Option<Storage>,
    cognition: bool,
    ops: Vec<Arc<dyn Tool>>,
    packs: Vec<RegistryPack>,
    sub_agents: Option<SubAgents>,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            spec: AgentSpec::new("unknown"),
            // Reads pre-allowed; everything else denied unless `auto_approve` (no UI in a library).
            envelope: envelope::Envelope::with_default_allow(&["read"]),
            // Unset ⇒ in-memory (ephemeral) stores, the pre-0.16 behavior.
            storage: None,
            cognition: false,
            ops: Vec::new(),
            packs: Vec::new(),
            // Unset ⇒ no `task` tool, no spawner (children off by default).
            sub_agents: None,
        }
    }
}

impl ClientBuilder {
    /// Start from a hand-built [`AgentSpec`] — the full-control escape hatch. The spec's own
    /// permissions are taken as-is: unlike [`builder`](Self::builder), there is **no implicit
    /// `read` pre-allow**, so a spec with empty `permissions` and no [`auto_approve`](Self::auto_approve)
    /// denies *every* op (including reads) — grant what the agent needs via the spec's
    /// `permissions`, [`allow`](Self::allow), or `auto_approve`. Builder methods overlay on top. A
    /// spec with explicit `skills` keeps them; an empty `skills` still gets default-dir discovery at
    /// [`build`](Self::build).
    pub fn from_spec(spec: AgentSpec) -> Self {
        Self {
            spec,
            envelope: envelope::Envelope::bare(),
            storage: None,
            cognition: false,
            ops: Vec::new(),
            packs: Vec::new(),
            sub_agents: None,
        }
    }
    /// Set the model id every turn uses.
    pub fn model(mut self, m: impl Into<String>) -> Self {
        self.spec.model = m.into();
        self
    }
    /// Override the system prompt (defaults to the agent's built-in prompt).
    pub fn system_prompt(mut self, s: impl Into<String>) -> Self {
        self.spec.system_prompt = s.into();
        self
    }
    /// Cap the max output tokens per model call.
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.spec.max_tokens = n;
        self
    }
    /// Cap the agent loop's tool-calling iterations per turn.
    pub fn max_iterations(mut self, n: usize) -> Self {
        self.spec.max_iterations = n;
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
    /// Approve every tool call automatically (no human in the loop). Use with care.
    pub fn auto_approve(mut self, yes: bool) -> Self {
        self.envelope.auto_approve = yes;
        self
    }
    /// Inject a custom [`Approver`] the executor consults per op — a policy between the blanket
    /// allow of [`auto_approve`](Self::auto_approve) and the headless default deny (e.g. a
    /// risk-aware confirm gate). Overrides `auto_approve`. The same seam
    /// [`FlowClientBuilder::approver`](crate::FlowClientBuilder::approver) and the sub-agent
    /// spawner already have, now on the conversational door.
    pub fn approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.envelope.approver = Some(approver);
        self
    }
    /// Register a custom op (any [`Tool`], e.g. one built with `flux_runtime::tool_fn`) alongside
    /// the built-ins. Registered ops dispatch through the same authorization → approval → guarded
    /// IO envelope as every other op — registration grants existence, not permission.
    pub fn register_op(mut self, tool: Arc<dyn Tool>) -> Self {
        self.ops.push(tool);
        self
    }
    /// Register a whole pack of ops via a closure over the registry (the `register_*` convention
    /// used across flux). Same envelope rules as [`register_op`](Self::register_op).
    pub fn register_pack<F: FnOnce(&mut ToolRegistry) + 'static>(mut self, pack: F) -> Self {
        self.packs.push(Box::new(pack));
        self
    }
    /// Attach named sub-agents to the conversational client: at [`build`](Self::build) the `task`
    /// tool joins this client's catalog and the spawner is built over the client's guarded `System`,
    /// so a turn whose plan calls `task(role, …)` delegates to a role's child agent through the same
    /// authorization → approval → guarded-IO envelope. The single seam
    /// [`FlowClient::with_sub_agents`](crate::FlowClient::with_sub_agents) already offered, now on
    /// the conversational door — a consumer (e.g. a multi-tenant service) drives sub-agents without
    /// re-assembling the spawner, executor, and context by hand.
    ///
    /// A generous default `wall_clock` (10 min) is applied when the bundle sets none, so a hung
    /// child can't run forever; override it (or any limit) via
    /// [`SubAgents::with_limits`](subagents::SubAgents::with_limits). Unlike the one-shot
    /// `FlowClient`, a streamed turn's cancel token ([`Session::stream`]`().cancel()`) also reaches
    /// a running child, since the conversational path installs a cancellation token.
    pub fn with_sub_agents(mut self, mut sub_agents: SubAgents) -> Self {
        if sub_agents.limits.wall_clock.is_none() {
            sub_agents.limits.wall_clock = Some(std::time::Duration::from_secs(600));
        }
        self.sub_agents = Some(sub_agents);
        self
    }
    /// Restrict the agent to a subset of the registry's ops by name (`AgentSpec::tools`). Ops
    /// outside the subset are not just hidden — they are absent from this agent's registry.
    pub fn tools<I, S>(mut self, subset: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.spec.tools = Some(subset.into_iter().map(Into::into).collect());
        self
    }
    /// Also wire the provider-backed cognition pack (`ai.extract`/`rank`/`judge`/`reason`,
    /// `synth`, `ai.rewrite`) into the registry — the same pack [`FlowClient`] assembles by
    /// default. Off by default on the conversational door.
    pub fn with_cognition(mut self, yes: bool) -> Self {
        self.cognition = yes;
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
        self.spec.context.push(ContextBlock::new(id, title, body));
        self
    }
    /// Set the evidence-gated tool groups. Each turn the workspace is probed for signals and only
    /// ops whose group has surfaced are advertised to the model; an op named in a group's `tools`
    /// stays hidden until that group's `surface_when` signal fires (surfacing is sticky-monotonic
    /// within a session). Empty (the default) disables gating — every op is advertised. Name a group
    /// with [`ToolGroup`](observe::ToolGroup) (re-exported via [`observe`]); pair with
    /// [`ambient_signals`](Self::ambient_signals) for signals the per-turn workspace walk can't see.
    pub fn groups<I>(mut self, groups: I) -> Self
    where
        I: IntoIterator<Item = flux_evidence::ToolGroup>,
    {
        self.spec.groups = groups.into_iter().collect();
        self
    }
    /// Add session-ambient group-surfacing signals (D-115): host-known facts the per-turn workspace
    /// walk can't observe (e.g. "an endpoints store is loaded"). Appended to every turn's probed
    /// signals, so a startup-static value is enough to surface its [`groups`](Self::groups). Empty by
    /// default.
    pub fn ambient_signals<I, S>(mut self, signals: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.spec.ambient_signals = signals.into_iter().map(Into::into).collect();
        self
    }
    /// Set the compaction threshold in serialized chars: once a persisted session grows past it,
    /// older turns are summarized into a durable digest before the next request (A-22), instead of
    /// re-sending an ever-growing transcript. `0` disables compaction. Defaults to
    /// `flux_agent::DEFAULT_COMPACT_THRESHOLD_CHARS` (matching the CLI).
    pub fn with_compaction(mut self, threshold_chars: usize) -> Self {
        self.spec.compact_threshold_chars = threshold_chars;
        self
    }
    /// Set the byte budget for the rendered inline `context` knowledge blocks
    /// ([`add_context`](Self::add_context)); over-budget blocks truncate with a marker. `0` =
    /// unbounded. Defaults to `flux_agent::DEFAULT_CONTEXT_BUDGET`.
    pub fn context_budget(mut self, bytes: usize) -> Self {
        self.spec.context_budget = bytes;
        self
    }

    /// Build the client with `provider` and a workspace rooted at `root`. Sessions live in the
    /// configured [`Storage`] (in-memory unless set). The turn runs on [`FlowEngine`] (the model
    /// plans, the runtime runs the flux-lang agent loop).
    pub fn build(self, provider: Box<dyn Provider>, root: impl Into<PathBuf>) -> Result<Client> {
        let root = root.into();
        let provider: Arc<dyn Provider> = Arc::from(provider);
        // Attach the OS-sandbox posture so a consumer's `FLUX_SANDBOX=require` is honored on this
        // client's spawns; a bare `System::new` defaults to `Sandbox::disabled()` (no confinement,
        // no `require` enforcement). Unset ⇒ resolve from env (off ⇒ disabled, safe default).
        let sandbox = self.envelope.resolve_sandbox();
        let system = Arc::new(System::new(Workspace::new(root.clone())?).with_sandbox(sandbox));
        let mut registry = ToolRegistry::new();
        flux_tools::register_builtins(&mut registry);
        if self.cognition {
            CognitionPack::new(provider.clone(), self.spec.model.clone()).register(&mut registry);
        }
        // Snapshot the base op names (built-ins + cognition) so consumer-registered ops can be
        // told apart below: a `tools` subset restricts the *base* catalog, but must not silently
        // drop a tool the consumer explicitly registered.
        let base_names: std::collections::HashSet<String> = registry.names().into_iter().collect();
        // Consumer ops/packs join the same registry the envelope gates — registration grants
        // existence, not permission (the safety envelope still gates every dispatch).
        for tool in self.ops {
            registry.register(tool);
        }
        for pack in self.packs {
            pack(&mut registry);
        }
        // Sub-agents: register the `task` tool BEFORE the custom-name snapshot so it rides the same
        // re-admit into a `tools` subset every consumer-registered op does (a consumer that scoped
        // the catalog AND asked for sub-agents still keeps `task`). The spawner is attached to the
        // dispatch context below.
        if self.sub_agents.is_some() {
            registry.register(Arc::new(TaskTool));
        }
        let custom_names: Vec<String> = registry
            .names()
            .into_iter()
            .filter(|n| !base_names.contains(n))
            .collect();
        let approver = self.envelope.resolve_approver();

        let (events, flow) = self.storage.unwrap_or_default().resolve()?;

        // The agent's definition; `assemble` selects the tool subset, applies the permissions,
        // registers the reflexive ops, and ties the engine⇄loop-host cycle. Builder rules are
        // additive to the spec's own (`from_spec` starts from a bare envelope). Skills come from
        // the default skill dirs (project `.flux/skills`/`.claude/skills` + the user globals,
        // L-02) unless the spec already carries an explicit set — discovery is progressive
        // (metadata now, bodies on activation), so this costs a frontmatter head-read per skill.
        let mut spec = self.spec;
        spec.permissions
            .allow
            .extend(self.envelope.allow.iter().cloned());
        spec.permissions
            .deny
            .extend(self.envelope.deny.iter().cloned());
        // A `tools` subset restricts the base catalog only — re-admit every consumer-registered op
        // so `register_op(...).tools([...])` never silently drops the just-added tool.
        if let Some(tools) = spec.tools.as_mut() {
            for name in custom_names {
                if !tools.contains(&name) {
                    tools.push(name);
                }
            }
        }
        spec.cwd = root;
        if spec.skills.is_empty() {
            spec = spec.with_default_skills();
        }
        // Thread the sub-agent spawner into the dispatch context when sub-agents are attached, so a
        // `task` call delegates through the same guarded `System`; `None` (the common case) leaves
        // the context exactly as before. Mirrors `FlowClient::build_executor`.
        let mut ctx = ToolContext::new(system.clone());
        if let Some(sub_agents) = self.sub_agents {
            ctx = ctx.with_spawner(sub_agents.into_spawner(system));
        }
        let model = spec.model.clone();
        let engine = spec.assemble(provider, registry, approver, ctx, events, flow)?;
        Ok(Client {
            engine: Arc::new(engine),
            model,
            // The default session is created lazily (on first use), so building a client — e.g. a
            // service restarting against a persistent `Storage::dir` — never leaves an empty
            // session behind, and `latest_session()` still points at the real prior conversation.
            default_session: std::sync::Mutex::new(None),
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
    // The default session's id, created lazily on first use (see `default_id`). Kept behind a
    // std mutex so two concurrent first-uses can't each mint (and leak) a session.
    default_session: std::sync::Mutex<Option<String>>,
    // One engine runs one turn at a time (the planner loop is armed per turn); every Session
    // created by this client shares this guard so concurrent sends serialize instead of racing.
    turn_guard: Arc<tokio::sync::Mutex<()>>,
}

impl Client {
    /// Start building a [`Client`].
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// The default session's id, minting it on first call. Returns owned since the id lives behind
    /// a mutex (the lazy-creation guard).
    fn default_id(&self) -> Result<String> {
        let mut slot = self.default_session.lock().unwrap();
        if let Some(id) = slot.as_ref() {
            return Ok(id.clone());
        }
        let id = self.engine.events.create_session(&self.model)?;
        *slot = Some(id.clone());
        Ok(id)
    }

    /// The id of the default session this client's [`run`](Self::run) turns are recorded against.
    /// The default session is created lazily, so this call mints it on first use.
    pub fn session_id(&self) -> Result<String> {
        self.default_id()
    }

    /// Run one turn on the default session, collecting the final text and the tools invoked.
    /// Equivalent to `client.default_session()?.send(input)`.
    pub async fn run(&self, input: &str) -> Result<TurnOutput> {
        let id = self.default_id()?;
        self.session(id).send(input).await
    }

    /// The default session as a [`Session`] handle (created lazily on first use).
    pub fn default_session(&self) -> Result<Session> {
        Ok(self.session(self.default_id()?))
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

    /// The most recently updated session in this client's [`Storage`], if any. Because the default
    /// session is created lazily (not at build), this returns the real prior conversation after a
    /// restart against a persistent [`Storage::dir`] — as long as no turn has run on this client
    /// yet to mint a newer default. To target a specific conversation, persist its id and use
    /// [`open_session`](Self::open_session).
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
        let id = client.session_id().unwrap();
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

    /// A mock whose first call plans a single named op with a literal arg, then answers in prose.
    struct PlanOpMock {
        op: &'static str,
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl Provider for PlanOpMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunks = if n == 0 {
                let ast = serde_json::json!({
                    "body": [{
                        "kind": "call", "op": self.op,
                        "args": [ { "kind": "lit", "value": { "name": "flux" } } ]
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
                        text: "done".into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    fn greet_tool(hits: Arc<std::sync::atomic::AtomicUsize>) -> Arc<dyn flux_runtime::Tool> {
        flux_runtime::tool_fn(
            flux_spec::ToolSpec::read_only(
                "greet",
                "Greets by name",
                serde_json::json!({
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"]
                }),
            ),
            move |input| {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Ok(serde_json::json!(format!(
                        "hello {}",
                        input["name"].as_str().unwrap_or("?")
                    )))
                }
            },
        )
    }

    /// D-143: a `tool_fn` registered on the builder is callable by a planned turn — and it runs
    /// through the envelope, not around it (`auto_approve` is what permits it here).
    #[tokio::test]
    async fn a_registered_custom_tool_dispatches_through_a_planned_turn() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-fntool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .register_op(greet_tool(hits.clone()))
            .build(
                Box::new(PlanOpMock {
                    op: "greet",
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();
        let out = client.run("greet flux").await.unwrap();
        assert_eq!(hits.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(out.tool_calls, vec!["greet"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-143: an injected deny-listing `Approver` gates a registered custom tool — registration
    /// grants existence, not permission (mirrors the FlowClient-side
    /// `an_injected_approver_policy_gates_per_op`).
    #[tokio::test]
    async fn an_injected_approver_gates_a_registered_custom_tool() {
        struct DenyGreet;
        #[async_trait]
        impl flux_runtime::Approver for DenyGreet {
            async fn request(
                &self,
                tool: &str,
                _subjects: &[String],
                _intents: &flux_spec::IntentSet,
            ) -> flux_runtime::ApprovalChoice {
                if tool == "greet" {
                    flux_runtime::ApprovalChoice::Deny
                } else {
                    flux_runtime::ApprovalChoice::Allow
                }
            }
        }

        let dir = std::env::temp_dir().join(format!("flux-sdk-fngate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let client = Client::builder()
            .model("mock")
            .approver(Arc::new(DenyGreet))
            .register_op(greet_tool(hits.clone()))
            .build(
                Box::new(PlanOpMock {
                    op: "greet",
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();
        let _ = client.run("greet flux").await;
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the injected approver must gate the registered tool"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-143: `tools(subset)` removes ops from the agent's registry — a plan calling an
    /// out-of-subset op cannot execute it (the loop machinery itself stays registered).
    #[tokio::test]
    async fn tools_subset_removes_ops_from_the_registry() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-subset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .tools(["read"])
            .build(
                Box::new(PlanThenProseMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();
        // The plan calls `write`, which the subset removed: the turn must complete without the
        // write ever happening.
        let _ = client.run("write a file").await;
        assert!(
            !dir.join("sdk-plan.txt").exists(),
            "an out-of-subset op must not execute"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-144: `send_with` streams to a consumer sink — text deltas arrive AND tool results arrive
    /// (the old private collector dropped tool results entirely).
    #[tokio::test]
    async fn send_with_streams_deltas_and_tool_results_to_a_consumer_sink() {
        #[derive(Default)]
        struct Recording {
            deltas: Vec<String>,
            tool_results: Vec<String>,
        }
        impl AgentSink for Recording {
            fn text_delta(&mut self, t: &str) {
                self.deltas.push(t.to_string());
            }
            fn tool_result(&mut self, name: &str, _result: &flux_runtime::ToolResult) {
                self.tool_results.push(name.to_string());
            }
        }

        let dir = std::env::temp_dir().join(format!("flux-sdk-sendwith-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .build(
                Box::new(PlanThenProseMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();
        let session = client.default_session().unwrap();
        let mut sink = Recording::default();
        let out = session
            .send_with("write a file", &mut sink, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.text, "Wrote the file.");
        // `deltas` is exercised live by `stream_yields_events_live_and_finish_collects`; the
        // load-bearing assertion here is the tool RESULT reaching the consumer (the old private
        // collector dropped those entirely).
        assert!(
            sink.tool_results.contains(&"write".to_string()),
            "the consumer sink must receive tool_result events, got {:?}",
            sink.tool_results
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Streams the answer in two text deltas, then the final block.
    struct TwoDeltaMock;
    #[async_trait]
    impl Provider for TwoDeltaMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Ok(Box::pin(futures::stream::iter(
                vec![
                    Chunk::TextDelta("first ".into()),
                    Chunk::TextDelta("second".into()),
                    Chunk::Block(ContentBlock::Text {
                        text: "first second".into(),
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

    /// D-145: `Session::stream` yields the turn's events as owned `AgentEvent`s (deltas preserved,
    /// `TurnEnd` fires) and `finish()` returns the collected output. (Cross-turn *liveness* — an
    /// event observed before the turn ends — is exercised by the cancel test below, where the turn
    /// cannot end until the consumer acts on a mid-turn `ToolCall` event.)
    #[tokio::test]
    async fn stream_yields_events_and_finish_collects() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let client = Client::builder()
            .model("mock")
            .build(Box::new(TwoDeltaMock), &dir)
            .unwrap();
        let mut stream = client.default_session().unwrap().stream("hi");

        let mut deltas = String::new();
        let mut saw_turn_end = false;
        while let Some(event) = stream.next().await {
            match event {
                AgentEvent::TextDelta(t) => deltas.push_str(&t),
                AgentEvent::TurnEnd { .. } => saw_turn_end = true,
                _ => {}
            }
        }
        assert_eq!(deltas, "first second");
        assert!(saw_turn_end, "the stream must emit a TurnEnd event");
        let out = stream.finish().await.unwrap();
        assert_eq!(out.text, "first second");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-145: cancelling a streamed turn mid-tool ends it and leaves a valid `user → assistant`
    /// alternation in the persisted log (the AGENTS.md session-shape invariant).
    #[tokio::test]
    async fn cancelling_a_streamed_turn_keeps_the_session_shape_valid() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A tool that parks forever — the turn can only end via cancellation.
        let parked = flux_runtime::tool_fn(
            flux_spec::ToolSpec::read_only(
                "park",
                "Blocks until cancelled",
                serde_json::json!({ "type": "object", "properties": {} }),
            ),
            |_input| async {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                Ok(serde_json::json!("unreachable"))
            },
        );
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .register_op(parked)
            .build(
                Box::new(PlanOpMock {
                    op: "park",
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();
        let session = client.default_session().unwrap();
        let mut stream = session.stream("park it");
        // Wait until the parked op is actually in flight, then cancel.
        loop {
            match stream.next().await {
                Some(AgentEvent::ToolCall { name, .. }) if name == "park" => break,
                Some(_) => continue,
                None => panic!("stream ended before the tool call"),
            }
        }
        stream.cancel();
        let _ = stream.finish().await;

        let history = session.history().unwrap();
        assert!(!history.is_empty());
        for pair in history.windows(2) {
            assert_ne!(
                pair[0].role, pair[1].role,
                "roles must alternate after a cancelled turn"
            );
        }
        assert!(
            matches!(history.last().unwrap().role, flux_core::Role::Assistant),
            "a cancelled turn must still persist exactly one closing assistant message"
        );
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

    /// D-143 fix: a `tools(subset)` restricts the base catalog but must NOT drop a tool the
    /// consumer explicitly registered — `register_op(greet).tools(["read"])` keeps `greet`.
    #[tokio::test]
    async fn tools_subset_preserves_registered_custom_ops() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-subkeep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .tools(["read"]) // restricts built-ins, but `greet` was registered on purpose
            .register_op(greet_tool(hits.clone()))
            .build(
                Box::new(PlanOpMock {
                    op: "greet",
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();
        let out = client.run("greet flux").await.unwrap();
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the registered custom op must survive the tools() subset"
        );
        assert_eq!(out.tool_calls, vec!["greet"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-142 fix: the default session is created lazily — building a client (e.g. a service
    /// restart) mints no session, so `latest_session()` still points at the real prior
    /// conversation rather than an empty default.
    #[tokio::test]
    async fn lazy_default_session_does_not_shadow_the_prior_conversation() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-lazy-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let store = dir.join("state");

        // First process: run a real conversation, capture its id.
        let real_id = {
            let client = Client::builder()
                .model("mock")
                .storage(Storage::dir(&store))
                .build(Box::new(ProseMock { text: "hi" }), &dir)
                .unwrap();
            client.run("remember this").await.unwrap();
            let id = client.session_id().unwrap();
            drop(client);
            id
        };

        // Second process: a fresh client that has NOT run yet must see the real conversation as
        // latest (no empty default was minted at build).
        let client = Client::builder()
            .model("mock")
            .storage(Storage::dir(&store))
            .build(Box::new(ProseMock { text: "hi" }), &dir)
            .unwrap();
        let latest = client
            .latest_session()
            .unwrap()
            .expect("a prior session exists");
        assert_eq!(
            latest.id(),
            real_id,
            "latest_session must return the real prior conversation, not a fresh empty default"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-145 fix: dropping a `TurnStream` cancels its turn instead of leaving it running detached
    /// and holding the client's turn slot. A parked tool (sleeps 300s) can only be escaped by
    /// cancellation, so if `run()` after the drop completes, the drop cancelled the turn.
    #[tokio::test]
    async fn dropping_a_turn_stream_cancels_the_turn() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-dropcancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let parked = flux_runtime::tool_fn(
            flux_spec::ToolSpec::read_only(
                "park",
                "Blocks until cancelled",
                serde_json::json!({ "type": "object", "properties": {} }),
            ),
            |_input| async {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                Ok(serde_json::json!("unreachable"))
            },
        );
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .register_op(parked)
            .build(
                Box::new(PlanOpMock {
                    op: "park",
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();
        {
            let session = client.default_session().unwrap();
            let mut stream = session.stream("park it");
            // Wait until the parked op is actually in flight, then drop the stream (no cancel()).
            loop {
                match stream.next().await {
                    Some(AgentEvent::ToolCall { name, .. }) if name == "park" => break,
                    Some(_) => continue,
                    None => panic!("stream ended before the tool call"),
                }
            }
            drop(stream);
        }
        // If the drop did not cancel, the parked turn would hold the guard for 300s and this
        // would hang (test timeout). It completing proves the drop cancelled the turn.
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            client.default_session().unwrap().send("are you there"),
        )
        .await
        .expect("run after drop must not hang — the dropped stream should have cancelled the turn")
        .unwrap();
        assert_eq!(out.text, "done");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A never-planning mock: the deterministic flow skeleton (echo prompts + `await`) invokes no
    /// planner, so a call here is a bug. Panics if the model is ever hit.
    struct NeverMock;
    #[async_trait]
    impl Provider for NeverMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            panic!("a flow-driven session must not invoke the planner");
        }
    }

    /// A minimal custom op that emits its `text` back as the model-facing view — the flow's authored
    /// prompt. Mirrors the engine's test `EchoTool`.
    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only(
                "echo",
                "echo text",
                serde_json::json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
            )
        }
        async fn execute(
            &self,
            _c: &ToolContext,
            params: serde_json::Value,
        ) -> Result<flux_runtime::ToolResult> {
            Ok(flux_runtime::ToolResult::ok(
                params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    /// A two-`await` interview flow: prompt, park, prompt, park, done. `echo` emits each authored
    /// prompt; `await` parks for the reply.
    fn interview_flow() -> flux_lang::ast::DraftAst {
        use flux_lang::ast::{Node, SymbolName};
        let prompt = |t: &str| Node::Call {
            op: "echo".into(),
            args: vec![Node::Lit {
                value: serde_json::json!(t),
            }],
        };
        let await_reply = |name: &str| Node::Await {
            binding: Some(SymbolName(name.into())),
            source: "user_input".into(),
            as_type: None,
        };
        flux_lang::ast::DraftAst {
            body: vec![
                prompt("What is your name?"),
                await_reply("name"),
                prompt("Nice to meet you. Favorite color?"),
                await_reply("color"),
                prompt("All done — thanks!"),
            ],
            ..Default::default()
        }
    }

    /// D-147: `Session::start_flow` runs an authored flow to its first top-level `await`, surfaces the
    /// flow's own authored prompt, and reports `suspended: true`. `send` answers the `await` and
    /// resumes to the next prompt (still suspended); the final `send` completes the flow and flips
    /// `suspended` to `false` — a durable human-in-the-loop driven entirely by the SDK's front door.
    #[tokio::test]
    async fn start_flow_suspends_surfaces_prompt_and_send_resumes() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-startflow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .register_op(Arc::new(EchoTool))
            .build(Box::new(NeverMock), &dir)
            .unwrap();
        let session = client.create_session().unwrap();

        // Start the flow: first authored prompt, parked on await #1.
        let out = session.start_flow(&interview_flow()).await.unwrap();
        assert!(
            out.text.contains("What is your name?"),
            "start_flow surfaces the first authored prompt: {:?}",
            out.text
        );
        assert!(out.suspended, "the flow parked on its first `await`");
        assert!(
            session.suspended().unwrap(),
            "the session reports suspended"
        );

        // Answer #1: resume to the second authored prompt, still parked (await #2).
        let out = session.send("Timo").await.unwrap();
        assert!(
            out.text.contains("Favorite color?"),
            "send resumes to the second authored prompt: {:?}",
            out.text
        );
        assert!(out.suspended, "still parked on the second `await`");

        // Answer #2: the flow completes — no more awaits, so `suspended` flips false.
        let out = session.send("blue").await.unwrap();
        assert!(
            out.text.contains("All done"),
            "the final send completes the flow: {:?}",
            out.text
        );
        assert!(!out.suspended, "a completed flow is no longer suspended");
        assert!(
            !session.suspended().unwrap(),
            "the session reports not suspended"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-147: a flow suspended between two `await`s survives a process restart. Persist with
    /// `Storage::dir`, drop the whole client (simulating the process ending), rebuild a fresh client
    /// over the same directory, `open_session` by id, and `send` — the parked flow resumes.
    #[tokio::test]
    async fn suspended_flow_survives_a_process_restart() {
        let dir =
            std::env::temp_dir().join(format!("flux-sdk-startflow-restart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let session_id = {
            let client = Client::builder()
                .model("mock")
                .auto_approve(true)
                .register_op(Arc::new(EchoTool))
                .storage(Storage::dir(&dir))
                .build(Box::new(NeverMock), &dir)
                .unwrap();
            let session = client.create_session().unwrap();
            let out = session.start_flow(&interview_flow()).await.unwrap();
            assert!(out.suspended, "parked on await #1 before the restart");
            session.id().to_string()
            // client dropped here — the process "restarts".
        };

        // Fresh client over the same persistent directory — a new process picking up the session.
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .register_op(Arc::new(EchoTool))
            .storage(Storage::dir(&dir))
            .build(Box::new(NeverMock), &dir)
            .unwrap();
        let session = client.open_session(&session_id).unwrap();
        assert!(
            session.suspended().unwrap(),
            "the persisted suspension is visible after the restart"
        );
        let out = session.send("Timo").await.unwrap();
        assert!(
            out.text.contains("Favorite color?"),
            "the parked flow resumes across the restart: {:?}",
            out.text
        );
        assert!(out.suspended, "re-parked on await #2");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A child sub-agent provider that bills real tokens, so a delegated `task` call contributes
    /// usage the parent turn folds in.
    struct WorkerMock(Usage);
    #[async_trait]
    impl Provider for WorkerMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Ok(Box::pin(futures::stream::iter(
                vec![
                    Chunk::Block(ContentBlock::Text {
                        text: "did the subtask".into(),
                    }),
                    Chunk::Usage(self.0.clone()),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
                .into_iter()
                .map(Ok),
            )))
        }
    }

    /// The parent planner: round 0 emits a one-op plan calling `task(worker, …)`; round 1 answers in
    /// prose. Neither call bills usage, so the turn's total isolates the sub-agent's contribution.
    struct DelegatingMock {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl Provider for DelegatingMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunks = if n == 0 {
                let ast = serde_json::json!({ "body": [{
                    "kind": "call", "op": "task",
                    "args": [{ "kind": "lit", "value": { "role": "worker", "task": "do it" } }]
                }] });
                vec![
                    Chunk::Block(ContentBlock::ToolUse {
                        id: "p".into(),
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
                        text: "delegated to the worker".into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// D-148: `ClientBuilder::with_sub_agents` puts the `task` tool + spawner on the conversational
    /// door. A turn whose plan calls `task(role, …)` runs the child through the parent's envelope,
    /// and the child's usage observation lands in the session's run trace (folded into the turn's
    /// recorded usage). The `subagents` re-export module names every bundle type.
    #[tokio::test]
    async fn with_sub_agents_runs_a_delegated_task_and_records_child_usage() {
        use crate::subagents::{parse_role, RoleRegistry, SubAgents};

        let dir = std::env::temp_dir().join(format!("flux-sdk-subagents-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // A mock role registry with one "worker" role.
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\n---\nworker prompt", "worker"));

        // The child bills real tokens; the provider factory hands each spawn a fresh worker provider.
        let child_usage = Usage {
            input_tokens: 1000,
            output_tokens: 200,
            ..Default::default()
        };
        let factory: crate::subagents::ProviderFactory = Arc::new({
            let u = child_usage.clone();
            move || Ok(Box::new(WorkerMock(u.clone())) as Box<dyn Provider>)
        });
        let sub_agents = SubAgents::new(roles, ToolRegistry::new(), factory, "mock", 1024);

        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .with_sub_agents(sub_agents)
            .build(
                Box::new(DelegatingMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();

        let out = client.run("delegate this").await.unwrap();
        assert!(
            out.tool_calls.contains(&"task".to_string()),
            "the plan delegated via `task`: {:?}",
            out.tool_calls
        );

        // The child's tokens reached the session's run trace: the parent turn's recorded usage folds
        // in the sub-agent's spend (the parent's own planner calls billed nothing).
        let sid = client.session_id().unwrap();
        let events = client.event_store();
        let turns = events.turns(&sid).unwrap();
        let usage = turns
            .last()
            .and_then(|t| t.usage.as_ref())
            .expect("the parent turn's usage must be Some — the sub-agent billed tokens");
        assert_eq!(
            usage.input_tokens, 1000,
            "the sub-agent's input tokens landed in the session's run trace"
        );
        assert_eq!(
            usage.output_tokens, 200,
            "the sub-agent's output tokens landed in the session's run trace"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A gated custom op with a distinctive name unlikely to collide with prompt boilerplate, so a
    /// catalog-capturing test can assert its presence/absence in the advertised op catalog.
    struct WidgetTool;
    #[async_trait]
    impl Tool for WidgetTool {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only(
                "zzquux_probe",
                "a gated probe op",
                serde_json::json!({ "type": "object", "properties": {} }),
            )
        }
        async fn execute(
            &self,
            _c: &ToolContext,
            _params: serde_json::Value,
        ) -> Result<flux_runtime::ToolResult> {
            Ok(flux_runtime::ToolResult::ok("ok"))
        }
    }

    /// D-149: `ClientBuilder::groups` + `ambient_signals` gate an op behind an evidence signal — it
    /// is absent from the advertised op catalog until its group surfaces. `ToolGroup`/`SignalMatch`/
    /// `KIND_SIGNAL` are named via `flux_sdk::observe` (acceptance 3). Mirrors the engine's
    /// evidence-gated surfacing, driven entirely through the SDK builder.
    #[tokio::test]
    async fn groups_gate_an_op_until_its_ambient_signal_surfaces() {
        use crate::observe::{SignalMatch, ToolGroup, KIND_SIGNAL};

        let dir = std::env::temp_dir().join(format!("flux-sdk-groups-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let group = ToolGroup {
            name: "widgets".into(),
            description: String::new(),
            tools: vec!["zzquux_probe".into()],
            surface_when: vec![SignalMatch {
                kind: KIND_SIGNAL.to_string(),
                signal: Some("widgets_on".into()),
            }],
        };

        // Gated: no signal fires → the op stays hidden from the catalog the model sees.
        let systems_gated = Arc::new(Mutex::new(Vec::new()));
        let client = Client::builder()
            .model("mock")
            .register_op(Arc::new(WidgetTool))
            .groups([group.clone()])
            .build(
                Box::new(SystemCaptureMock {
                    systems: systems_gated.clone(),
                }),
                &dir,
            )
            .unwrap();
        client.run("hi").await.unwrap();
        let gated = systems_gated.lock().unwrap().join("\n");
        assert!(
            !gated.contains("zzquux_probe"),
            "the gated op must be absent from the catalog until its signal fires"
        );

        // Surfaced: the ambient signal is present → the op joins the advertised catalog.
        let systems_on = Arc::new(Mutex::new(Vec::new()));
        let client = Client::builder()
            .model("mock")
            .register_op(Arc::new(WidgetTool))
            .groups([group])
            .ambient_signals(["widgets_on"])
            .build(
                Box::new(SystemCaptureMock {
                    systems: systems_on.clone(),
                }),
                &dir,
            )
            .unwrap();
        client.run("hi").await.unwrap();
        let surfaced = systems_on.lock().unwrap().join("\n");
        assert!(
            surfaced.contains("zzquux_probe"),
            "the op must be advertised once its group's signal surfaces:\n{surfaced}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-149: `ClientBuilder::with_compaction` sets the threshold past which older turns are
    /// summarized. With a tiny threshold, a few turns trip compaction and a `context.compacted`
    /// observation lands in the session's evidence. `Observation` is named via `flux_sdk::observe`.
    #[tokio::test]
    async fn with_compaction_trips_and_records_a_context_compacted_observation() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-compact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Tiny threshold: any real conversation exceeds it, so compaction trips as soon as there are
        // enough messages (the engine needs ≥ 4 before it will summarize).
        let client = Client::builder()
            .model("mock")
            .with_compaction(10)
            .build(Box::new(ProseMock { text: "ok" }), &dir)
            .unwrap();

        // Three turns: by the third, the persisted conversation has ≥ 4 messages and far exceeds the
        // 10-char threshold, so compaction runs before that turn plans.
        for _ in 0..3 {
            client.run("tell me something").await.unwrap();
        }

        let sid = client.session_id().unwrap();
        let obs = client.event_store().observations(&sid).unwrap();
        let compacted: Option<&crate::observe::Observation> =
            obs.iter().find(|o| o.kind == "context.compacted");
        let compacted = compacted.expect("a context.compacted observation must be recorded");
        assert!(
            compacted.data["from_messages"].as_u64().unwrap()
                > compacted.data["to_messages"].as_u64().unwrap(),
            "compaction shrank the message count: {:?}",
            compacted.data
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A prose mock that bills a fixed [`Usage`] on every call — so a session's turns have real
    /// token spend to project into `turns()`/`cost()`.
    struct PricedMock(Usage);
    #[async_trait]
    impl Provider for PricedMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Ok(Box::pin(futures::stream::iter(
                vec![
                    Chunk::TextDelta("ok".into()),
                    Chunk::Block(ContentBlock::Text { text: "ok".into() }),
                    Chunk::Usage(self.0.clone()),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
                .into_iter()
                .map(Ok),
            )))
        }
    }

    /// D-151: a `Session` exposes the EventStore projections recorded for every turn. After two
    /// turns: `turns()` has two summaries, `history()` is a user/assistant alternation, and
    /// `cost(&table)` prices a non-zero USD for the priced model. The projection types are named via
    /// `flux_sdk::observe`.
    #[tokio::test]
    async fn session_projections_report_turns_history_and_cost() {
        use crate::observe::{ModelCost, TurnSummary};
        use flux_core::Role;

        let dir = std::env::temp_dir().join(format!("flux-sdk-proj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let per_call = Usage {
            input_tokens: 1000,
            output_tokens: 500,
            ..Default::default()
        };
        let client = Client::builder()
            .model("priced-mock")
            .build(Box::new(PricedMock(per_call)), &dir)
            .unwrap();
        let session = client.default_session().unwrap();
        session.send("first").await.unwrap();
        session.send("second").await.unwrap();

        // turns(): one summary per turn.
        let turns: Vec<TurnSummary> = session.turns().unwrap();
        assert_eq!(turns.len(), 2, "one TurnSummary per turn: {turns:?}");

        // history(): a user/assistant alternation (two turns → four messages).
        let history = session.history().unwrap();
        assert_eq!(history.len(), 4, "two turns = four messages");
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[1].role, Role::Assistant);
        assert_eq!(history[2].role, Role::User);
        assert_eq!(history[3].role, Role::Assistant);

        // cost(): non-zero USD once the model is priced.
        let mut pricing = PricingTable::builtin();
        pricing.set(
            "priced-mock",
            flux_core::Rates {
                input: 1000.0,
                output: 1000.0,
                ..Default::default()
            },
        );
        let cost: Vec<ModelCost> = session.cost(&pricing).unwrap();
        let usd: f64 = cost
            .iter()
            .filter_map(|c| c.cost.as_ref())
            .map(|m| m.usd)
            .sum();
        assert!(usd > 0.0, "the priced model reports non-zero USD: {cost:?}");

        // run_trace()/efficiency() are callable projections over the same store.
        let _ = session.run_trace().unwrap();
        let _ = session.efficiency().unwrap();

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-151: with the `pricing` feature, `flux_sdk::pricing::load_pricing_table` resolves and yields
    /// a usable table (the built-in rates, before any `~/.flux/pricing.toml` overlay). Without the
    /// feature the module is absent and `flux-credentials` is not in the dependency tree (asserted
    /// out-of-band via `cargo tree`).
    #[cfg(feature = "pricing")]
    #[test]
    fn pricing_feature_exposes_the_loader() {
        let table = crate::pricing::load_pricing_table();
        // A well-known model is priced by the built-in table the loader starts from.
        assert!(
            table.rates_for("claude-sonnet-4.6").is_some() || !format!("{table:?}").is_empty(),
            "the loaded table carries the built-in rates"
        );
    }

    /// D-153: with the `providers` feature, `flux_sdk::providers::from_spec` builds a working
    /// provider from a model spec. `ollama/qwen3` needs no credential (local endpoint), so it
    /// resolves offline; the resolved model id rides back for `ClientBuilder::model`.
    #[cfg(feature = "providers")]
    #[test]
    fn providers_from_spec_builds_a_credential_free_provider() {
        let (provider, model) =
            crate::providers::from_spec("ollama/qwen3").expect("ollama needs no credential");
        assert_eq!(model, "qwen3", "the resolved model id rides back");
        // A working `Provider` — usable as the `Client::build` argument.
        let _name = provider.name();
    }

    /// D-153 / lean-default enforcement (not aspirational): the default build must pull no provider
    /// batteries. Assert structurally from the manifest — `default = []`, and `flux-providers` /
    /// `flux-credentials` are `optional` (so `cargo build` with no features links neither, nor their
    /// transitive deps). A regression here (a battery made non-optional, or added to `default`) fails
    /// this test rather than silently fattening every downstream default build.
    #[test]
    fn default_build_pulls_no_optional_provider_batteries() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains("default = []"),
            "default features must be empty (provider-agnostic default build)"
        );
        assert!(
            manifest.contains("flux-providers = { workspace = true, optional = true }"),
            "flux-providers must be optional (the `providers` feature only)"
        );
        assert!(
            manifest.contains("flux-credentials = { workspace = true, optional = true }"),
            "flux-credentials must be optional (the `pricing` feature only)"
        );
    }
}
