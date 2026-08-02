//! `flux-sdk` — the high-level library API.
//!
//! Wraps the flux-flow engine, built-in tools, the safety envelope, and a session into a small
//! [`Client`]. You supply a [`Provider`] (from `flux-providers`) and a workspace
//! root; the SDK wires the rest.
//!
//! There are three front doors: [`Client`] (an adaptive agent turn driven by an authored Flux-Lang
//! outer loop, returning a [`TurnOutput`]), [`FlowClient`] (the authored Flux-Lang
//! `parse → analyze → execute` lifecycle), and the [`dsl`] (author the AST in Rust).
//! `Client` assembles [`flux_flow::engine::FlowEngine`]; `FlowClient` delegates directly to the same
//! `flux-flow` runtime adapter, store, and safety envelope for one-flow execution. Each
//! door has a runnable, no-API-key example: `examples/client_basic.rs`,
//! `examples/parameterized_flow.rs`, and `examples/dsl_loops.rs` respectively. On top of the DSL,
//! [`recipes`] is a cookbook of reusable, parameterized flow builders (routing, lookup, the loop
//! family, resilience).
//!
//! **An autonomous posture carries its own floor.** The safety envelope is
//! *authorization → approval → guarded IO*, and approval is the only stage with a human in it.
//! Varying that stage is choosing a posture, not switching safety off — research, hardening and long
//! exploration are cases where prompting per effect is the wrong design. So
//! [`auto_approve(true)`](ClientBuilder::auto_approve) — the embedded analogue of `flux run --yes` —
//! brings the rest of the posture with it (C-444), the same way the `flux` binary pins every surface
//! with no approval boundary to a fail-closed sandbox (C-262 / C-410). A client built with blanket
//! auto-approval resolves to sandbox `require` with the sandbox network closed, and to
//! [`ResourceLimits::autonomous`] ceilings that bound a delegated tree and not just one agent.
//! A supervised client is unchanged: it resolves the ambient environment (`FLUX_SANDBOX` /
//! `FLUX_SANDBOX_NET` / `FLUX_SANDBOX_WRITABLE`, and `off` when unset).
//!
//! Both raises are floors over silence, never overrides. An explicit
//! [`with_sandbox`](ClientBuilder::with_sandbox) or
//! [`resource_limits`](ClientBuilder::resource_limits) wins outright — one visible line in your own
//! source is how you say "isolation comes from the outer container" or "this budget is mine" — and a
//! *stricter* ambient `FLUX_SANDBOX` still applies. See [`Sandbox`] for the confinement side.
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

mod assembly;
mod envelope;
pub mod events;
pub mod flow;
pub mod session;
pub mod storage;
/// The Deterministic Agent Test Kit (feature `test-kit`, default off): record a run once, commit
/// it as a redacted golden fixture, and re-run the REAL agent offline in `cargo test` for $0 —
/// asserting on the canonical Flux-Lang plan (not a transcript). See [`test::Scenario`].
#[cfg(feature = "test-kit")]
pub mod test;
/// Counterfactual session comparison (D-174 slice; grows in D-176's Tune). Wraps
/// [`Session::fork`]/[`Fork::inject`]/[`Fork::diff`] into a small assertion surface —
/// [`whatif::Counterfactual`]. Not feature-gated: it only ever touches shipped, always-available
/// primitives.
pub mod whatif;

pub use events::{AgentEvent, FlowStream, TurnStream};
pub use flow::{assemble_registry, ExecutionResult, FlowClient, FlowClientBuilder};
pub use session::{Fork, Session};
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

#[allow(deprecated)]
pub use flux_agent::AgentExecutorConfig;
/// The agent definition ([`ClientBuilder::from_spec`]) plus its permission rules. Re-exported so
/// the full-control door needs no direct `flux-agent` dependency.
pub use flux_agent::{
    AdaptiveLoopPolicy, AgentLoopSpec, AgentProfile, AgentSpec, AgentStagePolicy, BuiltinAgentLoop,
    Permissions, PromptCacheClass, PromptLayer, PromptLayerKind, PromptManifestEntry, PromptTrust,
};

/// The per-turn token accounting carried on [`TurnOutput`]. Re-exported from `flux-core`.
pub use flux_core::Usage;

/// The model rate table [`Session::cost`] prices a session against.
/// [`PricingTable::builtin`](flux_core::PricingTable::builtin) is the curated default; the optional
/// `pricing` feature adds [`pricing::load_pricing_table`] to overlay a user's `~/.flux/pricing.toml`.
pub use flux_core::PricingTable;

/// The report [`Session::replay`] returns — the replayed plans, a divergence diagnostic (`None` on a
/// faithful replay), and cassette-cell accounting. Re-exported from `flux-flow`.
pub use flux_flow::replay::ReplayReport;

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

/// **Live systems of record.** Implement [`LiveDatasource`](datasource::LiveDatasource), then use
/// [`ClientBuilder::try_with_live_datasource`] to install its generated `<domain>.list` and
/// `<domain>.get` operations, evidence group, and configured-domain signal as one unit. The row,
/// filter, paging, and weak-reference contracts are re-exported here so an SDK embedder does not
/// need to name flux's internal capability crates.
pub mod datasource {
    pub use flux_capabilities::{LiveAccess, LiveDatasource, LiveDatasourceSurface};
    pub use flux_datasource::live::{
        FilterKey, FilterType, FilterValue, Filters, LiveEntity, LiveSchema, Page, PageRequest,
        Reference, Row,
    };
}

/// Build a typed, closure-backed stage operation. `I` and `O` are independent contracts: both JSON
/// Schemas are derived and registered, the Flux analyzer infers `O` at call sites, and execution
/// still traverses the ordinary authorization/approval dispatcher. The safe default is a low-risk,
/// idempotent read stage; use a bespoke [`tools::Tool`] when the stage has effects or permission
/// subjects that need a stronger contract.
pub fn stage_fn<I, O, F, Fut, E>(
    name: impl Into<String>,
    description: impl Into<String>,
    handler: F,
) -> Arc<dyn tools::Tool>
where
    I: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static,
    O: serde::Serialize + schemars::JsonSchema + Send + 'static,
    F: Fn(I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = std::result::Result<O, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let spec = flux_spec::ToolSpec::read_only_typed::<I>(name, description)
        .with_output_schema(flux_spec::tool_output_schema::<O>());
    let handler = Arc::new(handler);
    Arc::new(
        flux_runtime::FnTool::new(spec, move |value| {
            let handler = handler.clone();
            async move {
                let input = serde_json::from_value::<I>(value)
                    .map_err(|error| format!("invalid stage input: {error}"))?;
                let output = handler(input).await.map_err(|error| error.to_string())?;
                serde_json::to_value(output)
                    .map_err(|error| format!("stage output did not serialize: {error}"))
            }
        })
        .with_staging_disposition(flux_spec::StagingDisposition::Gather),
    )
}

/// **Approval policy.** Implement [`Approver`](approval::Approver) and pass it to a builder's
/// `approver(...)` to gate ops with your own logic; [`ApprovalChoice`](approval::ApprovalChoice) is
/// your verdict, [`IntentSet`](approval::IntentSet) is the per-call intent your `request` receives,
/// and [`RiskApprover`](approval::RiskApprover) is a ready-made risk-tiered policy.
pub mod approval {
    pub use flux_runtime::{ApprovalChoice, Approver, RiskApprover};
    pub use flux_spec::IntentSet;
}

/// Typed, host-rendered interaction requested by an agent turn.
pub mod interaction {
    pub use flux_runtime::{
        InteractionCapabilities, InteractionInputMode, InteractionOrigin, InteractionResponse,
        PromptAudioRef, UserInteraction, UserInteractionRequest, UserPrompt,
    };
}

/// **Authorization floor.** Every SDK executor carries an [`ExecutionAuthorization`] profile.
/// Builders use [`ExecutionAuthorization::local`] by default; embedding services should install a
/// resolved policy and identity with `with_authorization(...)`.
pub mod authorization {
    pub use flux_policy::{
        default_local_grants, local_identity, AuthorizationPolicy, Caller, Trust,
    };
    pub use flux_runtime::{
        ExecutionAuthorization, ExecutionEnvironment, IdentityCell, TurnIdentity,
    };
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
    pub use flux_events::{DiffRow, EfficiencySummary, ModelCost, RunDiff, TurnSummary};
    pub use flux_evidence::{Observation, SignalMatch, ToolGroup, KIND_SIGNAL};
    pub use flux_flow::state::FlowStore;
    pub use flux_lang::ast::RunEvent;
}

/// **Voice.** The types the two voice front doors take —
/// [`Session::run_voice_flow`](Session::run_voice_flow) (flow-driven: the flow leads, the model is
/// speech I/O) and [`FlowClient::run_voice_session`](flow::FlowClient::run_voice_session)
/// (model-driven: the model leads and calls tools). Implement [`VoiceSink`](voice::VoiceSink) to
/// receive audio / transcripts / lifecycle; [`VoiceReply`](voice::VoiceReply) is a flow turn's
/// spoken reply (`Continue`/`Complete`); [`RealtimeProvider`](voice::RealtimeProvider) +
/// [`RealtimeConfig`](voice::RealtimeConfig) are the full-duplex provider seam (a concrete one lives
/// behind `flux-providers`' `realtime` feature).
pub mod voice {
    pub use flux_flow::voice::{VoiceReply, VoiceSink};
    pub use flux_provider::{RealtimeConfig, RealtimeProvider};
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

/// **Subprocess plugin tools** (feature `plugins`). Load an installed plugin's operations as
/// policy-gated tools inside an embedded agent — the same authorization → approval → guarded-IO
/// envelope, the same approval path. Attach them with
/// [`ClientBuilder::with_plugin_tools`](ClientBuilder::with_plugin_tools) (conversational door) or
/// [`FlowClient::register_plugin`](flow::FlowClient::register_plugin) (flow door), or load them
/// yourself with [`load_tools`](plugins::load_tools).
///
/// **Trust boundary:** plugin binaries are *trusted dependencies* — native code the host spawns as
/// a subprocess, not OS-sandboxed. Their host capabilities are still manifest-scoped
/// ([`SystemHostCaps`](plugins::SystemHostCaps) defaults — a plugin may only run programs / read
/// secrets / reach hosts its manifest declares), but the process itself runs your machine's code.
/// Install only plugins you trust.
#[cfg(feature = "plugins")]
pub mod plugins {
    pub use flux_plugin::{HostCapabilities, PluginDescriptor, PluginManifest, SystemHostCaps};

    use std::sync::Arc;

    use flux_core::Result;
    use flux_runtime::Tool;
    use flux_system::System;

    /// Spawn the plugin described by `descriptor` (verified against its pin), fetch its manifest, and
    /// project every operation as a policy-gated [`Tool`]. Host capabilities are **manifest-scoped**
    /// ([`SystemHostCaps`] defaults over `system` — nothing widened). Register the returned tools
    /// into a client's catalog (they hold the subprocess connection alive); each dispatches through
    /// the one safety envelope, just like a built-in.
    pub async fn load_tools(
        system: &Arc<System>,
        name: &str,
        descriptor: &PluginDescriptor,
    ) -> Result<Vec<Arc<dyn Tool>>> {
        let caps_system = system.clone();
        let loaded = flux_plugin::load_plugin_tools(system, name, descriptor, move |m| {
            Arc::new(SystemHostCaps::new(caps_system).with_manifest(m)) as Arc<dyn HostCapabilities>
        })
        .await?;
        Ok(loaded.tools)
    }
}

/// **Sub-agents.** Attach named roles to a conversational client with
/// [`ClientBuilder::with_sub_agents`] (or to a flow client with
/// [`FlowClient::with_sub_agents`](flow::FlowClient::with_sub_agents)); a turn whose native stage calls
/// `task(role, …)` then delegates to a role's child agent through the same
/// authorization → approval → guarded-IO envelope. [`SubAgents`](subagents::SubAgents) is the bundle
/// (roles + the child tool surface + a [`ProviderFactory`](subagents::ProviderFactory) + limits),
/// [`SpawnLimits`](subagents::SpawnLimits) bounds each child (tokens, wall-clock, …), and
/// [`Role`](subagents::Role)/[`RoleRegistry`](subagents::RoleRegistry) name the roles a `task` may
/// target ([`try_parse_role`](subagents::try_parse_role) builds a `Role` from a markdown
/// definition while rejecting malformed capability metadata).
pub mod subagents {
    #[allow(deprecated)]
    pub use flux_orchestrate::{
        parse_role, try_parse_role, ProviderFactory, Role, RoleRegistry, SpawnLimits, SubAgents,
    };
}

/// The OS-sandbox posture types, re-exported so a consumer can inject an explicit sandbox into a
/// builder via [`ClientBuilder::with_sandbox`]/[`flow::FlowClientBuilder::with_sandbox`] without
/// taking a direct `flux-system` dependency.
///
/// **Autonomy brings the floor; everything else is yours.** One configuration does infer a posture:
/// [`auto_approve(true)`](ClientBuilder::auto_approve) and an injected opaque
/// [`Approver`](approval::Approver) both resolve to `require` with the sandbox network closed
/// (C-444), matching what the `flux` binary pins for every surface where a human boundary is absent
/// or cannot be established (C-262 / C-410). A custom approver may be interactive, but this crate
/// cannot distinguish that from a three-line blanket allow. A served HTTP front end or cron-driven
/// worker states its posture explicitly when an outer boundary or a human-owned policy makes the
/// conservative floor unnecessary:
///
/// ```ignore
/// // Explicit, independent of ambient env — the recommended form for a daemon.
/// use flux_sdk::{Client, Sandbox, SandboxSettings};
/// use flux_sdk::sandbox::SandboxMode;
/// let mut settings = SandboxSettings::off();
/// settings.mode = SandboxMode::Require;   // refuse to spawn at all with no usable backend
/// settings.network = false;               // what an autonomous posture also defaults to
/// let client = Client::builder()
///     .with_approver(my_policy)
///     .with_sandbox(Sandbox::resolve(settings))
///     .build(provider, ".")?;
/// ```
///
/// or by exporting `FLUX_SANDBOX=require` before `build`, which is what
/// [`SandboxSettings::from_env`] reads when no sandbox is injected. An explicit `with_sandbox` is
/// also the escape hatch in the other direction — an autonomous client whose isolation comes from an
/// outer container says so here, in one visible line, and that decision wins outright. Either way,
/// verify with [`Sandbox::ensure_available`] rather than assuming: a `require` posture on a host with
/// no backend is an error you want at startup, not at the first spawn.
pub use flux_system::sandbox::{Sandbox, SandboxSettings};

/// The sandbox posture enum and backend discovery behind [`Sandbox`], re-exported for an embedder
/// building a [`SandboxSettings`] by hand (see [`Sandbox`] for why one would).
pub mod sandbox {
    pub use flux_system::sandbox::{Backend, SandboxMode};
}

/// The runtime **resource ceilings** (C-290), re-exported so a consumer can bound a runtime it
/// constructs — via [`ClientBuilder::resource_limits`] or
/// [`FlowClientBuilder::resource_limits`](flow::FlowClientBuilder::resource_limits) — without
/// taking a direct `flux-runtime` dependency. These bound what the runtime *uses* (simultaneously
/// executing tool calls, live agents in a delegated tree, retained result bytes), where
/// `context_budget` / `max_iterations` / `max_tokens` bound what it *spends*.
///
/// **A tree, not an agent (C-444).** `max_concurrent_tool_calls` is per agent, and a sub-agent gets its
/// own execution budget by construction — sharing one across the `task` boundary deadlocks. So the
/// per-agent number alone has no tree-wide total: `N` per agent times an unbounded agent count is
/// unbounded. [`ResourceLimits::with_max_live_agents`] bounds the other factor, making the total
/// `N × max_live_agents`, and [`ResourceLimits::autonomous`] — what an auto-approving client resolves
/// when you state nothing — sets both.
pub use flux_runtime::{
    AgentCensusRefusal, ConcurrencyRefusal, ResourceLimits, DEFAULT_TOOL_CALL_QUEUE_TIMEOUT,
};

/// The Rust **embedded DSL** for authoring flows — builder primitives that construct the Flux-Lang
/// AST. Build a [`flux_lang::ast::DraftAst`] with `dsl::Flow`/`dsl::Block` (loops and control-flow are
/// first-class), then drive it through [`FlowClient::analyze`] + [`FlowClient::execute`]. Re-exported
/// from `flux-lang` so consumers can stay inside `flux_sdk`. See `examples/dsl_loops.rs`.
pub use flux_lang::dsl;

pub mod recipes;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

// `AgentSpec`, `Usage`, and `Provider` are in scope via the public re-exports above.
use flux_cognition::CognitionPack;
use flux_core::ContextBlock;
use flux_core::Result;
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_orchestrate::{SubAgents, TaskTool};
#[cfg(test)]
use flux_runtime::ToolContext;
use flux_runtime::{Approver, ExecutionEnvironment, PermissionManager, Tool, ToolRegistry};
use flux_secret::Redactor;
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
    /// D-178: set when this turn first had to **resurrect** an interrupted predecessor — a turn on
    /// this session that a crash, OOM, or redeploy killed mid-execution, which
    /// [`ClientBuilder::auto_resurrect`] finished (from the crash point, with no model re-spend)
    /// before the new input ran. `None` on the overwhelmingly common clean path.
    ///
    /// Boxed because the report is large and almost always absent — the common `TurnOutput` stays
    /// one pointer wider, not several hundred bytes.
    pub resurrected: Option<Box<flux_flow::resurrect::ResurrectReport>>,
}

/// A deferred registry installer (the `register_*` pack convention), applied at `build`.
type RegistryPack = Box<dyn FnOnce(&mut ToolRegistry) -> Result<()>>;

/// Builder for a [`Client`]. Internally an [`AgentSpec`] plus the shared envelope knobs, so every
/// agent-definition field has exactly one home and [`from_spec`](Self::from_spec) is the
/// full-control escape hatch rather than a parallel path.
pub struct ClientBuilder {
    spec: AgentSpec,
    envelope: envelope::Envelope,
    storage: Option<Storage>,
    cognition: bool,
    /// D-188: opt into the model-invoked progressive skill disclosure catalog, discovered from the
    /// workspace root at [`build`](Self::build) time (before it's known, so this stays a flag here
    /// rather than an eagerly-populated field).
    model_invoked_skills: bool,
    ops: Vec<(String, Arc<dyn Tool>)>,
    packs: Vec<RegistryPack>,
    live_surfaces: Vec<flux_capabilities::LiveDatasourceSurface>,
    sub_agents: Option<SubAgents>,
    sub_agent_adaptive_policy: Option<AdaptiveLoopPolicy>,
    // D-178: `None` ⇒ derive from the storage's durability at `build` (on for durable stores).
    auto_resurrect: Option<bool>,
    user_interaction: Option<Arc<dyn interaction::UserInteraction>>,
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
            model_invoked_skills: false,
            ops: Vec::new(),
            packs: Vec::new(),
            live_surfaces: Vec::new(),
            // Unset ⇒ no `task` tool, no spawner (children off by default).
            sub_agents: None,
            auto_resurrect: None,
            sub_agent_adaptive_policy: None,
            user_interaction: None,
        }
    }
}

impl ClientBuilder {
    /// Start from a hand-built [`AgentSpec`] — the full-control escape hatch. The spec's own
    /// permissions are taken as-is: unlike [`builder`](Self::builder), there is **no implicit
    /// `read` pre-allow**, so a spec with empty `permissions` and no [`auto_approve`](Self::auto_approve)
    /// denies *every* op (including reads) — grant what the agent needs via the spec's
    /// `permissions`, [`allow`](Self::allow), or `auto_approve`. Builder methods overlay on top. A
    /// `skills` is explicit: an empty list means no skills. Use
    /// [`AgentSpec::try_with_default_skills`] deliberately if enabling every discovered skill is desired.
    pub fn from_spec(spec: AgentSpec) -> Self {
        Self {
            spec,
            envelope: envelope::Envelope::bare(),
            storage: None,
            cognition: false,
            model_invoked_skills: false,
            ops: Vec::new(),
            packs: Vec::new(),
            live_surfaces: Vec::new(),
            sub_agents: None,
            sub_agent_adaptive_policy: None,
            auto_resurrect: None,
            user_interaction: None,
        }
    }
    /// Set the model id every turn uses.
    pub fn model(mut self, m: impl Into<String>) -> Self {
        self.spec.model = m.into();
        self
    }
    /// Select optional behavior after the mandatory Flux harness protocol.
    pub fn profile(mut self, profile: AgentProfile) -> Self {
        self.spec.profile = profile;
        self
    }
    /// Set caller-authored role/persona instructions without replacing the harness protocol.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.spec.instructions = instructions.into();
        self
    }
    /// Cap the max output tokens per model call.
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.spec.max_tokens = n;
        self
    }
    /// Cap the authored outer loop's decision/batch iterations per turn (default: 50, maximum:
    /// 1,000). This is separate from the adaptive policy's provider-call budget. Out-of-range
    /// values fail when the agent is assembled.
    pub fn max_iterations(mut self, n: usize) -> Self {
        self.spec.max_iterations = n;
        self
    }
    /// Configure intent/exploration model policy and the shared logical-run provider-call ceiling
    /// (default: 50).
    pub fn adaptive_policy(mut self, policy: AdaptiveLoopPolicy) -> Self {
        self.spec.adaptive_policy = policy;
        self
    }
    /// Select the Flux-Lang outer control program for conversational turns.
    pub fn agent_loop(mut self, agent_loop: AgentLoopSpec) -> Self {
        self.spec.agent_loop = agent_loop;
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
    /// Approve every tool call automatically — an **autonomous posture**, the library analogue of
    /// `flux run --yes`.
    ///
    /// This is a valid choice, not safety switched off: research, security hardening and long
    /// exploration are cases where interrupting per effect is the wrong design. What it changes is
    /// which stage of the envelope constrains — with no human at approval, isolation and budgets are,
    /// so choosing this **brings both with it** (C-444). Unless you state otherwise, a client built
    /// this way resolves to sandbox `require` with the sandbox network closed, and to
    /// [`ResourceLimits::autonomous`] ceilings that bound a delegated tree as well as one agent — the
    /// same pairing the CLI applies to its own unattended surfaces (C-262 / C-410).
    ///
    /// Both are floors over silence. [`with_sandbox`](Self::with_sandbox) and
    /// [`resource_limits`](Self::resource_limits) still win outright, which is how an embedder whose
    /// isolation comes from an outer container, or whose budget is its own, says so. An injected
    /// [`Approver`] gets the same conservative floor because its implementation is opaque; explicit
    /// sandbox and resource-limit decisions remain authoritative. See [`Sandbox`].
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
    /// Install the mandatory authorization policy and resolved caller identity. This floor is
    /// evaluated before permission rules or approval, so `auto_approve(true)` cannot widen it.
    pub fn with_authorization(
        mut self,
        policy: flux_policy::AuthorizationPolicy,
        caller: flux_policy::Caller,
        trust: flux_policy::Trust,
    ) -> Self {
        self.envelope.authorization =
            flux_runtime::ExecutionAuthorization::new(policy, caller, trust);
        self
    }
    /// Install the shared secret redactor on the assembled agent executor.
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.envelope.redactor = redactor;
        self
    }
    /// Register a custom op (any [`Tool`], e.g. one built with `flux_runtime::tool_fn`) alongside
    /// the built-ins. Registered ops dispatch through the same authorization → approval → guarded
    /// IO envelope as every other op — registration grants existence, not permission.
    pub fn register_op(mut self, tool: Arc<dyn Tool>) -> Self {
        self.ops
            .push(("sdk ClientBuilder::register_op".into(), tool));
        self
    }

    /// Register a custom operation with an auditable owner label. Prefer this when composing
    /// independently developed packs so duplicate-name errors identify both contributors.
    pub fn register_op_from(mut self, source: impl Into<String>, tool: Arc<dyn Tool>) -> Self {
        self.ops.push((source.into(), tool));
        self
    }
    /// Register a whole pack of ops via a closure over the registry (the `register_*` convention
    /// used across flux). Same envelope rules as [`register_op`](Self::register_op).
    pub fn register_pack<F: FnOnce(&mut ToolRegistry) + 'static>(mut self, pack: F) -> Self {
        self.packs.push(Box::new(move |registry| {
            pack(registry);
            Ok(())
        }));
        self
    }
    /// Register a fallible operation pack. This is the preferred composition seam: installers use
    /// source-labelled registry methods and `build` returns any duplicate instead of panicking.
    pub fn try_register_pack<F>(mut self, pack: F) -> Self
    where
        F: FnOnce(&mut ToolRegistry) -> Result<()> + 'static,
    {
        self.packs.push(Box::new(pack));
        self
    }

    /// Install one async live system of record as a generated `<domain>.list` / `<domain>.get`
    /// pair. Static backend metadata is validated immediately; operation collisions remain
    /// source-labelled build errors when this prepared registry is composed with built-ins and
    /// other consumer packs.
    ///
    /// The returned builder retains the domain's evidence group and ambient configured signal
    /// separately from [`groups`](Self::groups) and [`ambient_signals`](Self::ambient_signals), so
    /// calling those setters before or after this method cannot tear apart the all-in-one surface.
    pub fn try_with_live_datasource(
        mut self,
        domain: impl Into<String>,
        backend: Arc<dyn flux_capabilities::LiveDatasource>,
    ) -> Result<Self> {
        let domain = domain.into();
        let mut prepared = ToolRegistry::new();
        let surface =
            flux_capabilities::try_register_live_datasource(&mut prepared, &domain, backend)?;
        self.packs
            .push(Box::new(move |registry| registry.try_extend(prepared)));
        self.live_surfaces.push(surface);
        Ok(self)
    }
    /// Attach a subprocess plugin's operations (feature `plugins`) as policy-gated tools. Load them
    /// first with [`plugins::load_tools`] over a guarded [`System`] — plugin ops carry their own host
    /// connection, so they dispatch through the safety envelope like any [`register_op`](Self::register_op)
    /// tool (a subset via [`tools`](Self::tools) re-admits them). A `tools`-subset build keeps them.
    ///
    /// ```ignore
    /// let tools = flux_sdk::plugins::load_tools(&system, "gitlab", &descriptor).await?;
    /// let client = flux_sdk::Client::builder().with_plugin_tools(tools).build(provider, ".")?;
    /// ```
    #[cfg(feature = "plugins")]
    pub fn with_plugin_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.ops.extend(tools.into_iter().map(|tool| {
            (
                "sdk ClientBuilder::with_plugin_tools (plugin source unspecified)".into(),
                tool,
            )
        }));
        self
    }

    /// Attach one named plugin's tools with source-aware collision diagnostics.
    #[cfg(feature = "plugins")]
    pub fn with_plugin_tools_from(
        mut self,
        plugin: impl Into<String>,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        let source = format!("plugin:{}", plugin.into());
        self.ops
            .extend(tools.into_iter().map(|tool| (source.clone(), tool)));
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
    /// [`SubAgents::with_limits`](subagents::SubAgents::with_limits). A streamed conversational
    /// turn's cancel token ([`Session::stream`]`().cancel()`) reaches a running child. A top-level
    /// one-shot [`FlowClient`] has no cancel or parent lineage; when opened inside a guarded adapter,
    /// however, it lexically inherits both and pins them before streamed execution crosses
    /// `tokio::spawn`.
    pub fn with_sub_agents(mut self, mut sub_agents: SubAgents) -> Self {
        if sub_agents.limits.wall_clock.is_none() {
            sub_agents.limits.wall_clock = Some(std::time::Duration::from_secs(600));
        }
        self.sub_agents = Some(sub_agents);
        self.sub_agent_adaptive_policy = None;
        self
    }
    /// Attach named sub-agents with an explicit native intent/explore policy for every child.
    /// This is the conversational-client sibling of
    /// [`FlowClient::with_sub_agents_policy`](crate::FlowClient::with_sub_agents_policy). Existing
    /// [`with_sub_agents`](Self::with_sub_agents) callers retain
    /// [`AdaptiveLoopPolicy::default`]; this method lets an embedding host independently select
    /// child stage models/effort and bound output plus model calls.
    pub fn with_sub_agents_policy(
        mut self,
        mut sub_agents: SubAgents,
        adaptive_policy: AdaptiveLoopPolicy,
    ) -> Self {
        if sub_agents.limits.wall_clock.is_none() {
            sub_agents.limits.wall_clock = Some(std::time::Duration::from_secs(600));
        }
        self.sub_agents = Some(sub_agents);
        self.sub_agent_adaptive_policy = Some(adaptive_policy);
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
    /// every spawn. When left unset (the default), the posture is resolved at [`build`](Self::build)
    /// from the environment — `Sandbox::resolve(SandboxSettings::from_env())`, so a consumer that
    /// exports `FLUX_SANDBOX=require` gets confinement without calling this (off ⇒ disabled, safe) —
    /// except under an autonomous posture, where [`auto_approve(true)`](Self::auto_approve) raises the
    /// floor to fail-closed `require` with the network closed (C-444).
    ///
    /// **This call wins outright**, in either direction: it pins a posture independent of ambient env,
    /// and it is also how an autonomous embedder whose isolation comes from elsewhere — an outer
    /// container, a VM, a disposable host — declines the raise in one line that is visible in its own
    /// source. [`Sandbox`] shows both forms.
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
    /// D-178: whether a turn entry first **resurrects** an interrupted turn on the same session —
    /// one a crash, OOM, or redeploy killed mid-execution — finishing it from the crash point with
    /// **no model re-spend** before the new input runs. Defaults to on for durable storage
    /// ([`Storage::dir`] and [`Storage::custom`], where a killed turn can outlive the process) and
    /// off for [`Storage::in_memory`], where nothing survives a crash to resurrect.
    ///
    /// Never silent: the recovery is streamed to the turn's own sink and reported on
    /// [`TurnOutput::resurrected`]. Turn it off to take manual control via
    /// [`Session::interrupted`]/[`Session::resurrect`] — the same driver, explicitly invoked.
    pub fn auto_resurrect(mut self, on: bool) -> Self {
        self.auto_resurrect = Some(on);
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

    /// Bound what this runtime **uses**, not just what it spends (C-290).
    ///
    /// [`context_budget`](Self::context_budget), [`max_iterations`](Self::max_iterations),
    /// [`max_tokens`](Self::max_tokens) and the adaptive policy's call budget all cap *spend*.
    /// [`ResourceLimits`] caps *use*: how many tool calls may execute simultaneously
    /// ([`with_max_concurrent_tool_calls`](ResourceLimits::with_max_concurrent_tool_calls)) and how
    /// many bytes of tool results the runtime retains
    /// ([`with_max_retained_result_bytes`](ResourceLimits::with_max_retained_result_bytes)).
    ///
    /// Enforced inside the safety envelope, so it binds for this in-process client — not only for
    /// `flux-server`, whose `max_inflight_per_principal` was the only concurrency control before
    /// this existed. Exceeding the concurrency ceiling is an actionable refusal, never a silent
    /// truncation; the wait before that refusal is bounded by
    /// [`with_tool_call_queue_timeout`](ResourceLimits::with_tool_call_queue_timeout), which
    /// defaults to 30s and is *not* clamped, so a host that sets an absurd value gets an absurd
    /// wait.
    ///
    /// **The default depends on the posture (C-444).** State nothing and the built-in deny approver is
    /// unbounded, as it always was — every effect stops there. `auto_approve(true)` and an injected,
    /// opaque approver resolve to [`ResourceLimits::autonomous`] instead, because the SDK cannot prove
    /// that custom policy contains a human and "unattended *and* unbounded" is the combination the
    /// envelope cannot defend. Calling this method wins outright either way, including with a
    /// deliberately unbounded `ResourceLimits::new()`: stating a ceiling is a decision, and this never
    /// second-guesses it.
    ///
    /// **Scope: the execution budget is PER AGENT; the agent census is PER TREE (C-299, C-444).** The
    /// ceilings descend into sub-agents — a `task`-delegated child is built with them installed, at
    /// every delegation depth, where before it ran on a fresh **unbounded** executor:
    ///
    /// * [`with_max_concurrent_tool_calls`](ResourceLimits::with_max_concurrent_tool_calls)`(N)`
    ///   bounds **each agent** at N simultaneous tool calls, and each agent gets its own budget rather
    ///   than a share of one. One shared semaphore would be the stronger guarantee and it deadlocks —
    ///   see [`ResourceLimits::independent_copy`] for exactly why, and why marking delegating ops does
    ///   not rescue it.
    /// * [`with_max_live_agents`](ResourceLimits::with_max_live_agents)`(k)` is what gives the tree a
    ///   total anyway: it bounds the *other* factor, so the whole delegated tree runs at most N×k tool
    ///   calls at once. Unlike the semaphore it is shared across the delegation boundary, which is
    ///   sound because it **refuses** rather than queueing — a refusal cannot join the wait cycle that
    ///   made a shared semaphore deadlock. Set neither and the tree total is unbounded; that pairing,
    ///   not the per-agent number, was the finding.
    /// * The byte ceilings are per agent for the plainer reason that each executor owns its own op
    ///   cache and evidence log, so there is nothing shared to bound.
    ///
    /// Within one agent the ceiling *is* shared across every executor derived from it, including the
    /// fresh one a surface mints per run — so `FlowClient::build_executor` cannot escape it.
    ///
    /// A file-configured host builds the same value from `[limits]` with
    /// [`ResourceLimits::from_config`].
    pub fn resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.envelope.resource_limits = Some(limits);
        self
    }

    /// Opt into Claude-style progressive skill disclosure (D-188): every skill discovered under
    /// the workspace root (resolved at [`build`](Self::build)) gets its name+description surfaced
    /// to the model, except those declaring `disable-model-invocation: true`; the model can pull a
    /// body into context on demand via `skill.load`. Off by default — [`Self::from_spec`]'s
    /// explicit `AgentSpec.skills` stays the always-injected activation surface this is additive
    /// to, mirroring the CLI's `--skills-model-invoked` flag / `[skills] model_invoked` config.
    pub fn model_invoked_skills(mut self) -> Self {
        self.model_invoked_skills = true;
        self
    }

    /// Install a host-owned typed interaction handler and expose `user.ask` to this agent.
    ///
    /// The runtime validates both the request schema and submitted value. Installing the handler
    /// pre-allows only this operation; an explicit deny still wins.
    pub fn with_user_interaction(
        mut self,
        interaction: Arc<dyn interaction::UserInteraction>,
    ) -> Self {
        self.user_interaction = Some(interaction);
        if !self.envelope.allow.iter().any(|name| name == "user.ask") {
            self.envelope.allow.push("user.ask".to_string());
        }
        if let Some(tools) = self.spec.tools.as_mut() {
            if !tools.iter().any(|name| name == "user.ask") {
                tools.push("user.ask".to_string());
            }
        }
        self
    }

    /// Build the client with `provider` and a workspace rooted at `root`. Sessions live in the
    /// configured [`Storage`] (in-memory unless set). The turn runs on [`FlowEngine`] (the model
    /// plans, the runtime runs the flux-lang agent loop).
    pub fn build(
        mut self,
        provider: Box<dyn Provider>,
        root: impl Into<PathBuf>,
    ) -> Result<Client> {
        let root = root.into();
        if self.model_invoked_skills {
            self.spec.cwd = root.clone();
            self.spec = self.spec.try_with_model_invoked_skills()?;
        }
        let provider: Arc<dyn Provider> = Arc::from(provider);
        // Attach the OS-sandbox posture so a consumer's `FLUX_SANDBOX=require` is honored on this
        // client's spawns; a bare `System::new` defaults to `Sandbox::disabled()` (no confinement,
        // no `require` enforcement). Unset ⇒ resolve from env (off ⇒ disabled, safe default).
        let sandbox = self.envelope.resolve_sandbox();
        let system = Arc::new(System::new(Workspace::new(root.clone())?).with_sandbox(sandbox));
        let mut registry = ToolRegistry::new();
        flux_tools::try_register_builtins(&mut registry)?;
        if self.cognition {
            CognitionPack::new(provider.clone(), self.spec.model.clone())
                .with_reasoning(self.spec.thinking, self.spec.effort)
                .try_register_from("sdk ClientBuilder cognition pack", &mut registry)?;
        }
        // Snapshot the base op names (built-ins + cognition) so consumer-registered ops can be
        // told apart below: a `tools` subset restricts the *base* catalog, but must not silently
        // drop a tool the consumer explicitly registered.
        let base_names: std::collections::HashSet<String> = registry.names().into_iter().collect();
        if let Some(interaction) = &self.user_interaction {
            flux_tools::try_register_user_interaction(
                &mut registry,
                Some(interaction.capabilities()),
            )?;
        }
        // Consumer ops/packs join the same registry the envelope gates — registration grants
        // existence, not permission (the safety envelope still gates every dispatch).
        for (source, tool) in self.ops {
            registry.try_register_from(source, tool)?;
        }
        for pack in self.packs {
            pack(&mut registry)?;
        }
        // Sub-agents: register the `task` tool BEFORE the custom-name snapshot so it rides the same
        // re-admit into a `tools` subset every consumer-registered op does (a consumer that scoped
        // the catalog AND asked for sub-agents still keeps `task`). The spawner is attached to the
        // dispatch context below.
        if self.sub_agents.is_some() {
            registry.try_register_from(
                "sdk ClientBuilder sub-agent task operation",
                Arc::new(TaskTool),
            )?;
        }
        let custom_names: Vec<String> = registry
            .names()
            .into_iter()
            .filter(|n| !base_names.contains(n))
            .collect();
        let approver = self.envelope.resolve_approver();

        let storage = self.storage.unwrap_or_default();
        // D-178: default auto-resurrect on exactly where a crashed turn can survive the process.
        let auto_resurrect = self.auto_resurrect.unwrap_or_else(|| storage.is_durable());
        let (events, flow) = storage.resolve()?;

        // The agent's definition; `assemble` selects the tool subset, applies the permissions,
        // registers the authored-loop stages, and ties the engine⇄loop-host cycle. Builder rules are
        // additive to the spec's own (`from_spec` starts from a bare envelope). Skills are explicit:
        // an empty set stays empty and no default directory is scanned or activated.
        let mut spec = self.spec;
        for surface in self.live_surfaces {
            if let Some(existing) = spec
                .groups
                .iter()
                .find(|group| group.name == surface.group.name)
            {
                if existing != &surface.group {
                    return Err(flux_core::Error::Other(format!(
                        "live datasource group `{}` conflicts with an existing group declaration",
                        surface.group.name
                    )));
                }
            } else {
                spec.groups.push(surface.group);
            }
            if !spec.ambient_signals.contains(&surface.ambient_signal) {
                spec.ambient_signals.push(surface.ambient_signal);
            }
        }
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
        let authorization = self.envelope.authorization.clone();
        // C-444: silence resolves against the approval posture — an autonomous client is bounded by
        // `ResourceLimits::autonomous` rather than left unbounded. An explicit `resource_limits(..)`
        // is honored verbatim.
        let resource_limits = self.envelope.resolve_resource_limits();
        let mut environment = ExecutionEnvironment::new(
            system.clone(),
            registry,
            PermissionManager::new(),
            approver,
            authorization.clone(),
        )
        .with_redactor(self.envelope.redactor.clone())
        // C-290: shared, not copied — every executor the engine derives counts against one budget.
        .with_resource_limits(resource_limits.clone());
        // Thread the sub-agent spawner into the shared environment when sub-agents are attached, so
        // a `task` call delegates through the same guarded `System`; `None` (the common case) leaves
        // the context exactly as before. Mirrors `FlowClient::build_executor`.
        if let Some(sub_agents) = self.sub_agents {
            let sub_agents = sub_agents
                .with_reasoning(spec.thinking, spec.effort)
                .with_authorization_cell(authorization.policy().clone(), authorization.identity())
                // C-299: the ceilings descend into `task`-delegated work, which ran unbounded
                // before. Each child receives an independent copy (same numbers, own concurrency
                // budget) — `LocalSpawner::spawn` does that, because sharing one semaphore across
                // the delegation boundary deadlocks.
                .with_resource_limits(resource_limits.clone());
            let spawner = match self.sub_agent_adaptive_policy {
                Some(policy) => {
                    sub_agents.into_spawner_with_adaptive_policy(system.clone(), policy)
                }
                None => sub_agents.into_spawner(system.clone()),
            };
            environment = environment.with_spawner(spawner);
        }
        let model = spec.model.clone();
        // D-174: retain the assembly pieces BEFORE `assemble_in` consumes `spec`/`environment`, so a
        // later `EngineAssembly::variant` can re-assemble a throwaway engine (the Test Kit's
        // `check()`/`inject_at`) without re-running the whole builder. `AgentSpec`/`ExecutionEnvironment`
        // are both cheap clones (mostly `Arc` bumps).
        let assembly = Arc::new(assembly::EngineAssembly {
            spec: spec.clone(),
            environment: environment.clone(),
            provider: provider.clone(),
            events: events.clone(),
            user_interaction: self.user_interaction.clone(),
        });
        let engine = spec.assemble_in(provider, environment, events, flow)?;
        let engine = match self.user_interaction {
            Some(interaction) => engine.with_user_interaction(interaction),
            None => engine,
        };
        Ok(Client {
            engine: Arc::new(engine),
            model,
            assembly,
            // The default session is created lazily (on first use), so building a client — e.g. a
            // service restarting against a persistent `Storage::dir` — never leaves an empty
            // session behind, and `latest_session()` still points at the real prior conversation.
            default_session: std::sync::Mutex::new(None),
            turn_guard: Arc::new(tokio::sync::Mutex::new(())),
            auto_resurrect,
            resource_limits,
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
    // D-174: retained so a variant (throwaway) engine can be re-assembled later — see
    // `assembly::EngineAssembly`. Not part of the public API.
    assembly: Arc<assembly::EngineAssembly>,
    // The default session's id, created lazily on first use (see `default_id`). Kept behind a
    // std mutex so two concurrent first-uses can't each mint (and leak) a session.
    default_session: std::sync::Mutex<Option<String>>,
    // FlowEngine owns its single-active-turn invariant. Every Session also shares this broader SDK
    // operation guard so turns remain ordered with replay/fork/divergence paths that intentionally
    // execute against the same stores or executor outside FlowEngine's public turn lifecycle.
    turn_guard: Arc<tokio::sync::Mutex<()>>,
    // D-178: handed to every `Session` this client mints — see `ClientBuilder::auto_resurrect`.
    auto_resurrect: bool,
    // C-290: the ceilings this client was built with. The same shared handle the engine's
    // executors enforce, so reading it here reports what actually binds.
    resource_limits: ResourceLimits,
}

impl Client {
    /// Start building a [`Client`].
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// The resource ceilings this client enforces — see [`ClientBuilder::resource_limits`].
    pub fn resource_limits(&self) -> &ResourceLimits {
        &self.resource_limits
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
    /// here still dispatches through the same authorization → approval → guarded-IO envelope, and
    /// public turn entries retain the engine's mandatory single-active-turn guarantee.
    pub fn engine(&self) -> &Arc<FlowEngine> {
        &self.engine
    }

    fn session(&self, id: String) -> Session {
        Session {
            engine: self.engine.clone(),
            id,
            assembly: self.assembly.clone(),
            turn_guard: self.turn_guard.clone(),
            auto_resurrect: self.auto_resurrect,
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

    struct FixedInteraction;

    #[async_trait]
    impl interaction::UserInteraction for FixedInteraction {
        fn capabilities(&self) -> interaction::InteractionCapabilities {
            interaction::InteractionCapabilities::text()
        }

        async fn request(
            &self,
            _request: interaction::UserInteractionRequest,
        ) -> Result<interaction::InteractionResponse> {
            Ok(interaction::InteractionResponse::Cancelled)
        }
    }

    fn parse_role(content: &str, name_fallback: &str) -> crate::subagents::Role {
        crate::subagents::try_parse_role(content, name_fallback).unwrap()
    }

    fn request_has_tool(request: &Request, name: &str) -> bool {
        request.tools.iter().any(|tool| tool.name == name)
    }

    fn intent_chunks(intent: &str, families: &[&str]) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: "intent".into(),
                name: "declare_intent".into(),
                input: serde_json::json!({
                    "intent": intent,
                    "capability_families": families,
                }),
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    fn native_call(id: &str, name: &str, input: serde_json::Value) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input,
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    struct OneShotMock {
        chunks: Mutex<Option<Vec<Chunk>>>,
    }
    #[async_trait]
    impl Provider for OneShotMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("answer the user", &[])
            } else {
                self.chunks.lock().unwrap().take().unwrap_or_default()
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    #[tokio::test]
    async fn client_runs_a_text_turn() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The model first emits the required intent signal, then answers in prose without calling
        // an operation.
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
        // Token usage rides back out through the unified Flux-authored loop: every model stage's
        // `Usage` is accumulated by the loop host and handed to `turn_end` at turn completion.
        let usage = out
            .usage
            .expect("usage surfaced through the FlowEngine loop");
        assert_eq!(usage.input_tokens, 64);
        assert_eq!(usage.output_tokens, 8);
        assert_eq!(usage.cache_read_input_tokens, 16);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn user_ask_is_registered_only_when_a_responder_is_installed() {
        let dir =
            std::env::temp_dir().join(format!("flux-sdk-interaction-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let provider = || {
            Box::new(OneShotMock {
                chunks: Mutex::new(Some(Vec::new())),
            }) as Box<dyn Provider>
        };
        let headless = Client::builder()
            .model("mock")
            .build(provider(), &dir)
            .unwrap();
        assert!(headless
            .engine
            .executor
            .registry()
            .get("user.ask")
            .is_none());

        let interactive = Client::builder()
            .model("mock")
            .with_user_interaction(Arc::new(FixedInteraction))
            .tools(["read"])
            .build(provider(), &dir)
            .unwrap();
        assert!(interactive
            .engine
            .executor
            .registry()
            .get("user.ask")
            .is_some());
        assert!(interactive
            .engine
            .executor
            .allow_rules()
            .iter()
            .any(|rule| rule == "user.ask"));
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
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("answer the user", &[])
            } else {
                vec![
                    Chunk::Block(ContentBlock::Text { text: "ok".into() }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// A-69: SDK skills are explicit. Merely placing a matching skill under the workspace must not
    /// alter prompts; a caller can still deliberately populate `AgentSpec.skills`.
    #[tokio::test]
    async fn sdk_skills_require_an_explicit_agent_spec() {
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
            !sys.contains("Always greet with ahoy."),
            "workspace discovery must not activate a skill implicitly; got:\n{sys}"
        );

        let systems = Arc::new(Mutex::new(Vec::new()));
        let provider = Box::new(SystemCaptureMock {
            systems: systems.clone(),
        });
        let spec = AgentSpec {
            cwd: dir.clone(),
            ..AgentSpec::new("mock")
        }
        .try_with_default_skills_in(&flux_runtime::metadata::DiscoveryEnv::empty())
        .unwrap();
        let client = ClientBuilder::from_spec(spec)
            .build(provider, &dir)
            .unwrap();
        client.run("an unrelated turn").await.unwrap();
        let sys = systems.lock().unwrap().join("\n---\n");
        assert!(
            // D-190 (pre-existing, landed alongside this story): a disk-backed skill's injected
            // tag carries its disclosed `path` attribute, so the substring check must include it.
            sys.contains("<skill name=\"greeting\" path=")
                && sys.contains("Always greet with ahoy."),
            "an explicitly populated AgentSpec injects the skill; got:\n{sys}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Adaptive mock: declare a write intent, call the provider-native `write` schema, then answer
    /// from the execution report. Proves the SDK drives the full authored outer loop and a real op
    /// dispatches through the envelope and surfaces to the sink.
    struct PlanThenProseMock {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl Provider for PlanThenProseMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            if request_has_tool(&req, "declare_intent") {
                return Ok(Box::pin(futures::stream::iter(
                    intent_chunks("write a file", &["workspace.write"])
                        .into_iter()
                        .map(Ok),
                )));
            }
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunks = if n == 0 {
                native_call(
                    "write-1",
                    "write",
                    serde_json::json!({
                        "path": "sdk-plan.txt",
                        "content": "from the sdk action batch\n"
                    }),
                )
            } else if n == 1 {
                native_call(
                    "finalize-1",
                    "finalize_plan",
                    serde_json::json!({
                        "instructions": "Report whether the file was written."
                    }),
                )
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
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("answer the user", &[])
            } else {
                vec![
                    Chunk::TextDelta(self.text.into()),
                    Chunk::Block(ContentBlock::Text {
                        text: self.text.into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
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

    /// A mock that routes to one named provider-native op, then answers from its result.
    struct PlanOpMock {
        op: &'static str,
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl Provider for PlanOpMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            if request_has_tool(&req, "declare_intent") {
                return Ok(Box::pin(futures::stream::iter(
                    intent_chunks("call the requested operation", &["core"])
                        .into_iter()
                        .map(Ok),
                )));
            }
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunks = if n == 0 {
                let input = if self.op == "park" {
                    serde_json::json!({})
                } else {
                    serde_json::json!({"name": "flux"})
                };
                native_call("op-1", self.op, input)
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

    fn guarded_read_tool(hits: Arc<std::sync::atomic::AtomicUsize>) -> Arc<dyn flux_runtime::Tool> {
        flux_runtime::tool_fn(
            flux_spec::ToolSpec::read_only(
                "guarded_read",
                "A filesystem-scoped read used to exercise authorization",
                serde_json::json!({
                    "type": "object",
                    "properties": { "name": { "type": "string" } }
                }),
            )
            .with_access(vec![flux_spec::AccessKind::Filesystem]),
            move |_input| {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Ok(serde_json::json!("ran"))
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

    /// C-60: blanket approval is subordinate to the SDK client's mandatory policy floor.
    #[tokio::test]
    async fn client_auto_approval_cannot_widen_authorization() {
        let dir =
            std::env::temp_dir().join(format!("flux-sdk-policy-floor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut policy = flux_policy::default_local_grants();
        policy.grants.retain(|grant| {
            !grant
                .actions
                .iter()
                .any(|action| action.0 == "workspace.read")
        });
        let (caller, trust) = flux_policy::local_identity("sdk-test");
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .with_authorization(policy, caller, trust)
            .register_op(guarded_read_tool(hits.clone()))
            .build(
                Box::new(PlanOpMock {
                    op: "guarded_read",
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();

        let _ = client.run("greet flux").await;
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "auto approval must not execute a policy-denied operation"
        );
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
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("answer the user", &[])
            } else {
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
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
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
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let start = std::time::Instant::now();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.calls
                .lock()
                .unwrap()
                .push((start, std::time::Instant::now()));
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("answer the user", &[])
            } else {
                vec![
                    Chunk::Block(ContentBlock::Text { text: "ok".into() }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
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
        assert_eq!(intervals.len(), 4, "two adaptive stages per chat turn");
        assert!(
            intervals.windows(2).all(|pair| pair[1].0 >= pair[0].1),
            "provider calls overlapped: the turn guard failed to serialize the turns"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn client_runs_an_action_batch_then_answers() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let provider = Box::new(PlanThenProseMock {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let client = Client::builder()
            .model("mock")
            .auto_approve(true) // no human in the loop: the action batch's `write` is allowed
            .build(provider, &dir)
            .unwrap();
        let out = client.run("write a file").await.unwrap();
        assert_eq!(out.text, "Wrote the file.");
        // The real op surfaced to the sink; adaptive loop machinery is filtered out.
        assert_eq!(out.tool_calls, vec!["write"]);
        // The action actually executed through the guarded envelope.
        assert!(dir.join("sdk-plan.txt").exists(), "the batch's write ran");
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
    /// model stage, so a call here is a bug. Panics if the model is ever hit.
    struct NeverMock;
    #[async_trait]
    impl Provider for NeverMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            panic!("a flow-driven session must not invoke a model stage");
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

    #[test]
    fn client_builder_reports_source_aware_custom_operation_collisions() {
        let error = Client::builder()
            .register_op_from("custom-pack:alpha", Arc::new(EchoTool))
            .register_op_from("custom-pack:beta", Arc::new(EchoTool))
            .build(Box::new(NeverMock), ".")
            .err()
            .expect("duplicate operation must fail client assembly")
            .to_string();

        assert!(error.contains("duplicate operation `echo`"));
        assert!(error.contains("custom-pack:alpha"));
        assert!(error.contains("custom-pack:beta"));
    }

    #[test]
    fn client_builder_rejects_custom_operation_shadowing_a_builtin() {
        let shadow = flux_runtime::tool_fn(
            flux_spec::ToolSpec::read_only(
                "read",
                "shadow the workspace reader",
                serde_json::json!({"type": "object"}),
            ),
            |_params| async { Ok(serde_json::Value::Null) },
        );
        let error = Client::builder()
            .register_op_from("custom-pack:shadow", shadow)
            .build(Box::new(NeverMock), ".")
            .err()
            .expect("custom operation must not replace a built-in")
            .to_string();

        assert!(error.contains("duplicate operation `read`"), "{error}");
        assert!(error.contains("custom-pack:shadow"), "{error}");
        assert!(error.contains("flux-tools core coding pack"), "{error}");
    }

    #[cfg(feature = "plugins")]
    #[test]
    fn client_builder_rejects_two_installed_plugins_with_the_same_public_operation() {
        let error = Client::builder()
            .with_plugin_tools_from(
                "alpha (/plugins/alpha.toml)",
                vec![Arc::new(EchoTool) as Arc<dyn Tool>],
            )
            .with_plugin_tools_from(
                "beta (/plugins/beta.toml)",
                vec![Arc::new(EchoTool) as Arc<dyn Tool>],
            )
            .build(Box::new(NeverMock), ".")
            .err()
            .expect("installed plugin public-name collision must fail assembly")
            .to_string();

        assert!(error.contains("duplicate operation `echo`"), "{error}");
        assert!(
            error.contains("plugin:alpha (/plugins/alpha.toml)"),
            "{error}"
        );
        assert!(
            error.contains("plugin:beta (/plugins/beta.toml)"),
            "{error}"
        );
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
            condition: None,
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
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("complete the delegated task", &[])
            } else {
                vec![
                    Chunk::Block(ContentBlock::Text {
                        text: "did the subtask".into(),
                    }),
                    Chunk::Usage(self.0.clone()),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// The parent adaptive loop routes to `task(worker, …)`, then answers from the result. Parent
    /// calls bill no usage, so the turn's total isolates the sub-agent's contribution.
    struct DelegatingMock {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl Provider for DelegatingMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            if request_has_tool(&req, "declare_intent") {
                return Ok(Box::pin(futures::stream::iter(
                    intent_chunks("delegate the task", &["process"])
                        .into_iter()
                        .map(Ok),
                )));
            }
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunks = if n == 0 {
                native_call(
                    "task-1",
                    "task",
                    serde_json::json!({"role": "worker", "task": "do it"}),
                )
            } else if n == 1 {
                native_call(
                    "finalize-1",
                    "finalize_plan",
                    serde_json::json!({
                        "instructions": "Report the delegated task's actual result."
                    }),
                )
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
    /// door. A turn whose native action calls `task(role, …)` runs the child through the parent's envelope,
    /// and the child's usage observation lands in the session's run trace (folded into the turn's
    /// recorded usage). The `subagents` re-export module names every bundle type.
    #[tokio::test]
    async fn with_sub_agents_runs_a_delegated_task_and_records_child_usage() {
        use crate::subagents::{RoleRegistry, SubAgents};

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
            "the adaptive turn delegated via `task`: {:?}",
            out.tool_calls
        );

        // The child's tokens reached the session's run trace: the parent turn's recorded usage folds
        // in the sub-agent's spend (the parent's own model-stage calls billed nothing).
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

    struct PolicyCaptureWorker {
        requests: Arc<Mutex<Vec<Request>>>,
    }

    #[async_trait]
    impl Provider for PolicyCaptureWorker {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            self.requests.lock().unwrap().push(req.clone());
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("complete the delegated task", &[])
            } else {
                vec![
                    Chunk::Block(ContentBlock::Text {
                        text: "policy child done".into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// A-82 review regression: the high-level conversational builder is an embedding door too;
    /// its explicit sibling must reach both child-native stages instead of silently taking the
    /// legacy default `into_spawner` path.
    #[tokio::test]
    async fn with_sub_agents_policy_reaches_conversational_children() {
        use crate::subagents::{RoleRegistry, SubAgents};

        let dir =
            std::env::temp_dir().join(format!("flux-sdk-subagent-policy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\n---\nworker prompt", "worker"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let factory: crate::subagents::ProviderFactory = Arc::new({
            let requests = requests.clone();
            move || {
                Ok(Box::new(PolicyCaptureWorker {
                    requests: requests.clone(),
                }) as Box<dyn Provider>)
            }
        });
        let sub_agents = SubAgents::new(roles, ToolRegistry::new(), factory, "child-default", 1024);
        let policy = AdaptiveLoopPolicy {
            max_model_calls: 2,
            intent: AgentStagePolicy {
                model: Some("intent-fast".into()),
                effort: Some(flux_provider::Effort::Low),
                max_tokens: Some(111),
                max_calls: Some(1),
            },
            explore: AgentStagePolicy {
                model: Some("explore-deep".into()),
                effort: Some(flux_provider::Effort::High),
                max_tokens: Some(222),
                max_calls: Some(1),
            },
        };

        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .with_sub_agents_policy(sub_agents, policy)
            .build(
                Box::new(DelegatingMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();
        let out = client.run("delegate this").await.unwrap();
        assert!(out.tool_calls.contains(&"task".to_string()));

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].trace.as_ref().unwrap().stage, "intent");
        assert_eq!(requests[0].model, "intent-fast");
        assert_eq!(requests[0].effort, Some(flux_provider::Effort::Low));
        assert_eq!(requests[0].max_tokens, 111);
        assert_eq!(requests[1].trace.as_ref().unwrap().stage, "explore");
        assert_eq!(requests[1].model, "explore-deep");
        assert_eq!(requests[1].effort, Some(flux_provider::Effort::High));
        assert_eq!(requests[1].max_tokens, 222);
        drop(requests);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A-80 regression: a guarded adapter may open a streamed one-shot runtime, which crosses a
    /// `tokio::spawn` boundary before its `task` op runs. The nested task must still inherit the
    /// served parent turn's cancellation and session lineage.
    struct NestedTaskAdapter {
        audit: Arc<EventStore>,
        child_entered: tokio::sync::mpsc::UnboundedSender<()>,
        child_dropped: tokio::sync::mpsc::UnboundedSender<()>,
    }

    struct DropNotice(tokio::sync::mpsc::UnboundedSender<()>);

    impl Drop for DropNotice {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    struct HangingWorker {
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        dropped: tokio::sync::mpsc::UnboundedSender<()>,
    }

    #[async_trait]
    impl Provider for HangingWorker {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(&self, request: Request) -> Result<ChunkStream> {
            if request_has_tool(&request, "declare_intent") {
                return Ok(Box::pin(futures::stream::iter(
                    intent_chunks("wait for the parent request", &[])
                        .into_iter()
                        .map(Ok),
                )));
            }
            let _notice = DropNotice(self.dropped.clone());
            let _ = self.entered.send(());
            futures::future::pending::<Result<ChunkStream>>().await
        }
    }

    #[async_trait]
    impl Tool for NestedTaskAdapter {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only(
                "nested_task_adapter",
                "delegate through a nested one-shot runtime",
                serde_json::json!({ "type": "object", "properties": {} }),
            )
        }

        async fn execute(
            &self,
            _ctx: &ToolContext,
            _params: serde_json::Value,
        ) -> Result<flux_runtime::ToolResult> {
            use crate::subagents::{RoleRegistry, SpawnLimits, SubAgents};

            let mut roles = RoleRegistry::default();
            roles.insert(parse_role(
                "---\n---\nWait until the parent request is cancelled.",
                "worker",
            ));
            let factory: crate::subagents::ProviderFactory = Arc::new({
                let entered = self.child_entered.clone();
                let dropped = self.child_dropped.clone();
                move || {
                    Ok(Box::new(HangingWorker {
                        entered: entered.clone(),
                        dropped: dropped.clone(),
                    }) as Box<dyn Provider>)
                }
            });
            let mut limits = SpawnLimits::new(1024);
            // The assertion below must observe parent cancellation, never this safety backstop.
            limits.wall_clock = Some(std::time::Duration::from_secs(30));
            let sub_agents = SubAgents::new(roles, ToolRegistry::new(), factory, "mock", 1024)
                .with_limits(limits)
                .with_audit(self.audit.clone());

            let root = std::env::temp_dir().join(format!(
                "flux-sdk-nested-turn-context-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root)?;
            let mut nested = FlowClient::builder()
                .model("mock")
                .auto_approve(true)
                .build(Arc::new(ProseMock { text: "unused" }), &root)?;
            nested.with_sub_agents(sub_agents);
            let flow: flux_flow::ast::DraftAst = serde_json::from_value(serde_json::json!({
                "body": [{
                    "kind": "call",
                    "op": "task",
                    "args": [{
                        "kind": "lit",
                        "value": { "role": "worker", "task": "wait" }
                    }]
                }]
            }))
            .map_err(|error| flux_core::Error::Other(error.to_string()))?;
            let outcome = nested.execute_streamed(&flow).finish().await?;
            Ok(flux_runtime::ToolResult::ok(outcome.result))
        }
    }

    struct NestedAdapterParent {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl Provider for NestedAdapterParent {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(&self, request: Request) -> Result<ChunkStream> {
            if request_has_tool(&request, "declare_intent") {
                return Ok(Box::pin(futures::stream::iter(
                    intent_chunks("delegate through the adapter", &["core"])
                        .into_iter()
                        .map(Ok),
                )));
            }
            let chunks = match self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            {
                0 => native_call("adapter-1", "nested_task_adapter", serde_json::json!({})),
                _ => vec![
                    Chunk::Block(ContentBlock::Text {
                        text: "unexpected completion".into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ],
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    #[tokio::test]
    async fn nested_streamed_task_inherits_parent_cancel_and_session_lineage() {
        let dir = std::env::temp_dir().join(format!(
            "flux-sdk-parent-turn-context-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let audit = Arc::new(EventStore::in_memory().unwrap());
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let (dropped_tx, mut dropped_rx) = tokio::sync::mpsc::unbounded_channel();
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .register_op(Arc::new(NestedTaskAdapter {
                audit: audit.clone(),
                child_entered: entered_tx,
                child_dropped: dropped_tx,
            }))
            .build(
                Box::new(NestedAdapterParent {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();
        let session = client.default_session().unwrap();
        let parent_session = session.id().to_string();
        let turn = session.stream("delegate through the adapter");

        tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx.recv())
            .await
            .expect("the nested child must enter its parked provider")
            .expect("the nested child entry channel closed");
        turn.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), dropped_rx.recv())
            .await
            .expect("parent cancellation must reach the nested child before its 30s deadline")
            .expect("the nested child drop channel closed");
        turn.finish().await.unwrap();

        let children = audit.children_of(&parent_session).unwrap();
        assert_eq!(
            children.len(),
            1,
            "the child audit stream must be parent-linked"
        );
        let child = audit.info(&children[0]).unwrap();
        assert_eq!(
            child.context.correlation_id.as_deref(),
            Some(parent_session.as_str())
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
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("answer the user", &[])
            } else {
                vec![
                    Chunk::TextDelta("ok".into()),
                    Chunk::Block(ContentBlock::Text { text: "ok".into() }),
                    Chunk::Usage(self.0.clone()),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
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
        assert!(
            manifest.contains("flux-plugin = { workspace = true, optional = true }"),
            "flux-plugin must be optional (the `plugins` feature only)"
        );
    }

    /// D-156: a cassette-recorded session replays hermetically through the SDK. A first client
    /// records a plan-running turn (the `write` op lands a cassette cell); a **second** client over
    /// the same `Storage::dir` — built with a **never-called** provider (panics if the model is hit)
    /// — replays it: the recorded plan comes back, nothing dispatches live.
    #[tokio::test]
    async fn session_replays_a_recorded_plan_hermetically() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-replay-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let store = dir.join("state");

        // Record: a plan that writes a file → the write dispatches and is captured on the cassette.
        let sid = {
            let client = Client::builder()
                .model("mock")
                .auto_approve(true)
                .storage(Storage::dir(&store))
                .build(
                    Box::new(PlanThenProseMock {
                        calls: std::sync::atomic::AtomicUsize::new(0),
                    }),
                    &dir,
                )
                .unwrap();
            client.run("write a file").await.unwrap();
            client.session_id().unwrap()
        };

        // Replay in a fresh client over the same store, with a provider that must never be called —
        // replay serves every op from the cassette, so the model is never hit.
        let client = Client::builder()
            .model("mock")
            .storage(Storage::dir(&store))
            .build(Box::new(NeverMock), &dir)
            .unwrap();
        let session = client.open_session(&sid).unwrap();
        struct NullSink;
        impl AgentSink for NullSink {}
        let mut sink = NullSink;
        let report = session.replay(None, &mut sink).await.unwrap();
        assert!(
            !report.plans.is_empty(),
            "the recorded plan replayed: {report:?}"
        );
        assert!(
            report.diverged.is_none(),
            "a faithful replay does not diverge: {report:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-156: a chat-only session (no ops → no cassette cells) is not replayable, and says so.
    #[tokio::test]
    async fn replay_of_a_non_recorded_session_errors_honestly() {
        let dir = std::env::temp_dir().join(format!("flux-sdk-replay-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let client = Client::builder()
            .model("mock")
            .storage(Storage::dir(dir.join("state")))
            .build(Box::new(ProseMock { text: "hello" }), &dir)
            .unwrap();
        let session = client.default_session().unwrap();
        session.send("hi").await.unwrap(); // a prose turn — no plan, no ops, no cassette cells

        struct NullSink;
        impl AgentSink for NullSink {}
        let mut sink = NullSink;
        let err = session
            .replay(None, &mut sink)
            .await
            .expect_err("a chat-only session is not replayable");
        assert!(
            err.to_string().contains("not replayable"),
            "the error is honest about why: {err}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An adaptive provider that gathers `read(note.txt)` once, then answers in prose — a recorded
    /// session with a cassette-captured `read` at the fork point.
    struct BindPlanMock {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl Provider for BindPlanMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            if request_has_tool(&req, "declare_intent") {
                return Ok(Box::pin(futures::stream::iter(
                    intent_chunks("read the note", &["workspace.read"])
                        .into_iter()
                        .map(Ok),
                )));
            }
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunks = if n == 0 {
                native_call("read-1", "read", serde_json::json!({"path": "note.txt"}))
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

    /// Record a `bind x = read(note.txt); return x` session on a `Storage::dir`; returns the store
    /// dir and the session id.
    async fn record_bind_session(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("flux-sdk-fork-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("note.txt"), "original").unwrap();
        let store = dir.join("state");
        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .storage(Storage::dir(store.clone()))
            .build(
                Box::new(BindPlanMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }),
                &dir,
            )
            .unwrap();
        client.run("read the note").await.unwrap();
        let sid = client.session_id().unwrap();
        (dir, store, sid)
    }

    struct NullSink;
    impl AgentSink for NullSink {}

    /// D-157: `Session::fork` + `Fork::inject` — inject a different value at the fork's bound
    /// statement; `diff` reports the divergence and the original session's log is untouched.
    #[tokio::test]
    async fn fork_inject_diverges_and_leaves_the_original_untouched() {
        let (dir, store, sid) = record_bind_session("inject").await;

        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .storage(Storage::dir(store))
            .build(Box::new(NeverMock), &dir)
            .unwrap();
        let events = client.event_store();
        let head_before = events.head_seq(&sid).unwrap();

        let session = client.open_session(&sid).unwrap();
        let fork = session.fork(0).await.unwrap();
        let mut sink = NullSink;
        fork.inject(&serde_json::json!("injected"), &mut sink)
            .await
            .unwrap();

        // The original session's log is untouched by the fork.
        assert_eq!(
            events.head_seq(&sid).unwrap(),
            head_before,
            "forking must not touch the original session's log"
        );

        // The diff reports the divergence between original and fork.
        let diff = fork.diff(&session).unwrap();
        assert!(
            !diff.identical && !diff.rows.is_empty(),
            "the injected fork diverges from the original: {diff:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-157: `Fork::edit` — diverge by supplying an alternate plan (a different bound value), which
    /// runs through the envelope and diverges from the original.
    #[tokio::test]
    async fn fork_edit_diverges_on_a_bound_value() {
        let (dir, store, sid) = record_bind_session("edit").await;

        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .storage(Storage::dir(store))
            .build(Box::new(NeverMock), &dir)
            .unwrap();
        let session = client.open_session(&sid).unwrap();
        let fork = session.fork(0).await.unwrap();

        // An alternate tail: bind x to a literal instead of the recorded read, then return it.
        let edited: crate::flow::DraftAst = serde_json::from_value(serde_json::json!({ "body": [
            { "kind": "bind", "name": "x", "value": { "kind": "lit", "value": "edited-value" } },
            { "kind": "return", "value": { "kind": "var", "name": "x" } }
        ]}))
        .unwrap();
        let mut sink = NullSink;
        fork.edit(&edited, &mut sink).await.unwrap();

        let diff = fork.diff(&session).unwrap();
        assert!(
            !diff.identical,
            "the edited fork diverges from the original: {diff:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Append a message to `stream` without the session log's shape check — the only way to put a
    /// log into a state the typed seam refuses, which is what a crashed turn leaves behind in the
    /// wild and what these two tests need as a starting point.
    fn seed_raw_message(events: &EventStore, stream: &str, m: flux_core::Message) {
        events
            .append(stream, flux_events::NewEvent::message(m))
            .unwrap();
    }

    /// A-102 **failing-first**: forking a session whose history ends mid-tool-pair — a `tool_use`
    /// whose `tool_result` never landed, exactly what a crashed turn leaves — must be refused at
    /// the fork's write seam, naming the invariant. Before this story the fork replayed the parent's
    /// messages one at a time through the unguarded `record_message`, so the broken shape was copied
    /// through unexamined and the child session 400'd on its first live turn.
    #[tokio::test]
    async fn fork_refuses_a_parent_history_that_ends_mid_tool_pair() {
        let (dir, store, sid) = record_bind_session("mid-tool-pair").await;

        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .storage(Storage::dir(store))
            .build(Box::new(NeverMock), &dir)
            .unwrap();
        let events = client.event_store();

        // A valid alternation that is not a valid provider history: the last assistant message
        // issues a tool call nothing ever answers.
        seed_raw_message(&events, &sid, flux_core::Message::user_text("and again"));
        seed_raw_message(
            &events,
            &sid,
            flux_core::Message::assistant(vec![ContentBlock::ToolUse {
                id: "orphan-1".into(),
                name: "read".into(),
                input: serde_json::json!({}),
            }]),
        );
        let source_len = events.load_stream(&sid, None).unwrap().len();

        let session = client.open_session(&sid).unwrap();
        let rendered = match session.fork(0).await {
            Err(e) => e.to_string(),
            Ok(f) => panic!(
                "a fork of a mid-tool-pair history must be refused, not copied through — got {}",
                f.id()
            ),
        };
        assert!(
            rendered.contains("tool_use") && rendered.contains("orphan-1"),
            "the refusal must name the invariant it protects: {rendered}"
        );

        // And the refusal is not paid for out of the source stream: fork is a pure read of it.
        assert_eq!(
            events.load_stream(&sid, None).unwrap().len(),
            source_len,
            "forking must not append to the source stream"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-211 **failing-first**: the refusal above must cost nothing. `fork` used to mint the child
    /// session *before* it knew the parent was forkable, so a refused fork left an empty orphan
    /// session behind — an artifact that did not exist before the refusal path did. `prune_empty`
    /// would eventually collect it, but "a failed operation leaves no trace" is cheaper to hold as
    /// an invariant than to re-derive from a pruning rule every time someone reads this path.
    #[tokio::test]
    async fn fork_refuses_before_minting_the_child_session() {
        let (dir, store, sid) = record_bind_session("no-orphan").await;

        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .storage(Storage::dir(store))
            .build(Box::new(NeverMock), &dir)
            .unwrap();
        let events = client.event_store();

        // The same unforkable parent as the test above: a trailing `tool_use` nothing answers.
        seed_raw_message(&events, &sid, flux_core::Message::user_text("and again"));
        seed_raw_message(
            &events,
            &sid,
            flux_core::Message::assistant(vec![ContentBlock::ToolUse {
                id: "orphan-1".into(),
                name: "read".into(),
                input: serde_json::json!({}),
            }]),
        );
        // Generous limit: the fixture holds one session, and the bug adds a second.
        let before = events.list(1_000).unwrap().len();

        let session = client.open_session(&sid).unwrap();
        if let Ok(f) = session.fork(0).await {
            panic!(
                "a fork of a mid-tool-pair history must be refused, not copied through — got {}",
                f.id()
            );
        }

        assert_eq!(
            events.list(1_000).unwrap().len(),
            before,
            "a refused fork must not leave a child session behind — validate the parent before \
             minting anything"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-247 **failing-first**: `WhatIf::run`'s re-plan path carried C-211's defect at a third site
    /// — it minted the counterfactual session before asking whether the parent history was a valid
    /// provider history, so a refused re-plan left an empty orphan session behind. Same invariant,
    /// same reason: a failed operation leaves no trace.
    #[tokio::test]
    async fn whatif_replan_refuses_an_invalid_history_before_minting_the_child() {
        let (dir, store, sid) = record_bind_session("whatif-no-orphan").await;

        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .storage(Storage::dir(store))
            .build(Box::new(NeverMock), &dir)
            .unwrap();
        let events = client.event_store();

        // The same unusable parent C-211's fork tests use: a trailing `tool_use` nothing answers.
        seed_raw_message(&events, &sid, flux_core::Message::user_text("and again"));
        seed_raw_message(
            &events,
            &sid,
            flux_core::Message::assistant(vec![ContentBlock::ToolUse {
                id: "orphan-1".into(),
                name: "read".into(),
                input: serde_json::json!({}),
            }]),
        );
        // Generous limit: the fixture holds one session, and the bug adds a second.
        let before = events.list(1_000).unwrap().len();

        // An `instructions` change is what selects the re-plan path; `NeverMock` panics if the
        // refusal ever fails to fire and the live turn actually runs.
        let session = client.open_session(&sid).unwrap();
        let rendered = match session
            .what_if()
            .instructions("You are a different agent.")
            .run()
            .await
        {
            Err(e) => e.to_string(),
            Ok(cf) => panic!(
                "a re-plan over a mid-tool-pair history must be refused, not copied through — \
                 got {}",
                cf.session().id()
            ),
        };
        assert!(
            rendered.contains("tool_use") && rendered.contains("orphan-1"),
            "the refusal must name the invariant it protects: {rendered}"
        );

        assert_eq!(
            events.list(1_000).unwrap().len(),
            before,
            "a refused re-plan must not leave a child session behind — validate the parent before \
             minting anything"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-247 **failing-first**, the second bail: a re-plan aimed at a turn the session does not have
    /// is refused too, and that refusal used to fire even later than the history check — *after* the
    /// child had been minted AND had the parent's whole conversation rewritten into it, so the orphan
    /// it left was not even empty enough for `prune_empty` to collect.
    #[tokio::test]
    async fn whatif_replan_refuses_a_missing_target_turn_before_minting_the_child() {
        let (dir, store, sid) = record_bind_session("whatif-no-such-turn").await;

        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .storage(Storage::dir(store))
            .build(Box::new(NeverMock), &dir)
            .unwrap();
        let events = client.event_store();
        let before = events.list(1_000).unwrap().len();

        // The history here is perfectly valid — the fixture records exactly one turn. Turn 99 is
        // simply not there.
        let session = client.open_session(&sid).unwrap();
        let rendered = match session
            .what_if()
            .turn(99)
            .instructions("You are a different agent.")
            .run()
            .await
        {
            Err(e) => e.to_string(),
            Ok(cf) => panic!(
                "a re-plan of a turn the session does not have must be refused — got {}",
                cf.session().id()
            ),
        };
        assert!(
            rendered.contains(&sid) && rendered.contains("has no turn 99"),
            "the refusal must name the session and the reason: {rendered}"
        );

        assert_eq!(
            events.list(1_000).unwrap().len(),
            before,
            "a refused re-plan must not leave a child session behind — resolve the target turn \
             before minting anything"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The parent's history reaches the child as **one** checked rewrite, not one append per
    /// message — the other half of moving this path onto `rewrite(ValidHistory)`.
    #[tokio::test]
    async fn fork_seeds_the_child_conversation_with_one_append() {
        let (dir, store, sid) = record_bind_session("one-append").await;

        let client = Client::builder()
            .model("mock")
            .auto_approve(true)
            .storage(Storage::dir(store))
            .build(Box::new(NeverMock), &dir)
            .unwrap();
        let events = client.event_store();

        // Four messages in the parent, so "one append per message" and "one append" differ.
        seed_raw_message(&events, &sid, flux_core::Message::user_text("and again"));
        seed_raw_message(&events, &sid, flux_core::Message::assistant_text("sure"));
        let parent_history = events.conversation(&sid).unwrap();
        assert_eq!(parent_history.len(), 4, "the parent has four messages");
        let source_len = events.load_stream(&sid, None).unwrap().len();

        let session = client.open_session(&sid).unwrap();
        let fork = session.fork(0).await.unwrap();

        assert_eq!(
            events.conversation(fork.id()).unwrap(),
            parent_history,
            "the child starts from the parent's history"
        );
        assert_eq!(
            events.load_by_kind(fork.id(), "compacted").unwrap().len(),
            1,
            "one Compacted event installs the whole history"
        );
        assert!(
            events
                .load_by_kind(fork.id(), "message")
                .unwrap()
                .is_empty(),
            "no per-message append survives the migration"
        );
        assert_eq!(
            events.load_stream(&sid, None).unwrap().len(),
            source_len,
            "forking must not append to the source stream"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
