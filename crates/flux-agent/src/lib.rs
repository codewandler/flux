//! `flux-agent` — the Agent pillar: what an *agent* is, and how to assemble one.
//!
//! An agent is a configured instance of the flux-flow engine. This crate owns the **definition** —
//! [`AgentSpec`] (model, persona, skills, tool selection, permissions, settings) and the markdown
//! [`Role`] format — plus the assembler that turns a spec into a running
//! [`FlowEngine`](flux_flow::engine::FlowEngine). The turn loop itself lives in flux-flow (it is a
//! flux-lang program, `agent-loop.flux`); this crate sits *on top of* the engine.

use std::path::PathBuf;
use std::sync::Arc;

use flux_core::{render_knowledge_blocks, ContextBlock, Error, Result};
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
pub use flux_flow::engine::{AgentLoopSpec, BuiltinAgentLoop};
use flux_flow::state::FlowStore;
pub use flux_flow::{AdaptiveLoopPolicy, AgentStagePolicy};
use flux_provider::{Effort, Provider};
use flux_runtime::{
    Approver, ExecutionAuthorization, ExecutionEnvironment, Executor, PermissionManager,
    ToolContext, ToolRegistry,
};
use sha2::{Digest, Sha256};

pub mod role;
#[allow(deprecated)]
pub use role::{parse_role, try_parse_role, Role, RoleRegistry};

/// Harness-owned protocol present on every Flux agent-backed model call.
pub const HARNESS_SYSTEM_PROMPT: &str = include_str!("../assets/prompts/harness-core.md");

/// Coding behavior selected by [`AgentProfile::Coding`].
pub const CODING_PROFILE_PROMPT: &str = include_str!("../assets/prompts/profiles/coding.md");

const READ_TOOL_PROMPT: &str = include_str!("../assets/prompts/tools/read.md");
const EDIT_TOOL_PROMPT: &str = include_str!("../assets/prompts/tools/edit.md");
const SHELL_TOOL_PROMPT: &str = include_str!("../assets/prompts/tools/shell.md");
const TASK_TOOL_PROMPT: &str = include_str!("../assets/prompts/tools/task.md");

/// One built-in sub-agent role shipped with Flux. Repository/user role files override these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinRoleAsset {
    /// Stable role name used by `task`.
    pub name: &'static str,
    /// Short discovery description.
    pub description: &'static str,
    /// Embedded authored instructions layered after the harness protocol.
    pub instructions: &'static str,
}

/// Generic fallback roles embedded in the Flux package rather than supplied by a host repository.
pub const BUILTIN_ROLE_ASSETS: &[BuiltinRoleAsset] = &[
    BuiltinRoleAsset {
        name: "scout",
        description: "Fast read-only codebase reconnaissance",
        instructions: include_str!("../assets/roles/scout.md"),
    },
    BuiltinRoleAsset {
        name: "planner",
        description: "Produce a structured implementation plan",
        instructions: include_str!("../assets/roles/planner.md"),
    },
    BuiltinRoleAsset {
        name: "worker",
        description: "Execute a single well-scoped subtask",
        instructions: include_str!("../assets/roles/worker.md"),
    },
    BuiltinRoleAsset {
        name: "reviewer",
        description: "Review changes for correctness",
        instructions: include_str!("../assets/roles/reviewer.md"),
    },
    BuiltinRoleAsset {
        name: "evaluator",
        description: "Judge whether a goal is satisfied",
        instructions: include_str!("../assets/roles/evaluator.md"),
    },
    BuiltinRoleAsset {
        name: "summarizer",
        description: "Condense a transcript",
        instructions: include_str!("../assets/roles/summarizer.md"),
    },
];

/// Pre-allow/deny rules an agent's executor starts with (the rest gate through the approver).
#[derive(Debug, Default, Clone)]
pub struct Permissions {
    /// Tool/operation rules pre-allowed without prompting (e.g. `"read"`).
    pub allow: Vec<String>,
    /// Rules always denied.
    pub deny: Vec<String>,
}

/// Surface-owned inputs used when [`AgentSpec`] constructs its guarded [`Executor`].
///
/// Keeping the approval handler and dispatch context beside the mandatory authorization profile
/// makes the simple assembly door explicit without duplicating the broader executor builder that
/// richer surfaces use through [`AgentSpec::into_engine`].
#[deprecated(
    since = "0.24.0",
    note = "use flux_runtime::ExecutionEnvironment and AgentSpec::assemble_in; this shim is planned for removal in 0.26"
)]
pub struct AgentExecutorConfig {
    approver: Arc<dyn Approver>,
    context: ToolContext,
    authorization: ExecutionAuthorization,
}

#[allow(deprecated)]
impl AgentExecutorConfig {
    /// Bundle the surface's approval posture, guarded tool context, and authorization floor.
    pub fn new(
        approver: Arc<dyn Approver>,
        context: ToolContext,
        authorization: ExecutionAuthorization,
    ) -> Self {
        Self {
            approver,
            context,
            authorization,
        }
    }
}

/// Default byte budget for injected `context` blocks (A-19); overridable per spec.
pub const DEFAULT_CONTEXT_BUDGET: usize = 8192;

/// Default session size (serialized chars) past which a long-lived agent summarizes older turns
/// (A-22). Non-zero so served / agentic / SDK agents — which bind a conversation to one persistent
/// session and re-send the growing transcript every turn — compact by default instead of growing
/// unbounded until the provider's context window errors. Matches the CLI's `FLUX_COMPACT_CHARS`
/// default so behaviour is consistent across surfaces; override per-agent via
/// [`AgentSpec::with_compaction`] (or, on the served path, the `AgentDecl` settings / env).
///
/// **The value is deliberate, and 48,000 is rarely reached** (C-443). A sweep of a 112,114-event
/// local store found *zero* compactions: 85% of sessions are one-shot runs, and the average
/// multi-turn session is ~9% of this threshold. That is a statement about the workload, not an
/// argument to lower it — a threshold low enough to fire on those sessions would compact almost
/// every real one, spending a provider call and discarding fidelity under no memory pressure.
/// Raising it weakens the unbounded-growth guard this constant exists for.
///
/// **This remains a fixed history budget, not a fraction of a model window** (C-462). Conversation
/// history is only the growing part of a request; harness instructions, skills, tool schemas, and
/// stage prompts consume model-dependent headroom independently. A 5,095-call usage sweep found
/// whole prompts ranging from tiny to hundreds of thousands of tokens, so a model's nominal window
/// does not determine how much transcript is safe or economical to retain. Flux also has no
/// trustworthy context-window metadata for unknown, local, or custom model ids. The fixed cap keeps
/// transcript resend cost and compaction behavior bounded across every provider; deployments that
/// know their workload can tune it with [`AgentSpec::with_compaction`] / `FLUX_COMPACT_CHARS`.
pub const DEFAULT_COMPACT_THRESHOLD_CHARS: usize = 48_000;

/// Optional behavior layered after Flux's mandatory harness protocol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentProfile {
    /// General model-backed work with no coding-specific assumptions.
    #[default]
    General,
    /// The workspace coding lifecycle used by `flux run` and the ordinary SDK builder.
    Coding,
}

/// Semantic role of one system-prompt contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptLayerKind {
    Harness,
    Profile,
    ToolGuidance,
    Instructions,
    RepositoryPolicy,
    WorkspaceSnapshot,
    Knowledge,
    Skill,
    RuntimeNote,
}

impl PromptLayerKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Harness => "harness",
            Self::Profile => "profile",
            Self::ToolGuidance => "tool_guidance",
            Self::Instructions => "instructions",
            Self::RepositoryPolicy => "repository_policy",
            Self::WorkspaceSnapshot => "workspace_snapshot",
            Self::Knowledge => "knowledge",
            Self::Skill => "skill",
            Self::RuntimeNote => "runtime_note",
        }
    }
}

/// Authority class attached to prompt content. This is model context, not an authorization grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTrust {
    Harness,
    AgentAuthor,
    Repository,
    Data,
}

impl PromptTrust {
    fn as_str(self) -> &'static str {
        match self {
            Self::Harness => "harness",
            Self::AgentAuthor => "agent_author",
            Self::Repository => "repository",
            Self::Data => "data",
        }
    }
}

/// Cache lifetime of one prompt contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheClass {
    Static,
    Session,
    Turn,
}

/// One typed system-prompt contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptLayer {
    pub id: String,
    pub kind: PromptLayerKind,
    pub trust: PromptTrust,
    pub cache_class: PromptCacheClass,
    pub source: Option<String>,
    pub captured_at_unix_secs: Option<u64>,
    pub body: String,
}

impl PromptLayer {
    pub fn new(
        id: impl Into<String>,
        kind: PromptLayerKind,
        trust: PromptTrust,
        cache_class: PromptCacheClass,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            trust,
            cache_class,
            source: None,
            captured_at_unix_secs: None,
            body: body.into(),
        }
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn captured_at(mut self, unix_secs: u64) -> Self {
        self.captured_at_unix_secs = Some(unix_secs);
        self
    }

    /// Metadata safe to expose without printing the layer body.
    pub fn manifest(&self) -> PromptManifestEntry {
        let mut hasher = Sha256::new();
        hasher.update(self.body.as_bytes());
        PromptManifestEntry {
            id: self.id.clone(),
            kind: self.kind,
            trust: self.trust,
            cache_class: self.cache_class,
            source: self.source.clone(),
            captured_at_unix_secs: self.captured_at_unix_secs,
            bytes: self.body.len(),
            sha256: hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }
}

/// Body-free provenance for one rendered prompt layer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PromptManifestEntry {
    pub id: String,
    pub kind: PromptLayerKind,
    pub trust: PromptTrust,
    pub cache_class: PromptCacheClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_at_unix_secs: Option<u64>,
    pub bytes: usize,
    pub sha256: String,
}

/// A first-class agent definition: model, persona, skills, tool selection, permissions, and the
/// turn settings — everything that distinguishes one agent from another. Assemble it into a running
/// [`FlowEngine`] with [`AgentSpec::assemble`] (the simple path) or [`AgentSpec::into_engine`] (when
/// the surface builds its own richly-configured [`Executor`]).
#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub model: String,
    /// Optional behavior layered after the mandatory Flux harness protocol.
    pub profile: AgentProfile,
    /// Caller-authored role/persona instructions. These never replace the harness protocol.
    pub instructions: String,
    /// Surface-assembled repository policy, workspace evidence, and other typed context.
    pub prompt_layers: Vec<PromptLayer>,
    /// Skills explicitly enabled for this agent. Each body is injected on every turn; metadata
    /// triggers are discovery hints only and never activate a skill implicitly.
    pub skills: Vec<flux_skill::Skill>,
    /// Tool selection: a subset of the provided registry's ops by name. `None` = every available op.
    pub tools: Option<Vec<String>>,
    /// Pre-allow/deny rules for the safety envelope.
    pub permissions: Permissions,
    pub max_tokens: u32,
    /// Authored decision/batch iterations per turn. Must be between 1 and
    /// [`flux_flow::MAX_AGENT_LOOP_ITERATIONS`], inclusive.
    pub max_iterations: usize,
    /// Ask capable providers/models to expose adaptive thinking for this agent's calls.
    pub thinking: bool,
    /// Provider-mapped reasoning effort applied to every model call this agent owns.
    pub effort: Option<Effort>,
    /// The explicit Flux-Lang outer loop. Defaults to the shipped adaptive preset.
    pub agent_loop: AgentLoopSpec,
    /// Evidence-gated tool groups (empty disables gating — every op advertised).
    pub groups: Vec<flux_evidence::ToolGroup>,
    /// Built-in intent/exploration cognition policy, including the logical-run model-call ceiling.
    pub adaptive_policy: AdaptiveLoopPolicy,
    /// Session-ambient group-surfacing signals (D-115): host-known facts the per-turn workspace
    /// walk can't see — e.g. the CLI injects `endpoint` when its startup-loaded endpoints store
    /// is non-empty. Appended to every turn's probed signals; surfacing is sticky-monotonic, so
    /// startup-static values are enough. Empty by default.
    pub ambient_signals: Vec<String>,
    /// Summarize older turns once the persisted session exceeds this many chars (`0` disables it).
    pub compact_threshold_chars: usize,
    /// Workspace root, re-probed each turn for tool-surfacing signals.
    pub cwd: PathBuf,
    /// Knowledge blocks injected inline into the system prompt as `<knowledge-base>` sections (A-19).
    /// Empty by default; rendered after the authored prompt layers, bounded by `context_budget`. This is the
    /// "grounded knowledge" path — small KBs handed to the model directly, no retrieval round-trip.
    pub context: Vec<ContextBlock>,
    /// Byte budget for rendered `context` (`0` = unbounded). Over-budget blocks truncate with a marker.
    pub context_budget: usize,
    /// D-188: the opt-in model-invoked skill catalog — every discovered skill eligible for
    /// on-demand `skill.load` (already filtered to exclude `disable-model-invocation: true`
    /// skills). Empty (the default) means the mode is off: no catalog is surfaced in the system
    /// prompt and `skill.load` stays out of the advertised op set, so an agent that never touches
    /// this field behaves byte-identically to before D-188. Distinct from `skills`, the explicit
    /// `--skill`-style active set — populate this via [`Self::try_with_model_invoked_skills`].
    pub model_invoked_skills: Vec<flux_skill::Skill>,
}

impl Default for AgentSpec {
    fn default() -> Self {
        AgentSpec {
            model: String::new(),
            profile: AgentProfile::Coding,
            instructions: String::new(),
            prompt_layers: Vec::new(),
            skills: Vec::new(),
            tools: None,
            permissions: Permissions::default(),
            max_tokens: 4096,
            max_iterations: flux_flow::DEFAULT_AGENT_LOOP_ITERATIONS,
            thinking: false,
            effort: None,
            agent_loop: AgentLoopSpec::default(),
            groups: Vec::new(),
            adaptive_policy: AdaptiveLoopPolicy::default(),
            ambient_signals: Vec::new(),
            compact_threshold_chars: DEFAULT_COMPACT_THRESHOLD_CHARS,
            cwd: PathBuf::from("."),
            context: Vec::new(),
            context_budget: DEFAULT_CONTEXT_BUDGET,
            model_invoked_skills: Vec::new(),
        }
    }
}

impl AgentSpec {
    /// A coding-agent spec for `model`. Kept as the ordinary SDK/CLI default.
    pub fn new(model: impl Into<String>) -> Self {
        Self::coding(model)
    }

    /// A coding-agent spec: mandatory harness protocol plus the coding profile.
    pub fn coding(model: impl Into<String>) -> Self {
        AgentSpec {
            model: model.into(),
            ..Self::default()
        }
    }

    /// A general agent: mandatory harness protocol plus caller-authored instructions.
    pub fn general(model: impl Into<String>, instructions: impl Into<String>) -> Self {
        AgentSpec {
            model: model.into(),
            profile: AgentProfile::General,
            instructions: instructions.into(),
            ..Self::default()
        }
    }

    /// Append one typed context contribution after the profile and authored instructions.
    pub fn with_prompt_layer(mut self, layer: PromptLayer) -> Self {
        self.prompt_layers.push(layer);
        self
    }

    /// Explicitly enable every skill from the guarded project and trusted user-global default
    /// directories rooted at this spec's `cwd`. Set `cwd` first. Most callers should select named
    /// skills instead of enabling the whole set.
    ///
    /// The user-global half is rooted at the **process**'s `HOME`; tests pin it with
    /// [`Self::try_with_default_skills_in`].
    pub fn try_with_default_skills(self) -> Result<Self> {
        let env = flux_runtime::metadata::DiscoveryEnv::from_process();
        self.try_with_default_skills_in(&env)
    }

    /// [`Self::try_with_default_skills`] against an explicit
    /// [`DiscoveryEnv`](flux_runtime::metadata::DiscoveryEnv) rather than the process's own (C-393).
    ///
    /// Discovery walks `~/.flux/skills`, `~/.agents/skills` and `~/.claude/skills` in addition to
    /// the project roots, so a test going through the process-reading form asserts against whatever
    /// the developer keeps in their own home. Same value-held-env idiom as `load_config_in`
    /// (C-332), `router_in` (C-392) and `DiscoveryEnv` itself (C-297) — purely additive.
    pub fn try_with_default_skills_in(
        mut self,
        env: &flux_runtime::metadata::DiscoveryEnv,
    ) -> Result<Self> {
        self.skills = flux_runtime::metadata::discover_skills_in(&self.cwd, &[], env)?.skills;
        Ok(self)
    }

    /// Compatibility wrapper for the former infallible builder. Guard failures are intentionally
    /// loud; new code should propagate [`Self::try_with_default_skills`].
    #[deprecated(note = "use try_with_default_skills and propagate project metadata failures")]
    pub fn with_default_skills(self) -> Self {
        self.try_with_default_skills()
            .expect("guarded default skill discovery failed")
    }

    /// Opt into Claude-style progressive skill disclosure (D-188): discover every skill under this
    /// spec's `cwd` (set `cwd` first) and enable model-invoked on-demand loading for all of them
    /// except those marked `disable-model-invocation: true`. Distinct from — and additive to —
    /// [`Self::skills`](Self)/[`Self::try_with_default_skills`], which stays the explicit
    /// always-injected activation surface; this only surfaces name+description up front and loads
    /// a body when the model calls `skill.load`.
    pub fn try_with_model_invoked_skills(self) -> Result<Self> {
        let env = flux_runtime::metadata::DiscoveryEnv::from_process();
        self.try_with_model_invoked_skills_in(&env)
    }

    /// [`Self::try_with_model_invoked_skills`] against an explicit
    /// [`DiscoveryEnv`](flux_runtime::metadata::DiscoveryEnv) rather than the process's own (C-393)
    /// — the model-invoked twin of [`Self::try_with_default_skills_in`], and for the same reason.
    pub fn try_with_model_invoked_skills_in(
        mut self,
        env: &flux_runtime::metadata::DiscoveryEnv,
    ) -> Result<Self> {
        self.model_invoked_skills =
            flux_runtime::metadata::discover_skills_in(&self.cwd, &[], env)?
                .skills
                .into_iter()
                .filter(|skill| !skill.disable_model_invocation)
                .collect();
        Ok(self)
    }

    /// Set the compaction threshold (serialized chars) — the size past which older turns are
    /// summarized before the next request (A-22). `0` disables compaction (a one-shot / short-turn
    /// agent that must never compact). Chainable; this is the per-agent override that wins over the
    /// non-zero [`DEFAULT_COMPACT_THRESHOLD_CHARS`].
    pub fn with_compaction(mut self, threshold_chars: usize) -> Self {
        self.compact_threshold_chars = threshold_chars;
        self
    }

    /// Append a knowledge block injected inline into the system prompt (A-19). Chainable.
    pub fn with_context(
        mut self,
        id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        self.context.push(ContextBlock::new(id, title, body));
        self
    }

    /// Ordered layers for this spec and the exact operation names visible to its executor.
    pub fn effective_prompt_layers_for_tools(&self, tools: &[String]) -> Vec<PromptLayer> {
        let mut layers = vec![PromptLayer::new(
            "flux.harness",
            PromptLayerKind::Harness,
            PromptTrust::Harness,
            PromptCacheClass::Static,
            HARNESS_SYSTEM_PROMPT.trim_end(),
        )];
        if self.profile == AgentProfile::Coding {
            layers.push(PromptLayer::new(
                "flux.profile.coding",
                PromptLayerKind::Profile,
                PromptTrust::Harness,
                PromptCacheClass::Static,
                CODING_PROFILE_PROMPT.trim_end(),
            ));
            for (name, id, body) in [
                ("read", "flux.tool.read", READ_TOOL_PROMPT),
                ("edit", "flux.tool.edit", EDIT_TOOL_PROMPT),
                ("bash", "flux.tool.bash", SHELL_TOOL_PROMPT),
                ("task", "flux.tool.task", TASK_TOOL_PROMPT),
            ] {
                if tools.iter().any(|tool| tool == name) {
                    layers.push(PromptLayer::new(
                        id,
                        PromptLayerKind::ToolGuidance,
                        PromptTrust::Harness,
                        PromptCacheClass::Static,
                        body.trim_end(),
                    ));
                }
            }
        }
        if !self.instructions.trim().is_empty() {
            layers.push(PromptLayer::new(
                "agent.instructions",
                PromptLayerKind::Instructions,
                PromptTrust::AgentAuthor,
                PromptCacheClass::Static,
                self.instructions.trim_end(),
            ));
        }
        layers.extend(self.prompt_layers.iter().cloned());
        if !self.context.is_empty() {
            let blocks = render_knowledge_blocks(&self.context, self.context_budget);
            if !blocks.is_empty() {
                layers.push(PromptLayer::new(
                    "agent.knowledge",
                    PromptLayerKind::Knowledge,
                    PromptTrust::Data,
                    PromptCacheClass::Session,
                    blocks,
                ));
            }
        }
        layers
    }

    /// Body-free manifest for context inspection and audit records.
    pub fn prompt_manifest_for_tools(&self, tools: &[String]) -> Vec<PromptManifestEntry> {
        let mut manifest = self
            .effective_prompt_layers_for_tools(tools)
            .iter()
            .map(PromptLayer::manifest)
            .collect::<Vec<_>>();
        manifest.extend(self.skills.iter().map(|skill| {
            let mut layer = PromptLayer::new(
                format!("agent.skill.{}", skill.name),
                PromptLayerKind::Skill,
                PromptTrust::Data,
                PromptCacheClass::Session,
                skill.body.text(),
            );
            if let Some(source) = &skill.source {
                layer = layer.with_source(source.display().to_string());
            }
            layer.manifest()
        }));
        manifest
    }

    /// The system prompt actually handed to an executor exposing `tools`.
    pub fn effective_system_prompt_for_tools(&self, tools: &[String]) -> String {
        self.effective_prompt_layers_for_tools(tools)
            .iter()
            .map(render_prompt_layer)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// The executor-independent prompt, excluding operation-specific guidance.
    pub fn effective_system_prompt(&self) -> String {
        self.effective_system_prompt_for_tools(&[])
    }

    /// Build the standard agent executor for this spec (select the `tools` subset, apply
    /// `permissions`, install the mandatory authorization profile, register the authored-loop ops)
    /// and assemble the engine. For full control over the executor, build it yourself and call
    /// [`AgentSpec::into_engine`].
    #[allow(deprecated)]
    #[deprecated(
        since = "0.24.0",
        note = "use AgentSpec::assemble_in with flux_runtime::ExecutionEnvironment; this shim is planned for removal in 0.26"
    )]
    pub fn assemble(
        self,
        provider: Arc<dyn Provider>,
        registry: ToolRegistry,
        executor: AgentExecutorConfig,
        events: Arc<EventStore>,
        flow: FlowStore,
    ) -> Result<FlowEngine> {
        let perms = PermissionManager::from_rules(&self.permissions.allow, &self.permissions.deny);
        let environment = ExecutionEnvironment::from_context(
            registry,
            perms,
            executor.approver,
            executor.authorization,
            executor.context,
        );
        self.assemble_in(provider, environment, events, flow)
    }

    /// Assemble this definition through the shared guarded execution-environment path.
    ///
    /// The surface owns workspace, catalog, approval, and authority decisions. This method applies
    /// the spec's tool subset and permission rules, restores the canonical authored-loop control
    /// plane, then builds the executor without consulting ambient process state.
    pub fn assemble_in(
        self,
        provider: Arc<dyn Provider>,
        environment: ExecutionEnvironment,
        events: Arc<EventStore>,
        flow: FlowStore,
    ) -> Result<FlowEngine> {
        let mut registry = environment.registry().subset(self.tools.as_deref());
        register_agent_ops(&mut registry)?;
        let permissions =
            PermissionManager::from_rules(&self.permissions.allow, &self.permissions.deny);
        let executor = environment
            .with_registry(registry)
            .with_permissions(permissions)
            .into_executor();
        self.into_engine(provider, executor, events, flow)
    }

    /// Assemble the engine from a fully-built [`Executor`]. The caller owns the registry (including
    /// [`register_agent_ops`]), permissions, approver, context, hooks, policy, and identity — used by
    /// the CLI (rich executor) and orchestrate (policy/identity-scoped sub-agents). Only the
    /// engine-identity fields of the spec (`model`, prompt layers, `skills`, settings, `groups`,
    /// `cwd`) are consumed here; `tools`/`permissions` are the caller's responsibility on this path.
    pub fn into_engine(
        self,
        provider: Arc<dyn Provider>,
        executor: Executor,
        events: Arc<EventStore>,
        flow: FlowStore,
    ) -> Result<FlowEngine> {
        let mut adaptive_policy = self.adaptive_policy.clone();
        resolve_adaptive_policy(provider.name(), &mut adaptive_policy)?;
        let tool_names = executor.registry().names();
        let system_prompt = self.effective_system_prompt_for_tools(&tool_names);
        let engine = FlowEngine::assemble_with_loop(
            provider,
            executor,
            events,
            flow,
            self.model,
            system_prompt,
            self.max_tokens,
            self.max_iterations,
            self.skills,
            self.compact_threshold_chars,
            self.groups,
            self.cwd,
            self.agent_loop,
        )?;
        engine.loop_host.set_adaptive_policy(adaptive_policy);
        Ok(engine
            .with_reasoning(self.thinking, self.effort)
            .with_ambient_signals(self.ambient_signals)
            .with_model_invoked_skills(self.model_invoked_skills))
    }
}

fn prompt_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

fn render_prompt_layer(layer: &PromptLayer) -> String {
    match layer.kind {
        PromptLayerKind::Harness | PromptLayerKind::Profile | PromptLayerKind::ToolGuidance => {
            layer.body.trim_end().to_string()
        }
        _ => {
            let mut tag = format!(
                "<context id=\"{}\" kind=\"{}\" trust=\"{}\"",
                prompt_attr(&layer.id),
                layer.kind.as_str(),
                layer.trust.as_str()
            );
            if let Some(source) = &layer.source {
                tag.push_str(&format!(" source=\"{}\"", prompt_attr(source)));
            }
            tag.push_str(">\n");
            tag.push_str(layer.body.trim_end());
            tag.push_str("\n</context>");
            tag
        }
    }
}

fn resolve_adaptive_policy(provider: &str, policy: &mut AdaptiveLoopPolicy) -> Result<()> {
    if policy.max_model_calls == 0 {
        return Err(Error::Config(
            "adaptive max_model_calls must be greater than zero".into(),
        ));
    }
    for (name, stage) in [
        ("intent", &mut policy.intent),
        ("explore", &mut policy.explore),
    ] {
        if stage.max_tokens == Some(0) {
            return Err(Error::Config(format!(
                "adaptive {name} max_tokens must be greater than zero"
            )));
        }
        if stage.max_calls == Some(0) {
            return Err(Error::Config(format!(
                "adaptive {name} max_calls must be greater than zero"
            )));
        }
        if let Some(model) = stage.model.as_deref() {
            if model.trim().is_empty() {
                return Err(Error::Config(format!(
                    "adaptive {name} model must not be empty"
                )));
            }
            stage.model = Some(flux_core::resolve_role_model(provider, model).map_err(
                |error| Error::Config(format!("adaptive {name} model is invalid: {error}")),
            )?);
        }
    }
    Ok(())
}

/// Register the typed adaptive stages the Flux-Lang agent loop (`agent-loop.flux`) calls, plus
/// model-facing `op.register` (`register_reflect`) and the evidence
/// `observe`/`evidence`/`metrics` (`register_evidence`). Call on the registry before building the [`Executor`] — and crucially
/// **after** any [`subset`](flux_runtime::ToolRegistry::subset), so a tool-restricted agent (a role
/// with `tools: [read, grep]`) still has the loop machinery (these ops are the engine's own control
/// flow, not model-facing tools, and match what [`FlowEngine::assemble`] pre-allows).
pub fn register_agent_ops(registry: &mut ToolRegistry) -> Result<()> {
    let mut assembled = registry.clone();
    flux_tools::install_reflect(&mut assembled)?;
    flux_tools::install_evidence(&mut assembled)?;
    *registry = assembled;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-188: `try_with_model_invoked_skills` discovers every skill under `cwd` EXCEPT one
    /// declaring `disable-model-invocation: true`, and leaves `skills` (the explicit always-on
    /// activation set) untouched — the two are additive, not the same knob.
    #[test]
    fn try_with_model_invoked_skills_excludes_disable_model_invocation() {
        let root = std::env::temp_dir().join(format!(
            "flux-agent-model-invoked-skills-{}",
            std::process::id()
        ));
        let dir = root.join(".flux/skills");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pdf.md"),
            "---\nname: pdf-extract\ndescription: extract PDFs\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            dir.join("private.md"),
            "---\nname: private-only\ndescription: manual only\ndisable-model-invocation: \
             true\n---\nbody",
        )
        .unwrap();

        let spec = AgentSpec {
            cwd: root.clone(),
            ..AgentSpec::new("mock")
        }
        .try_with_model_invoked_skills_in(&flux_runtime::metadata::DiscoveryEnv::empty())
        .unwrap();

        let names: Vec<&str> = spec
            .model_invoked_skills
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"pdf-extract"), "got {names:?}");
        assert!(!names.contains(&"private-only"), "got {names:?}");
        assert!(
            spec.skills.is_empty(),
            "the opt-in catalog must not populate the explicit `skills` activation set"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn canonical_control_plane_replaces_conflicts_and_survives_tool_subsets() {
        let mut registry = ToolRegistry::new();
        registry
            .try_register_from(
                "injected conflicting control plane",
                flux_runtime::tool_fn(
                    flux_spec::ToolSpec::read_only(
                        "observe",
                        "injected observe handler",
                        serde_json::json!({"type": "object"}),
                    ),
                    |_input| async { Ok(serde_json::Value::Null) },
                ),
            )
            .unwrap();
        registry
            .try_register_from(
                "visible role tool",
                flux_runtime::tool_fn(
                    flux_spec::ToolSpec::read_only(
                        "visible",
                        "visible role tool",
                        serde_json::json!({"type": "object"}),
                    ),
                    |_input| async { Ok(serde_json::Value::Null) },
                ),
            )
            .unwrap();

        register_agent_ops(&mut registry).unwrap();
        assert_ne!(
            registry.get("observe").unwrap().spec().description,
            "injected observe handler",
            "agent-owned control-plane names must use the canonical handler"
        );

        let mut restricted = registry.subset(Some(&["visible".to_string()]));
        register_agent_ops(&mut restricted).unwrap();
        assert_eq!(
            restricted.names(),
            vec![
                "ai_segment",
                "approve_batch",
                "detect_intent",
                "evidence",
                "execute_batch",
                "explore",
                "metrics",
                "observe",
                "op.register",
                "present_results",
                "visible",
            ]
        );
    }

    /// C-60: the convenience assembly door receives an explicit authorization profile, and an
    /// allow-everything approver cannot widen an empty (deny-all) policy floor.
    #[tokio::test]
    async fn assemble_auto_approval_cannot_widen_the_authorization_floor() {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool_hits = hits.clone();
        let mut registry = ToolRegistry::new();
        registry.register(flux_runtime::tool_fn(
            flux_spec::ToolSpec::read_only("guarded_probe", "probe", serde_json::json!({}))
                .with_access(vec![flux_spec::AccessKind::Filesystem]),
            move |_input| {
                let hits = tool_hits.clone();
                async move {
                    hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(serde_json::json!("ran"))
                }
            },
        ));
        let mut spec = AgentSpec::new("null");
        spec.permissions.allow.push("guarded_probe".into());
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let root = std::env::temp_dir().join(format!(
            "flux-agent-c60-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let system = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&root).unwrap(),
        ));
        let (caller, trust) = flux_policy::local_identity("agent-test");
        let environment = ExecutionEnvironment::new(
            system,
            registry,
            PermissionManager::new(),
            Arc::new(flux_runtime::AllowApprover),
            ExecutionAuthorization::new(flux_policy::AuthorizationPolicy::default(), caller, trust),
        );
        let engine = spec
            .assemble_in(
                Arc::new(flux_provider::NullProvider),
                environment,
                events,
                flow,
            )
            .unwrap();

        let result = engine
            .executor
            .dispatch("guarded_probe", serde_json::json!({}))
            .await;
        assert!(result.is_error && result.content.contains("denied by policy"));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        std::fs::remove_dir_all(root).ok();
    }

    /// The conditional `bash` guidance must contain both measured terminal-bench clauses:
    /// (1) verify runtime tools with `command -v` before writing files, and
    /// (2) start persistent servers in the background and confirm the port before finishing.
    #[test]
    fn default_system_prompt_bash_bullet_has_runtime_checks() {
        let prompt =
            AgentSpec::coding("mock").effective_system_prompt_for_tools(&["bash".to_string()]);
        // Clause 1: pre-flight check for required runtime tools.
        assert!(
            prompt.contains("command -v"),
            "bash bullet must instruct the agent to verify runtime tools with `command -v`"
        );
        assert!(
            prompt.contains("stop and report clearly rather than writing files that"),
            "bash bullet must tell the agent to stop and report when a required tool is missing"
        );

        // Clause 2: background server start + port-readiness confirmation.
        assert!(
            prompt.contains("nohup") && prompt.contains("&"),
            "bash bullet must show a background-server example (e.g. `nohup node server.js &`)"
        );
        assert!(
            prompt.contains("--retry-connrefused"),
            "bash bullet must mention --retry-connrefused as a port-readiness probe"
        );
        assert!(
            prompt.contains("ss -tlnp"),
            "bash bullet must mention `ss -tlnp` as an alternative port-readiness probe"
        );
        assert!(
            prompt.contains("never write files and exit silently when the server never started"),
            "bash bullet must forbid writing files and exiting silently when the server never started"
        );
    }

    /// N-004: the `# Tools` section must tell the agent the `read` line-number prefixes are a
    /// reference aid, not file content — so a sub-agent asked to return a line verbatim strips the
    /// leading number+tab instead of echoing it (the retest saw `1\talpha` where `alpha` was wanted).
    #[test]
    fn default_system_prompt_read_bullet_flags_line_number_view() {
        let prompt =
            AgentSpec::coding("mock").effective_system_prompt_for_tools(&["read".to_string()]);
        assert!(
            prompt.contains("line-numbered view"),
            "read bullet must describe the line-numbered view"
        );
        assert!(
            prompt.contains("not file content"),
            "read bullet must say the line-number prefixes are not part of the file content"
        );
    }

    #[test]
    fn general_agent_keeps_harness_core_without_coding_profile() {
        let spec = AgentSpec::general("mock", "Classify the supplied record.");
        let prompt = spec.effective_system_prompt();

        assert!(prompt.starts_with(HARNESS_SYSTEM_PROMPT));
        assert!(prompt.contains("Classify the supplied record."));
        assert!(!prompt.contains(CODING_PROFILE_PROMPT));
    }

    #[test]
    fn prompt_layers_keep_order_provenance_and_bodies_out_of_the_manifest() {
        let spec = AgentSpec::general("mock", "Agent author instructions.").with_prompt_layer(
            PromptLayer::new(
                "repository.policy.AGENTS.md",
                PromptLayerKind::RepositoryPolicy,
                PromptTrust::Repository,
                PromptCacheClass::Session,
                "REPOSITORY-BODY-MARKER",
            )
            .with_source("AGENTS.md")
            .captured_at(42),
        );
        let layers = spec.effective_prompt_layers_for_tools(&[]);
        assert_eq!(
            layers
                .iter()
                .map(|layer| layer.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "flux.harness",
                "agent.instructions",
                "repository.policy.AGENTS.md"
            ]
        );

        let manifest = serde_json::to_string(&spec.prompt_manifest_for_tools(&[])).unwrap();
        assert!(manifest.contains("AGENTS.md"));
        assert!(manifest.contains("\"captured_at_unix_secs\":42"));
        assert!(!manifest.contains("REPOSITORY-BODY-MARKER"));
    }

    #[test]
    fn coding_tool_guidance_describes_only_visible_operations() {
        let spec = AgentSpec::coding("mock");
        let read_only = spec.effective_system_prompt_for_tools(&["read".into()]);
        assert!(read_only.contains("# Read operation"));
        assert!(!read_only.contains("# Edit operation"));
        assert!(!read_only.contains("# Shell operation"));
        assert!(!read_only.contains("# Sub-agent operation"));
    }

    /// A-22: non-CLI (served / agentic / SDK) agents get a sane NON-ZERO compaction threshold by
    /// default — a long-lived persistent-session agent bounds its conversation instead of growing
    /// until the provider context window blows. A per-agent `with_compaction` override tunes it or
    /// disables it entirely.
    #[test]
    fn served_agents_get_a_nonzero_compaction_default() {
        let spec = AgentSpec::new("mock");
        assert!(
            spec.compact_threshold_chars > 0,
            "served/SDK agents must compact by default (was {})",
            spec.compact_threshold_chars
        );
        assert_eq!(
            spec.compact_threshold_chars,
            DEFAULT_COMPACT_THRESHOLD_CHARS
        );
        // Per-agent override: tune it…
        assert_eq!(
            AgentSpec::new("mock")
                .with_compaction(12_345)
                .compact_threshold_chars,
            12_345
        );
        // …or disable it entirely (never compact).
        assert_eq!(
            AgentSpec::new("mock")
                .with_compaction(0)
                .compact_threshold_chars,
            0
        );
    }

    #[test]
    fn spec_defaults_use_the_default_persona() {
        let spec = AgentSpec::new("mock");
        assert_eq!(spec.model, "mock");
        assert_eq!(spec.profile, AgentProfile::Coding);
        assert!(spec.instructions.is_empty());
        assert_eq!(spec.max_iterations, 50);
        assert!(spec.tools.is_none());
        assert!(!spec.thinking);
        assert_eq!(spec.effort, None);
        let prompt = spec.effective_system_prompt();
        assert!(prompt.starts_with(HARNESS_SYSTEM_PROMPT.trim_end()));
        assert!(prompt.contains(CODING_PROFILE_PROMPT.trim_end()));
        assert!(spec.context.is_empty());
    }

    /// A-19: injected context blocks render into the effective system prompt, after the persona.
    #[test]
    fn context_blocks_render_into_effective_prompt() {
        let spec = AgentSpec::new("mock")
            .with_context("hours", "Opening hours", "Mon–Fri 09:00–18:00 CET.")
            .with_context("refund", "Refunds", "Refunds take 5–7 business days.");
        let p = spec.effective_system_prompt();
        assert!(
            p.starts_with(HARNESS_SYSTEM_PROMPT.trim_end()),
            "harness comes first"
        );
        assert!(
            p.contains("<knowledge-base id=\"hours\" title=\"Opening hours\">"),
            "block rendered: {p}"
        );
        assert!(p.contains("Mon–Fri 09:00–18:00 CET."));
        // order preserved
        assert!(p.find("hours").unwrap() < p.find("refund").unwrap());
    }

    /// A-73: adaptive is the explicit default and callers may supply an authored Flux loop.
    #[test]
    fn agent_loop_defaults_to_adaptive_and_accepts_authored_flux() {
        assert_eq!(AgentSpec::default().agent_loop, AgentLoopSpec::default());
        assert_eq!(AgentSpec::new("mock").agent_loop, AgentLoopSpec::default());
        let authored = AgentLoopSpec::parse("flow custom -> string\n  return \"ok\"").unwrap();
        let spec = AgentSpec {
            agent_loop: authored.clone(),
            ..AgentSpec::new("mock")
        };
        assert_eq!(spec.agent_loop, authored);
    }

    #[test]
    fn adaptive_stage_models_stay_on_the_parent_provider() {
        let mut matching = AdaptiveLoopPolicy {
            intent: AgentStagePolicy {
                model: Some("codex/fast-router".into()),
                ..AgentStagePolicy::default()
            },
            ..AdaptiveLoopPolicy::default()
        };
        resolve_adaptive_policy("codex", &mut matching).unwrap();
        assert_eq!(matching.intent.model.as_deref(), Some("fast-router"));

        let mut crossing = AdaptiveLoopPolicy {
            explore: AgentStagePolicy {
                model: Some("openai/gpt-5.5".into()),
                ..AgentStagePolicy::default()
            },
            ..AdaptiveLoopPolicy::default()
        };
        let error = resolve_adaptive_policy("codex", &mut crossing)
            .unwrap_err()
            .to_string();
        assert!(error.contains("provider 'openai'"), "{error}");
        assert!(error.contains("parent's provider ('codex')"), "{error}");
    }

    /// Guarded default discovery injects a project skill's bytes into the pure L0 parser.
    #[test]
    fn with_default_skills_populates_from_cwd_dirs() {
        let dir = std::env::temp_dir().join(format!("flux-agent-skills-{}", std::process::id()));
        let skills = dir.join(".flux").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("agent-spec-l02.md"),
            "---\nname: agent-spec-l02\ndescription: d\ntriggers: [zz]\n---\nBODY",
        )
        .unwrap();

        let spec = AgentSpec {
            cwd: dir.clone(),
            ..AgentSpec::new("mock")
        }
        .try_with_default_skills_in(&flux_runtime::metadata::DiscoveryEnv::empty())
        .unwrap();
        let s = spec
            .skills
            .iter()
            .find(|s| s.name == "agent-spec-l02")
            .expect("project skill discovered");
        assert_eq!(s.body.text(), "BODY");
        std::fs::remove_dir_all(&dir).ok();
    }
}
