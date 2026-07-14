//! `flux-orchestrate` — multi-agent orchestration: markdown agent roles, a sub-agent spawner,
//! and a `task` tool that delegates a subtask to a role and returns its result.
//!
//! A role is `.flux/agents/<name>.md` with frontmatter (`description`/`model`/`tools`) and a body
//! used as the sub-agent's system prompt. [`LocalSpawner`] runs a role as an isolated sub-agent
//! (fresh in-memory session, scoped toolset, auto-approved within its sandboxed tools) and returns
//! its final text. When [`SpawnRequest::activity`] is present, it additionally reports correlated
//! planning/tool/observation activity while keeping child thinking, prose, and result content private.
//! Plan-and-dispatch builds on this (follow-up).

// Agent roles + definitions live in the Agent-pillar crate (`flux-agent`); re-exported here so
// `flux_orchestrate::{Role, RoleRegistry, parse_role}` keep resolving for consumers.
pub use flux_agent::{parse_role, Role, RoleRegistry};

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_agent::register_agent_ops;
use flux_core::{Error, Result, Usage};
use flux_events::EventStore;
use flux_flow::AgentSink;
use flux_policy::{AuthorizationPolicy, Caller, Trust};
use flux_provider::{Effort, Provider};
use flux_runtime::{
    ApprovalChoice, Approver, Executor, IdentityCell, PermissionManager, SpawnActivity,
    SpawnActivityEvent, SpawnActivitySink, SpawnOutcome, SpawnRequest, Spawner, Tool, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_spec::{tool_input_schema, Effect, Idempotency, IntentSet, Risk, ToolSpec};
use flux_system::System;
use tokio_util::sync::CancellationToken;

/// Deserialize an op's JSON arguments into its typed input struct — the single source of truth
/// paired with the `schemars`-derived `input_schema`. Maps a serde error to the op-error style.
fn parse_params<T: serde::de::DeserializeOwned>(params: serde_json::Value, op: &str) -> Result<T> {
    serde_json::from_value(params)
        .map_err(|e| Error::Other(format!("{op}: invalid arguments: {e}")))
}

/// The headless approver for sub-agents: they run non-interactively, so they auto-approve their
/// scoped, policy-permitted tool calls — but a **destructive** operation is refused outright (a
/// sub-agent must never `rm -rf` etc. without a human). Combined with the inherited authorization
/// policy, this bounds sub-agents instead of the old blanket allow-everything approver.
struct SubAgentApprover;

#[async_trait]
impl Approver for SubAgentApprover {
    async fn request(
        &self,
        _tool: &str,
        _subjects: &[String],
        intents: &IntentSet,
    ) -> ApprovalChoice {
        if intents.is_destructive() {
            ApprovalChoice::Deny
        } else {
            ApprovalChoice::Allow
        }
    }

    /// The aggregate face of the same policy — an authored flow or host-built action batch may be
    /// approved as one unit, so the destructive deny must fire HERE, not only per-op. Denies on the
    /// batch's aggregate intents AND on the
    /// declared-destructive flag, which also covers spec-level `Risk::Destructive` ops (e.g.
    /// composites) whose concrete intents aren't statically visible. A destructive command assembled
    /// at runtime from `$symbols` is caught by the dispatcher's undisclosed-destructive re-fire,
    /// which routes back to `request` above — denied there too.
    async fn request_plan(&self, plan: &flux_runtime::PlanApprovalRequest) -> ApprovalChoice {
        if plan.destructive || plan.intents.is_destructive() {
            ApprovalChoice::Deny
        } else {
            ApprovalChoice::Allow
        }
    }
}

/// Produces a fresh provider per sub-agent (sub-agents can't share one `Box<dyn Provider>`).
pub type ProviderFactory = Arc<dyn Fn() -> Result<Box<dyn Provider>> + Send + Sync>;

/// Process-local correlation for live child activity. Ephemeral child stores each begin at `s_1`,
/// so a session id alone cannot distinguish concurrent storeless spawns of the same role.
static NEXT_SPAWN_ACTIVITY_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Per-sub-agent resource limits. Defaults preserve the historical behaviour: 30 iterations (a
/// planner that grounds a task in files or a worker that reads/edits/then runs the dev-gate needs
/// more than a handful of tool turns), the spawner's configured token budget, and no wall-clock
/// deadline.
#[derive(Clone)]
pub struct SpawnLimits {
    /// Per-turn tool-iteration cap.
    pub max_iterations: usize,
    /// Per-turn model token budget.
    pub max_tokens: u32,
    /// Optional wall-clock deadline. On expiry the child's cancel token is **fired** and `spawn` then
    /// awaits the child so it reaches its own cancel path and persists a finalizing assistant message
    /// — rather than abandoning it, which would leave an **unterminated turn** (a user message with no
    /// finalizing assistant message) in a shared audit store. (A bounded grace backstops a child that
    /// somehow doesn't observe the cancel; see `spawn`.)
    pub wall_clock: Option<std::time::Duration>,
}

impl SpawnLimits {
    /// Default limits for a given per-turn token budget (30 iterations, no wall-clock deadline).
    pub fn new(max_tokens: u32) -> Self {
        Self {
            max_iterations: 30,
            max_tokens,
            wall_clock: None,
        }
    }
}

/// Spawns sub-agents from roles, locally and in-process.
pub struct LocalSpawner {
    provider_factory: ProviderFactory,
    roles: Arc<RoleRegistry>,
    base_registry: ToolRegistry,
    system: Arc<System>,
    default_model: String,
    default_thinking: bool,
    default_effort: Option<Effort>,
    limits: SpawnLimits,
    /// Approver the sub-agent's tool calls dispatch through. `None` → the default [`SubAgentApprover`]
    /// (auto-approve non-destructive, deny destructive). A multi-tenant consumer injects an approver
    /// that approval-gates its mutations.
    approver: Option<Arc<dyn Approver>>,
    /// Authorization the sub-agents inherit (policy floor + a shared identity cell). The cell is
    /// read at *spawn time*, so a per-request surface that swaps the parent identity between turns
    /// (server principal mode) has children run under the current request's principal — never a
    /// stale build-time service identity. When unset, sub-agents still run under the headless
    /// approver but without the policy gate.
    auth: Option<(AuthorizationPolicy, IdentityCell)>,
    /// When set, child runs persist into this shared (tenant) event store instead of a throwaway
    /// in-memory one, so a sub-agent's inner tool calls land in the audit log the parent reads.
    audit: Option<Arc<EventStore>>,
    /// Current delegation depth (0 = a top-level agent's direct child). A child is a leaf when
    /// `depth + 1 >= max_depth`. The default `max_depth = 1` keeps every sub-agent a leaf.
    depth: usize,
    max_depth: usize,
}

impl LocalSpawner {
    pub fn new(
        provider_factory: ProviderFactory,
        roles: Arc<RoleRegistry>,
        base_registry: ToolRegistry,
        system: Arc<System>,
        default_model: impl Into<String>,
        max_tokens: u32,
    ) -> Self {
        Self {
            provider_factory,
            roles,
            base_registry,
            system,
            default_model: default_model.into(),
            default_thinking: false,
            default_effort: None,
            limits: SpawnLimits::new(max_tokens),
            approver: None,
            auth: None,
            audit: None,
            depth: 0,
            max_depth: 1,
        }
    }

    /// Bound spawned sub-agents by an authorization policy + resolved identity (inherited from the
    /// parent). Sub-agents then traverse the same policy floor as the top-level agent. Wraps the
    /// identity in a fresh, unshared cell — a per-request surface shares its live cell via
    /// [`with_authorization_cell`](Self::with_authorization_cell) instead.
    pub fn with_authorization(
        self,
        policy: AuthorizationPolicy,
        caller: Caller,
        trust: Trust,
    ) -> Self {
        self.with_authorization_cell(policy, IdentityCell::new(caller, trust))
    }

    /// Like [`with_authorization`](Self::with_authorization), but sharing an externally-owned
    /// identity cell (typically the parent executor's — see `Executor::identity`), so identity
    /// swaps on the parent propagate to every subsequently spawned child.
    pub fn with_authorization_cell(
        mut self,
        policy: AuthorizationPolicy,
        cell: IdentityCell,
    ) -> Self {
        self.auth = Some((policy, cell));
        self
    }

    /// Override the per-sub-agent resource limits (iteration cap, token budget, wall-clock deadline).
    pub fn with_limits(mut self, limits: SpawnLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Set the reasoning policy inherited by roles that do not override it in frontmatter.
    pub fn with_reasoning(mut self, thinking: bool, effort: Option<Effort>) -> Self {
        self.default_thinking = thinking;
        self.default_effort = effort;
        self
    }

    /// Inject the approver a sub-agent's tool calls dispatch through (default: [`SubAgentApprover`]).
    pub fn with_approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = Some(approver);
        self
    }

    /// Persist child runs into a shared (tenant) event store so their inner tool calls are auditable.
    pub fn with_audit(mut self, events: Arc<EventStore>) -> Self {
        self.audit = Some(events);
        self
    }

    /// Allow bounded nested delegation: a sub-agent at `depth < max_depth` whose own `effective_tools`
    /// include `task` keeps the `task` tool and a depth-incremented spawner built over its own narrowed
    /// registry (never the unrestricted base). Default `1` (children are leaves). `> 1` is an opt-in
    /// escape hatch — the recursion bound is `max_depth`, never unbounded.
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth.max(1);
        self
    }

    /// A clone of this spawner at a deeper delegation level (shares all Arc-held state), rebased on
    /// `base_registry` — the caller's own narrowed registry (base ∩ its `effective_tools`), never the
    /// unrestricted original. This is what makes a `with_tools` ceiling transitive across nested
    /// delegation: a grandchild role's `tools` allowlist is intersected against a pool that has already
    /// had everything the ancestor's ceiling excluded removed, so no descendant's own role declaration
    /// can resurrect a tool an ancestor narrowed away, no matter how many hops down.
    fn at_depth(&self, depth: usize, base_registry: ToolRegistry) -> LocalSpawner {
        LocalSpawner {
            provider_factory: self.provider_factory.clone(),
            roles: self.roles.clone(),
            base_registry,
            system: self.system.clone(),
            default_model: self.default_model.clone(),
            default_thinking: self.default_thinking,
            default_effort: self.default_effort,
            limits: self.limits.clone(),
            approver: self.approver.clone(),
            auth: self.auth.clone(),
            audit: self.audit.clone(),
            depth,
            max_depth: self.max_depth,
        }
    }
}

#[async_trait]
impl Spawner for LocalSpawner {
    /// Run one sub-agent. `request.cap_scope` (the caller's active `with_tools` allowlist, if any)
    /// is intersected into the role's own `tools` so a `task` invoked from inside a capability
    /// scope can never hand the child a broader tool set than the block that spawned it
    /// (capabilities only ever narrow on descent: role ∩ block scope). `request.parent_session`
    /// is recorded as the child session's `correlation_id` (A-08). When `request.activity` is set,
    /// the private child collector also emits correlated live lifecycle events; it still returns the
    /// child's final prose only through [`SpawnOutcome`].
    async fn spawn(
        &self,
        request: SpawnRequest,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<SpawnOutcome> {
        let role_name = request.role.as_str();
        let task = request.task.as_str();
        let cap_scope = request.cap_scope.as_deref();
        let role = self
            .roles
            .get(role_name)
            .ok_or_else(|| Error::Other(format!("unknown role: {role_name}")))?;

        let provider = (self.provider_factory)()?;
        // Captured before the provider moves into the child engine: the canonical-spec stamp on
        // the child's usage attribution needs the provider name (C-15).
        let provider_name = provider.name().to_string();

        // Scoped toolset; sub-agents run autonomously under the policy-bounded headless approver
        // (auto-approve scoped, policy-permitted calls; refuse destructive ones — unless an approver
        // is injected). `register_agent_ops` adds the typed stages the Flux-Lang agent loop calls —
        // sub-agents run the same audited loop as the top-level agent.
        //
        // `effective_tools` is the role's own allowlist further intersected with the caller's active
        // capability scope (if any) — narrow-only, same rule as `Executor::push_cap_scope`. A role with
        // no `tools` restriction (`None` = "everything the base registry offers") still gets narrowed
        // down to exactly `cap_scope` when one is active, rather than staying unrestricted.
        let effective_tools: Option<Vec<String>> = match (role.tools.as_deref(), cap_scope) {
            (None, None) => None,
            (Some(role_tools), None) => Some(role_tools.to_vec()),
            (None, Some(scope)) => Some(scope.to_vec()),
            (Some(role_tools), Some(scope)) => Some(
                role_tools
                    .iter()
                    .filter(|t| scope.contains(t))
                    .cloned()
                    .collect(),
            ),
        };
        let mut registry = self.base_registry.subset(effective_tools.as_deref());
        register_agent_ops(&mut registry);

        // Recursion bound: a child at the leaf depth must never spawn further sub-agents, so `task` is
        // stripped from its registry AND no spawner is installed in its context (the two guards that
        // make a sub-agent a leaf). Below the bound, the child keeps `task` and a depth-incremented
        // spawner — but ONLY when `task` itself survived the role ∩ cap_scope narrowing above
        // (`effective_tools`); a `with_tools` block that excluded `task` must make this child a leaf
        // too, exactly as it would for any other excluded tool. With the default `max_depth = 1`, every
        // child is a leaf — today's behaviour exactly.
        let child_depth = self.depth + 1;
        let child_has_task = effective_tools
            .as_deref()
            .is_none_or(|tools| tools.iter().any(|t| t == "task"));
        let child_can_delegate = child_depth < self.max_depth && child_has_task;
        let mut ctx = ToolContext::new(self.system.clone());
        if child_can_delegate {
            // Bounded nested delegation: the child keeps both halves of the delegation capability —
            // the `task` tool in its registry AND a depth-incremented spawner in its context. The
            // depth-next spawner is rebased on THIS child's own narrowed registry (base ∩
            // `effective_tools`), never the unrestricted `base_registry` — so a `with_tools` ceiling is
            // transitive across nested delegation: a grandchild role's `tools` allowlist can only ever
            // draw from a pool this ancestor has already narrowed, no matter how many hops down
            // (capabilities only ever narrow on descent, see `push_cap_scope`'s doc).
            registry.register(Arc::new(TaskTool));
            let child_base = self.base_registry.subset(effective_tools.as_deref());
            ctx = ctx.with_spawner(Arc::new(self.at_depth(child_depth, child_base)));
        } else {
            // Leaf (depth bound hit, or this hop's own scope excludes `task`): never spawn further
            // sub-agents. Both guards apply — `task` is stripped from the registry and no spawner is
            // installed in the context.
            registry.remove("task");
        }
        let approver: Arc<dyn Approver> = self
            .approver
            .clone()
            .unwrap_or_else(|| Arc::new(SubAgentApprover));
        let activity_redactor = ctx.redactor.clone();
        let mut executor = Executor::new(registry, PermissionManager::new(), approver, ctx);
        if let Some((policy, cell)) = &self.auth {
            // Snapshot the *current* identity at spawn time: under a per-request surface the cell
            // holds the request principal, not the build-time service identity. The child gets a
            // snapshot rather than the live cell — a child completes within one serialized turn,
            // and sharing would let a later request's identity bleed into a still-draining child.
            let (caller, trust) = cell.get();
            executor = executor
                .with_policy(policy.clone())
                .with_identity(caller, trust);
        }

        // The role *is* the agent definition: body → system prompt, `tools` already applied to the
        // scoped registry above, model inherits the spawner default when the role doesn't override it.
        let mut spec = role.to_spec(&self.default_model)?;
        spec.thinking = role.thinking.unwrap_or(self.default_thinking);
        spec.effort = role.effort.or(self.default_effort);
        // A-41: a role's `model:` override speaks the same provider-prefixed spec form `-m` accepts
        // (e.g. `openrouter/deepseek/deepseek-v4-flash`), but sub-agents always run on the PARENT's
        // provider — there is no per-sub-agent provider factory. Resolve it here, fast, at
        // spawn-time: the parent's own provider prefix is stripped (the natural form users write in
        // role frontmatter); any OTHER known provider prefix fails fast with a diagnostic naming
        // both providers, instead of reaching the wire and 400ing mid-turn. `default_model` (no
        // override) is untouched — it is already correct for this provider by construction.
        if let Some(role_model) = &role.model {
            spec.model = flux_core::resolve_role_model(&provider_name, role_model)?;
        }
        spec.max_tokens = self.limits.max_tokens;
        spec.max_iterations = self.limits.max_iterations;

        // Child runs persist into the shared (tenant) store when auditing; otherwise a throwaway
        // in-memory store keeps the sub-agent ephemeral (the documented mode for storeless hosts).
        let events = match &self.audit {
            Some(store) => store.clone(),
            None => Arc::new(EventStore::in_memory()?),
        };
        // The child stream carries its identity + provenance (A-08, on the D-02 context envelope):
        // `agent_id` names the role, `correlation_id` points back at the parent session — so a
        // shared audit store answers "what did the sub-agents of turn X do" with one indexed read.
        let child_ctx = flux_events::EventContext {
            agent_id: Some(format!("subagent:{role_name}")),
            correlation_id: request.parent_session.clone(),
            ..Default::default()
        };
        let session_id = events.create_session_with_context(&spec.model, &child_ctx)?;
        // Share the event store with the flow store so the child's run trace (its inner tool calls)
        // lands in the same log as its conversation — into the shared audit store when one is set.
        let flow = flux_flow::state::FlowStore::in_memory_with_events(events.clone())?;
        let engine = spec.into_engine(Arc::from(provider), executor, events, flow)?;
        // Captured now (not read back off `engine` after the run) purely for clarity at the call
        // sites below — `engine.model` never changes over a sub-agent's single turn. Canonical
        // provider/model spec (C-15): this string keys the parent's `CallUsage` attribution.
        let model = flux_core::canonical_model_spec(Some(&provider_name), &engine.model);

        // The child runs under a child of the parent's cancel token: cancelling the parent turn
        // cancels the child. A wall-clock deadline fires that same token so the child reaches its own
        // cancel path (a finalizing assistant message, valid in a shared audit store) and `spawn`
        // returns a typed error. NOTE: this clean finalization holds for the *deadline* path; if the
        // *parent turn* is cancelled, the parent engine drops the whole turn future (this `task` call
        // included) without awaiting the child — so under `with_audit` a parent Ctrl-C can leave the
        // child's turn unterminated in the shared store. See docs/designs/sub-agent-hardening.md.
        let run_cancel = cancel.child_token();
        let spawn_id = NEXT_SPAWN_ACTIVITY_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut sink = TextCollector::new(
            request.activity.clone(),
            spawn_id,
            role_name.to_string(),
            session_id.clone(),
            request.parent_session.clone(),
            child_depth,
            activity_redactor,
        );
        match self.limits.wall_clock {
            Some(dur) => {
                let run = engine.run_turn_cancellable(&session_id, task, &mut sink, &run_cancel);
                tokio::pin!(run);
                tokio::select! {
                    res = &mut run => { res?; }
                    _ = tokio::time::sleep(dur) => {
                        run_cancel.cancel();
                        // Let the child observe the cancel and finalize a valid session shape. A bounded
                        // grace backstops the (currently unreachable) case where an await inside the
                        // child doesn't observe the token — e.g. a compaction-time provider call, which
                        // sub-agents never hit since they run a single fresh-session turn. Without the
                        // backstop such a child would hang `spawn` forever, defeating the deadline.
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), run).await;
                        return Err(Error::Other(format!(
                            "sub-agent '{role_name}' exceeded its {dur:?} wall-clock limit"
                        )));
                    }
                }
            }
            None => {
                engine
                    .run_turn_cancellable(&session_id, task, &mut sink, &run_cancel)
                    .await?;
            }
        }
        // The child's accumulated per-turn usage (C-06 sub-agent rollup): `TaskTool` folds this into
        // the PARENT turn's tally and records it as a `CallUsage` attributed to the child's own model,
        // so the sub-agent's spend counts toward the parent's total without being double-attributed to
        // the parent's model. `None` when the child billed nothing (e.g. `mock`, or no usage reported).
        let usage = engine.loop_host.turn_usage();
        let usage = (usage.total() > 0).then_some(usage);
        let cancelled = sink.cancelled;
        sink.finish(cancelled, usage.clone());
        if cancelled {
            return Err(Error::Other(format!(
                "sub-agent '{role_name}' was cancelled"
            )));
        }
        let text = std::mem::take(&mut sink.text);
        let tool_calls = sink.tool_calls;
        Ok(SpawnOutcome {
            text,
            model,
            usage,
            session_id,
            tool_calls,
        })
    }
}

/// A reusable bundle for wiring sub-agents into any surface (the CLI, the SDK): the role catalog, the
/// tool surface children may be granted, how to build a fresh provider per child, and the safety
/// knobs. [`SubAgents::into_spawner`] is the single construction path — the surface then registers
/// [`TaskTool`] into its own catalog and installs the returned spawner via `ToolContext::with_spawner`.
pub struct SubAgents {
    /// The named roles a `task` call may target (in-memory or disk-loaded).
    pub roles: RoleRegistry,
    /// The tool surface children may be granted — each role's `tools` allowlist subsets this. Kept
    /// explicit (not the parent's assembled registry) so child wiring is decoupled from parent
    /// registration order and the child's tool surface is auditable.
    pub child_base: ToolRegistry,
    pub provider_factory: ProviderFactory,
    pub default_model: String,
    pub default_thinking: bool,
    pub default_effort: Option<Effort>,
    pub limits: SpawnLimits,
    pub approver: Option<Arc<dyn Approver>>,
    pub auth: Option<(AuthorizationPolicy, IdentityCell)>,
    pub audit: Option<Arc<EventStore>>,
    /// Max delegation depth (default `1` = children are leaves). `> 1` is a bounded opt-in for nested
    /// delegation; see [`LocalSpawner::with_max_depth`].
    pub max_depth: usize,
}

impl SubAgents {
    /// A bundle with default limits for `max_tokens`; everything else off (no approver override, no
    /// inherited authorization, no audit store, children are leaves). Set those with the `with_*` methods.
    pub fn new(
        roles: RoleRegistry,
        child_base: ToolRegistry,
        provider_factory: ProviderFactory,
        default_model: impl Into<String>,
        max_tokens: u32,
    ) -> Self {
        Self {
            roles,
            child_base,
            provider_factory,
            default_model: default_model.into(),
            default_thinking: false,
            default_effort: None,
            limits: SpawnLimits::new(max_tokens),
            approver: None,
            auth: None,
            audit: None,
            max_depth: 1,
        }
    }

    /// Inherit an authorization policy + resolved identity (the parent's floor) for every sub-agent.
    /// Wraps the identity in a fresh cell; a per-request surface shares its live cell via
    /// [`with_authorization_cell`](Self::with_authorization_cell).
    pub fn with_authorization(
        self,
        policy: AuthorizationPolicy,
        caller: Caller,
        trust: Trust,
    ) -> Self {
        self.with_authorization_cell(policy, IdentityCell::new(caller, trust))
    }

    /// Like [`with_authorization`](Self::with_authorization), but sharing an externally-owned
    /// identity cell (typically the parent executor's), so per-turn identity swaps propagate to
    /// every subsequently spawned child.
    pub fn with_authorization_cell(
        mut self,
        policy: AuthorizationPolicy,
        cell: IdentityCell,
    ) -> Self {
        self.auth = Some((policy, cell));
        self
    }

    /// Override the per-sub-agent resource limits.
    pub fn with_limits(mut self, limits: SpawnLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Set the reasoning policy inherited by roles that do not override it in frontmatter.
    pub fn with_reasoning(mut self, thinking: bool, effort: Option<Effort>) -> Self {
        self.default_thinking = thinking;
        self.default_effort = effort;
        self
    }

    /// Inject the approver a sub-agent's tool calls dispatch through.
    pub fn with_approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = Some(approver);
        self
    }

    /// Persist child runs into a shared (tenant) event store for auditability.
    pub fn with_audit(mut self, events: Arc<EventStore>) -> Self {
        self.audit = Some(events);
        self
    }

    /// Allow bounded nested delegation (default `1` = children are leaves). See
    /// [`LocalSpawner::with_max_depth`].
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth.max(1);
        self
    }

    /// Build the spawner over `system` (the guarded IO surface children share). The caller registers
    /// [`TaskTool`] into its catalog and installs the returned spawner via `ToolContext::with_spawner`.
    pub fn into_spawner(self, system: Arc<System>) -> Arc<dyn Spawner> {
        let limits = self.limits;
        let mut spawner = LocalSpawner::new(
            self.provider_factory,
            Arc::new(self.roles),
            self.child_base,
            system,
            self.default_model,
            limits.max_tokens,
        )
        .with_reasoning(self.default_thinking, self.default_effort)
        .with_limits(limits)
        .with_max_depth(self.max_depth);
        if let Some(approver) = self.approver {
            spawner = spawner.with_approver(approver);
        }
        if let Some((policy, cell)) = self.auth {
            spawner = spawner.with_authorization_cell(policy, cell);
        }
        if let Some(store) = self.audit {
            spawner = spawner.with_audit(store);
        }
        Arc::new(spawner)
    }
}

struct TextCollector {
    text: String,
    /// How many tool calls the child streamed — the cheap trace count `subagent.trace` reports.
    tool_calls: usize,
    activity: Option<Arc<dyn SpawnActivitySink>>,
    spawn_id: u64,
    role: String,
    child_session_id: String,
    parent_session: Option<String>,
    depth: usize,
    redactor: flux_secret::Redactor,
    next_call_id: u64,
    pending: std::collections::HashMap<String, Vec<u64>>,
    terminal_usage: Option<Usage>,
    cancelled: bool,
    terminal_emitted: bool,
}

impl TextCollector {
    fn new(
        activity: Option<Arc<dyn SpawnActivitySink>>,
        spawn_id: u64,
        role: String,
        child_session_id: String,
        parent_session: Option<String>,
        depth: usize,
        redactor: flux_secret::Redactor,
    ) -> Self {
        Self {
            text: String::new(),
            tool_calls: 0,
            activity,
            spawn_id,
            role,
            child_session_id,
            parent_session,
            depth,
            redactor,
            next_call_id: 0,
            pending: std::collections::HashMap::new(),
            terminal_usage: None,
            cancelled: false,
            terminal_emitted: false,
        }
    }

    fn emit(&self, event: SpawnActivityEvent) {
        if let Some(activity) = &self.activity {
            activity.emit(SpawnActivity {
                spawn_id: self.spawn_id,
                role: self.role.clone(),
                child_session_id: self.child_session_id.clone(),
                parent_session: self.parent_session.clone(),
                depth: self.depth,
                event,
            });
        }
    }

    fn active_call(&self, name: &str) -> Option<u64> {
        self.pending
            .get(name)
            .and_then(|calls| calls.last())
            .copied()
    }

    /// Emit exactly one terminal event at the spawner boundary. `AgentSink::turn_end` alone is
    /// insufficient: a timed-out child can finalize its engine turn and still make `spawn` fail.
    fn finish(&mut self, is_error: bool, usage: Option<Usage>) {
        if self.terminal_emitted {
            return;
        }
        self.terminal_emitted = true;
        self.emit(SpawnActivityEvent::Finished { usage, is_error });
    }
}

impl Drop for TextCollector {
    fn drop(&mut self) {
        if !self.terminal_emitted {
            // Covers engine errors, wall-clock returns, and cancellation dropping the in-flight
            // spawner future. Drop is synchronous, so the parent sees a terminal failure even when
            // no async cleanup path can be awaited.
            self.finish(true, self.terminal_usage.clone());
        }
    }
}

impl AgentSink for TextCollector {
    fn text_delta(&mut self, t: &str) {
        self.text.push_str(t);
    }

    // Intentionally do not forward `thinking_delta`: child reasoning is private even when the
    // parent surface elects to show delegated activity.

    fn planning(&mut self, active: bool) {
        self.emit(SpawnActivityEvent::Planning { active });
    }

    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        self.tool_calls += 1;
        if self.activity.is_none() {
            return;
        }
        self.next_call_id += 1;
        let call_id = self.next_call_id;
        self.pending
            .entry(name.to_string())
            .or_default()
            .push(call_id);
        let mut input = input.clone();
        redact_spawn_json(&self.redactor, &mut input);
        self.emit(SpawnActivityEvent::ToolCall {
            call_id,
            name: name.to_string(),
            input,
        });
    }

    fn tool_timing(&mut self, name: &str, timing: &flux_core::OperationTiming) {
        if let Some(call_id) = self.active_call(name) {
            self.emit(SpawnActivityEvent::ToolTiming {
                call_id,
                name: name.to_string(),
                timing: *timing,
            });
        }
    }

    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        let Some(call_id) = self.pending.get_mut(name).and_then(Vec::pop) else {
            return;
        };
        // Result content and error text stay inside the child/model transcript. Only the outcome
        // bit crosses the live reporter contract.
        self.emit(SpawnActivityEvent::ToolResult {
            call_id,
            name: name.to_string(),
            is_error: result.is_error,
        });
    }

    fn observation(&mut self, observation: &flux_evidence::Observation) {
        if let Some(activity) = SpawnActivity::from_observation(observation) {
            // A nested child already carries its originating role/session/depth/call id. Relay it
            // unchanged so an intermediate collector cannot collapse or misattribute the scope.
            if let Some(parent) = &self.activity {
                parent.emit(activity);
            }
            return;
        }
        if observation.kind == "turn.cancelled" {
            self.cancelled = true;
        }
        let mut observation = observation.clone();
        redact_spawn_json(&self.redactor, &mut observation.data);
        self.emit(SpawnActivityEvent::Observation { observation });
    }

    fn turn_end(&mut self, usage: Option<Usage>) {
        // Cache engine usage, but defer the terminal event until `LocalSpawner::spawn` knows
        // whether the overall operation succeeded (a timeout may occur after engine finalization).
        self.terminal_usage = usage;
    }
}

fn redact_spawn_json(redactor: &flux_secret::Redactor, value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = redactor.redact(text),
        serde_json::Value::Array(items) => {
            for item in items {
                redact_spawn_json(redactor, item);
            }
        }
        serde_json::Value::Object(fields) => {
            // JSON keys can contain credentials too (for example a model-generated header map).
            // Rebuild the map so both keys and values cross the live reporter scrubbed.
            let original = std::mem::take(fields);
            for (key, mut value) in original {
                redact_spawn_json(redactor, &mut value);
                fields.insert(redactor.redact(&key), value);
            }
        }
        _ => {}
    }
}

/// A simplified plan-and-dispatch: spawn the `planner` role to produce a plan for `goal`, then
/// the `worker` role to execute it, returning both. (Sequential; the dependency-wave variant below
/// runs workers in parallel.)
pub async fn plan_and_dispatch(
    spawner: &dyn Spawner,
    goal: &str,
    cancel: &CancellationToken,
) -> Result<String> {
    let plan = spawner
        .spawn(
            SpawnRequest::new(
                "planner",
                format!("Goal: {goal}\n\nProduce a concise, ordered plan."),
            ),
            cancel,
        )
        .await?
        .text;
    if cancel.is_cancelled() {
        return Ok(format!("── plan ──\n{plan}\n\n(interrupted)"));
    }
    let result = spawner
        .spawn(
            SpawnRequest::new(
                "worker",
                format!(
                    "Goal: {goal}\n\nPlan:\n{plan}\n\nExecute the plan and report what you did."
                ),
            ),
            cancel,
        )
        .await?
        .text;
    Ok(format!("── plan ──\n{plan}\n\n── result ──\n{result}"))
}

/// One planner-emitted subtask in the dependency graph.
#[derive(Debug, Clone, serde::Deserialize)]
struct Subtask {
    id: String,
    task: String,
    #[serde(default)]
    depends_on: Vec<String>,
}

/// Extract a JSON subtask array from planner output (tolerates surrounding prose/code fences).
fn parse_subtasks(text: &str) -> Result<Vec<Subtask>> {
    if let (Some(s), Some(e)) = (text.find('['), text.rfind(']')) {
        if e > s {
            if let Ok(v) = serde_json::from_str::<Vec<Subtask>>(&text[s..=e]) {
                return Ok(v);
            }
        }
    }
    Err(Error::Other(format!(
        "planner did not return a JSON subtask array; got: {}",
        text.chars().take(200).collect::<String>()
    )))
}

/// Topologically group subtasks into waves: each wave's tasks depend only on earlier waves. Unknown
/// dependency ids are ignored; a true cycle is an error.
fn topo_waves(subtasks: &[Subtask]) -> Result<Vec<Vec<&Subtask>>> {
    let ids: std::collections::HashSet<&str> = subtasks.iter().map(|s| s.id.as_str()).collect();
    let mut done: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut remaining: Vec<&Subtask> = subtasks.iter().collect();
    let mut waves = Vec::new();
    while !remaining.is_empty() {
        let mut ready = Vec::new();
        let mut not = Vec::new();
        for s in remaining {
            let satisfied = s
                .depends_on
                .iter()
                .all(|d| done.contains(d) || !ids.contains(d.as_str()));
            if satisfied {
                ready.push(s);
            } else {
                not.push(s);
            }
        }
        if ready.is_empty() {
            return Err(Error::Other("dependency cycle in plan".into()));
        }
        for s in &ready {
            done.insert(s.id.clone());
        }
        waves.push(ready);
        remaining = not;
    }
    Ok(waves)
}

/// Dependency-wave plan-and-dispatch: the `planner` emits a JSON array of subtasks with
/// `depends_on`; subtasks are grouped into topological waves and each wave's `worker`s run **in
/// parallel**, with completed dependency results threaded into dependents' prompts.
pub async fn plan_and_dispatch_waves(
    spawner: &dyn Spawner,
    goal: &str,
    cancel: &CancellationToken,
) -> Result<String> {
    let plan_text = spawner
        .spawn(
            SpawnRequest::new(
                "planner",
                format!(
                    "Goal: {goal}\n\nBreak this into subtasks. Respond with ONLY a JSON array of \
                     objects with fields `id` (string), `task` (string), and `depends_on` (array of \
                     ids). No prose, no code fences."
                ),
            ),
            cancel,
        )
        .await?
        .text;
    let subtasks = parse_subtasks(&plan_text)?;
    if subtasks.is_empty() {
        return Err(Error::Other("planner produced no subtasks".into()));
    }
    let waves = topo_waves(&subtasks)?;

    let mut results: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut output = format!(
        "── plan ({} subtasks, {} waves) ──\n",
        subtasks.len(),
        waves.len()
    );
    for wave in &waves {
        if cancel.is_cancelled() {
            output.push_str("(interrupted — remaining waves skipped)\n");
            break;
        }
        // Build each worker future eagerly (deps context resolved from prior waves), then run the
        // whole wave concurrently.
        let futures = wave.iter().map(|st| {
            let deps_context = st
                .depends_on
                .iter()
                .filter_map(|d| results.get(d).map(|r| format!("[{d}] {r}")))
                .collect::<Vec<_>>()
                .join("\n");
            let prompt = format!(
                "Goal: {goal}\n\nSubtask: {}\n\nContext from completed dependencies:\n{}\n\n\
                 Execute this subtask and report what you did.",
                st.task, deps_context
            );
            let id = st.id.clone();
            async move {
                (
                    id,
                    spawner
                        .spawn(SpawnRequest::new("worker", prompt.clone()), cancel)
                        .await
                        .map(|o| o.text),
                )
            }
        });
        for (id, res) in futures::future::join_all(futures).await {
            // One worker failing must not discard its already-completed siblings (or skip later
            // waves): record the failure as that subtask's result and carry on. Dependents see the
            // failure note in their context rather than the whole dispatch aborting.
            match res {
                Ok(text) => {
                    output.push_str(&format!("── {id} ──\n{text}\n\n"));
                    results.insert(id, text);
                }
                Err(e) => {
                    output.push_str(&format!("── {id} (failed) ──\n{e}\n\n"));
                    results.insert(id, format!("(failed: {e})"));
                }
            }
        }
    }
    Ok(output)
}

/// Arguments for the `task` op.
#[derive(Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskInput {
    /// Sub-agent role name
    role: String,
    /// What the sub-agent should do
    task: String,
}

/// The `task` tool: delegate a subtask to a named role's sub-agent and return its result.
pub struct TaskTool;

#[async_trait]
impl Tool for TaskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "task".into(),
            description: "Delegate a self-contained subtask to a sub-agent role \
                          (e.g. scout, planner, worker) and receive its result."
                .into(),
            input_schema: tool_input_schema::<TaskInput>(),
            output_schema: None,
            // A sub-agent runs arbitrary work (on its own executor) over the SHARED workspace:
            // declaring Process makes every `task` dispatch bump the parent's op-cache
            // invalidation generation, so post-task reads never replay pre-task state (L-54
            // review, 2026-07-09).
            effects: vec![Effect::Process],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: Vec::new(),
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        // Best-effort: a missing/invalid `role` yields no subjects rather than failing here.
        serde_json::from_value::<TaskInput>(params.clone())
            .map(|args| vec![args.role])
            .unwrap_or_default()
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: TaskInput = parse_params(params, "task")?;
        let Some(spawner) = &ctx.spawner else {
            return Ok(ToolResult::error("no sub-agent spawner configured"));
        };
        // Thread a child of the parent turn's cancellation token (installed on the context per turn by
        // the engine) so cancelling the parent turn cancels the sub-agent. Outside a cancellable driver
        // (e.g. the one-shot SDK path) no token is installed and the sub-agent runs to completion.
        let cancel = ctx
            .cancel_token()
            .map(|t| t.child_token())
            .unwrap_or_default();
        // The active `with_tools` block scope (if any) narrows the child's tool set too — capabilities
        // only ever narrow on descent, and a sub-agent invocation is a descent step like any other.
        // `active_cap_scope` reads the SAME shared stack `Executor::dispatch` checks, so this call site
        // sees exactly the scope this `task` call itself is subject to. `parent_session` (installed on
        // the context per turn by the engine) rides along so the child's audit stream correlates back
        // to THIS turn (A-08).
        let request = SpawnRequest {
            role: args.role.clone(),
            task: args.task.clone(),
            cap_scope: ctx.active_cap_scope(),
            parent_session: ctx.session_id(),
            activity: ctx.spawn_activity_sink(),
        };
        match spawner.spawn(request, &cancel).await {
            Ok(outcome) => {
                // C-06 sub-agent rollup: the child's token spend doesn't flow back through
                // `ToolResult` (a plain string) — it rides the shared evidence log instead, the same
                // side-channel `turn.iteration` already uses for "this turn only" facts that aren't
                // part of a tool's own return value. The engine reads `subagent.usage` observations at
                // turn-end to (a) fold the tokens into the PARENT turn's total and (b) emit a
                // `CallUsage` attributed to the sub-agent's own model — so cost_summary prices the
                // child's spend under the model that actually generated it, not the parent's.
                if let Some(usage) = &outcome.usage {
                    ctx.evidence
                        .lock()
                        .unwrap()
                        .record(flux_evidence::Observation::new(
                            "subagent.usage",
                            flux_evidence::Phase::Turn,
                            serde_json::json!({
                                "role": args.role,
                                "model": outcome.model,
                                "usage": usage,
                            }),
                        ));
                }
                // One compact trace marker on the PARENT's evidence trail (A-08): the child's
                // session id + how many tool calls it made. The full child trail already lives
                // durably under its own correlated stream (C-14 flush inside the child's engine) —
                // this is the pointer, never a wholesale copy (no double-persist).
                ctx.evidence
                    .lock()
                    .unwrap()
                    .record(flux_evidence::Observation::new(
                        "subagent.trace",
                        flux_evidence::Phase::Turn,
                        serde_json::json!({
                            "role": args.role,
                            "session": outcome.session_id,
                            "tool_calls": outcome.tool_calls,
                            "model": outcome.model,
                        }),
                    ));
                Ok(ToolResult::ok(outcome.text))
            }
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::{Chunk, ContentBlock, StopReason};
    use flux_provider::{ChunkStream, Request};
    use flux_system::Workspace;
    use serde_json::json;

    fn request_has_tool(request: &Request, name: &str) -> bool {
        request.tools.iter().any(|tool| tool.name == name)
    }

    fn intent_chunks(intent: &str, families: &[&str]) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: "intent".into(),
                name: "declare_intent".into(),
                input: json!({
                    "intent": intent,
                    "capability_families": families,
                }),
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    fn native_call(id: &str, name: &str, input: Value) -> Vec<Chunk> {
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

    fn prose_chunks(text: &str) -> Vec<Chunk> {
        vec![
            Chunk::TextDelta(text.into()),
            Chunk::Block(ContentBlock::Text { text: text.into() }),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ]
    }

    #[test]
    fn child_activity_redaction_scrubs_json_keys_and_values() {
        const SECRET: &str = "SECRET-MAP-KEY-4711";
        let redactor = flux_secret::Redactor::new();
        redactor.add_secret(SECRET);
        let mut value = json!({
            SECRET: SECRET,
            "nested": [{ SECRET: format!("prefix-{SECRET}-suffix") }],
        });

        redact_spawn_json(&redactor, &mut value);

        let encoded = value.to_string();
        assert!(
            !encoded.contains(SECRET),
            "secret crossed activity: {encoded}"
        );
        assert!(encoded.contains("[redacted]"));
        assert!(
            value.get("[redacted]").is_some(),
            "top-level key was not scrubbed"
        );
        assert!(
            value["nested"][0].get("[redacted]").is_some(),
            "nested key was not scrubbed"
        );
    }

    /// Mock provider: returns a fixed text reply (one canned turn).
    struct MockProvider;
    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("complete the assigned task", &[])
            } else {
                prose_chunks("scouted: 3 files")
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// Mock provider: a fixed text reply carrying a `Usage` chunk, so a spawned sub-agent's turn
    /// bills tokens — the fixture C-06's rollup test needs (`MockProvider` above deliberately bills
    /// nothing, for spawner tests that don't care about usage).
    struct MockProviderWithUsage(Usage);
    #[async_trait]
    impl Provider for MockProviderWithUsage {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("complete the assigned task", &[])
            } else {
                let mut chunks = prose_chunks("did the subtask");
                chunks.insert(chunks.len() - 1, Chunk::Usage(self.0.clone()));
                chunks
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    fn temp_system() -> Arc<System> {
        let dir = std::env::temp_dir().join(format!("flux-orch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(System::new(Workspace::new(&dir).unwrap()))
    }

    /// A bare-text [`SpawnOutcome`] (no usage) — the mock spawners below only script text replies.
    fn text_outcome(text: impl Into<String>) -> SpawnOutcome {
        SpawnOutcome {
            text: text.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn spawner_runs_a_role_and_returns_text() {
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ndescription: recon\ntools: [read]\n---\nYou are a scout.",
            "scout",
        ));
        let spawner = LocalSpawner::new(
            Arc::new(|| Ok(Box::new(MockProvider))),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        );
        let cancel = CancellationToken::new();
        let out = spawner
            .spawn(SpawnRequest::new("scout", "look around"), &cancel)
            .await
            .unwrap();
        assert_eq!(out.text, "scouted: 3 files");
        assert!(spawner
            .spawn(SpawnRequest::new("nope", "x"), &cancel)
            .await
            .is_err());
    }

    /// A-79 failing-first: the child status/read lifecycle must reach the reporter WHILE the read
    /// is still blocked, with stable role/session/call correlation. The final prose remains the
    /// private SpawnOutcome instead of becoming an activity event.
    #[tokio::test]
    async fn spawn_streams_correlated_child_activity_before_the_child_finishes() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct StatusTool;
        #[async_trait]
        impl Tool for StatusTool {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only("ui.status", "show progress", json!({"type": "object"}))
                    .with_group("core")
            }
            async fn execute(&self, _ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
                Ok(ToolResult::ok("status shown to the user"))
            }
        }

        struct BlockingRead {
            entered: tokio::sync::mpsc::UnboundedSender<()>,
            release: Arc<tokio::sync::Notify>,
        }
        #[async_trait]
        impl Tool for BlockingRead {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only("account.read", "read config", json!({"type": "object"}))
                    .with_group("core")
            }
            async fn execute(&self, _ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
                let _ = self.entered.send(());
                self.release.notified().await;
                Ok(ToolResult::ok("PRIVATE-RESULT"))
            }
        }

        struct ActivityProvider {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for ActivityProvider {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, request: Request) -> Result<ChunkStream> {
                if request_has_tool(&request, "declare_intent") {
                    return Ok(Box::pin(futures::stream::iter(
                        intent_chunks("inspect the account", &["core"])
                            .into_iter()
                            .map(Ok),
                    )));
                }
                let chunks = match self.calls.fetch_add(1, Ordering::Relaxed) {
                    0 => {
                        let status = request
                            .tools
                            .iter()
                            .find(|tool| {
                                tool.name == "ui.status" || tool.name.starts_with("ui_status__")
                            })
                            .expect("ui.status is advertised")
                            .name
                            .clone();
                        let mut chunks = native_call(
                            "status-1",
                            &status,
                            json!({"message": "Checking account configuration"}),
                        );
                        chunks.insert(0, Chunk::ThinkingDelta("PRIVATE-REASONING".into()));
                        chunks
                    }
                    1 => {
                        let read = request
                            .tools
                            .iter()
                            .find(|tool| {
                                tool.name == "account.read"
                                    || tool.name.starts_with("account_read__")
                            })
                            .expect("account.read is advertised")
                            .name
                            .clone();
                        native_call("read-1", &read, json!({"query": "PRIVATE-QUERY"}))
                    }
                    _ => prose_chunks("child finished"),
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        #[derive(Default)]
        struct CaptureActivity(std::sync::Mutex<Vec<SpawnActivity>>);
        impl SpawnActivitySink for CaptureActivity {
            fn emit(&self, activity: SpawnActivity) {
                self.0.lock().unwrap().push(activity);
            }
        }

        let release = Arc::new(tokio::sync::Notify::new());
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut child_tools = ToolRegistry::new();
        child_tools.register(Arc::new(StatusTool));
        child_tools.register(Arc::new(BlockingRead {
            entered: entered_tx,
            release: release.clone(),
        }));
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ntools: [ui.status, account.read]\n---\nYou inspect account configuration.",
            "config-admin",
        ));
        let spawner = Arc::new(LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(ActivityProvider {
                    calls: AtomicUsize::new(0),
                }))
            }),
            Arc::new(roles),
            child_tools,
            temp_system(),
            "mock",
            1024,
        ));
        let activity = Arc::new(CaptureActivity::default());
        let mut request = SpawnRequest::new("config-admin", "inspect it");
        request.parent_session = Some("parent-1".into());
        request.activity = Some(activity.clone());
        let run = tokio::spawn(async move {
            spawner
                .spawn(request, &CancellationToken::new())
                .await
                .unwrap()
        });

        if entered_rx.recv().await.is_none() {
            let ended = run.await;
            panic!("the child ended before its read began: {ended:?}");
        }
        let while_blocked = activity.0.lock().unwrap().clone();
        release.notify_one();
        let outcome = run.await.unwrap();

        assert!(
            while_blocked
                .iter()
                .any(|a| matches!(&a.event, SpawnActivityEvent::Planning { active: true })),
            "child planning must reach the parent before the child finishes: {while_blocked:?}"
        );
        assert!(
            while_blocked.iter().any(|a| matches!(
                &a.event,
                SpawnActivityEvent::ToolCall { name, .. } if name == "ui.status"
            )),
            "the status call must reach the parent before the child finishes: {while_blocked:?}"
        );
        assert!(
            while_blocked.iter().any(|a| matches!(
                &a.event,
                SpawnActivityEvent::ToolCall { name, .. } if name == "account.read"
            )),
            "the blocked read must already be visible: {while_blocked:?}"
        );

        let all = activity.0.lock().unwrap().clone();
        assert!(!all.is_empty());
        let spawn_id = all[0].spawn_id;
        assert!(spawn_id > 0 && all.iter().all(|a| a.spawn_id == spawn_id));
        assert!(all.iter().all(|a| {
            a.role == "config-admin"
                && !a.child_session_id.is_empty()
                && a.parent_session.as_deref() == Some("parent-1")
        }));
        let read_call = all.iter().find_map(|a| match &a.event {
            SpawnActivityEvent::ToolCall { call_id, name, .. } if name == "account.read" => {
                Some(*call_id)
            }
            _ => None,
        });
        assert!(all.iter().any(|a| matches!(
            &a.event,
            SpawnActivityEvent::ToolResult { call_id, name, is_error: false }
                if name == "account.read" && Some(*call_id) == read_call
        )));
        let terminals: Vec<_> = all
            .iter()
            .filter_map(|activity| match &activity.event {
                SpawnActivityEvent::Finished { is_error, .. } => Some(*is_error),
                _ => None,
            })
            .collect();
        assert_eq!(
            terminals,
            vec![false],
            "one honest success terminal: {all:?}"
        );
        assert_eq!(outcome.text, "child finished");
        assert!(
            !format!("{all:?}").contains("PRIVATE-RESULT")
                && !format!("{all:?}").contains("PRIVATE-REASONING"),
            "result content and reasoning must remain private: {all:?}"
        );
    }

    /// A storeless LocalSpawner creates a fresh in-memory event store per child, so both sessions
    /// may be named `s_1`. The process-local spawn id must still disambiguate concurrent children
    /// of the same role (the correlation key customer surfaces use for paired activity).
    #[tokio::test]
    async fn concurrent_storeless_children_get_distinct_activity_ids() {
        #[derive(Default)]
        struct Capture(std::sync::Mutex<Vec<SpawnActivity>>);
        impl SpawnActivitySink for Capture {
            fn emit(&self, activity: SpawnActivity) {
                self.0.lock().unwrap().push(activity);
            }
        }

        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\n---\nYou are a worker.", "worker"));
        let spawner = LocalSpawner::new(
            Arc::new(|| Ok(Box::new(MockProvider))),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        );
        let activity = Arc::new(Capture::default());
        let request = |task: &str| {
            let mut request = SpawnRequest::new("worker", task);
            request.parent_session = Some("parent".into());
            request.activity = Some(activity.clone());
            request
        };

        let left_cancel = CancellationToken::new();
        let right_cancel = CancellationToken::new();
        let (left, right) = tokio::join!(
            spawner.spawn(request("left"), &left_cancel),
            spawner.spawn(request("right"), &right_cancel),
        );
        assert_eq!(left.unwrap().text, "scouted: 3 files");
        assert_eq!(right.unwrap().text, "scouted: 3 files");

        let finished: Vec<_> = activity
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|activity| matches!(activity.event, SpawnActivityEvent::Finished { .. }))
            .map(|activity| {
                (
                    activity.spawn_id,
                    activity.role.clone(),
                    activity.child_session_id.clone(),
                )
            })
            .collect();
        assert_eq!(finished.len(), 2, "one completion per child: {finished:?}");
        assert_ne!(finished[0].0, finished[1].0, "spawn ids must be unique");
        assert!(finished.iter().all(|(_, role, _)| role == "worker"));
    }

    /// A-79 propagation seam: a real parent FlowEngine must derive the L2 reporter from its owned
    /// turn channel and TaskTool must snapshot it into SpawnRequest. This complements the LocalSpawner
    /// forwarding test above; together they pin the full parent-engine → task → child callback path.
    #[tokio::test]
    async fn parent_engine_derives_the_spawn_activity_reporter() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ReportingSpawner;
        #[async_trait]
        impl Spawner for ReportingSpawner {
            async fn spawn(
                &self,
                request: SpawnRequest,
                _cancel: &CancellationToken,
            ) -> Result<SpawnOutcome> {
                let reporter = request
                    .activity
                    .expect("TaskTool snapshots the active parent reporter");
                for event in [
                    SpawnActivityEvent::ToolCall {
                        call_id: 7,
                        name: "account.read".into(),
                        input: json!({"query": "private"}),
                    },
                    SpawnActivityEvent::ToolResult {
                        call_id: 7,
                        name: "account.read".into(),
                        is_error: false,
                    },
                ] {
                    reporter.emit(SpawnActivity {
                        spawn_id: 7,
                        role: request.role.clone(),
                        child_session_id: "child-7".into(),
                        parent_session: request.parent_session.clone(),
                        depth: 1,
                        event,
                    });
                }
                Ok(SpawnOutcome {
                    text: "child answer".into(),
                    model: "mock".into(),
                    session_id: "child-7".into(),
                    tool_calls: 1,
                    ..Default::default()
                })
            }
        }

        struct ParentProvider {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for ParentProvider {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, request: Request) -> Result<ChunkStream> {
                if request_has_tool(&request, "declare_intent") {
                    return Ok(Box::pin(futures::stream::iter(
                        intent_chunks("delegate the read", &["process"])
                            .into_iter()
                            .map(Ok),
                    )));
                }
                let chunks = match self.calls.fetch_add(1, Ordering::Relaxed) {
                    0 => native_call(
                        "task-1",
                        "task",
                        json!({"role": "config-admin", "task": "inspect it"}),
                    ),
                    1 => native_call(
                        "finalize-1",
                        "finalize_plan",
                        json!({"instructions": "Report the delegated result."}),
                    ),
                    _ => prose_chunks("parent answer"),
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        #[derive(Default)]
        struct ParentSink {
            children: Vec<SpawnActivity>,
            turn_ends: usize,
        }
        impl AgentSink for ParentSink {
            fn observation(&mut self, observation: &flux_evidence::Observation) {
                if let Some(activity) = SpawnActivity::from_observation(observation) {
                    self.children.push(activity);
                }
            }
            fn turn_end(&mut self, _usage: Option<Usage>) {
                self.turn_ends += 1;
            }
        }

        let system = temp_system();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TaskTool));
        register_agent_ops(&mut registry);
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["task".into()], &[]),
            Arc::new(flux_runtime::AllowApprover),
            ToolContext::new(system).with_spawner(Arc::new(ReportingSpawner)),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = flux_flow::state::FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = flux_flow::engine::FlowEngine::assemble(
            Arc::new(ParentProvider {
                calls: AtomicUsize::new(0),
            }),
            executor,
            events.clone(),
            flow,
            "mock".into(),
            "test".into(),
            1024,
            5,
            Vec::new(),
            0,
            Vec::new(),
            std::env::temp_dir().join(format!("flux-orch-parent-activity-{}", std::process::id())),
        )
        .unwrap();
        let session = events.create_session("mock").unwrap();
        let mut sink = ParentSink::default();

        engine
            .run_turn(&session, "delegate this", &mut sink)
            .await
            .unwrap();

        assert_eq!(sink.children.len(), 2, "both child lifecycle events cross");
        assert!(sink.children.iter().all(|a| {
            a.role == "config-admin"
                && a.child_session_id == "child-7"
                && a.parent_session.as_deref() == Some(session.as_str())
        }));
        assert_eq!(
            sink.turn_ends, 1,
            "child completion cannot end the parent turn"
        );
    }

    /// A mock provider that names a real provider (not `"mock"`) and records the `model` string it
    /// actually received on the wire — used to prove a role's `model:` override reaches the
    /// provider request through [`flux_core::resolve_role_model`], not verbatim (A-41).
    struct ModelCapturingProvider {
        provider_name: &'static str,
        seen_model: std::sync::Mutex<Option<String>>,
        seen_reasoning: std::sync::Mutex<Option<(bool, Option<Effort>)>>,
    }

    #[async_trait]
    impl Provider for ModelCapturingProvider {
        fn name(&self) -> &str {
            self.provider_name
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            *self.seen_model.lock().unwrap() = Some(req.model.clone());
            *self.seen_reasoning.lock().unwrap() = Some((req.thinking, req.effort));
            let chunks = if request_has_tool(&req, "declare_intent") {
                intent_chunks("complete the assigned task", &[])
            } else {
                prose_chunks("ok")
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// A-41: a role's `model:` prefixed by the parent's OWN provider name must have that prefix
    /// stripped before it reaches the wire — the live failure was the full spec
    /// (`openrouter/deepseek/deepseek-v4-flash`) going out verbatim and 400ing mid-turn.
    #[tokio::test]
    async fn spawn_strips_role_model_prefix_matching_parent_provider() {
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\nmodel: openrouter/deepseek/deepseek-v4-flash\n---\nYou are a scout.",
            "scout",
        ));
        let provider = Arc::new(ModelCapturingProvider {
            provider_name: "openrouter",
            seen_model: std::sync::Mutex::new(None),
            seen_reasoning: std::sync::Mutex::new(None),
        });
        let provider_for_factory = provider.clone();
        let spawner = LocalSpawner::new(
            Arc::new(move || {
                let p = provider_for_factory.clone();
                Ok(Box::new(NameForwarding(p)) as Box<dyn Provider>)
            }),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        )
        .with_reasoning(true, Some(Effort::High));
        let out = spawner
            .spawn(
                SpawnRequest::new("scout", "look around"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.text, "ok");
        assert_eq!(
            provider.seen_model.lock().unwrap().as_deref(),
            Some("deepseek/deepseek-v4-flash"),
            "the parent's own provider prefix must be stripped before hitting the wire"
        );
        assert_eq!(
            *provider.seen_reasoning.lock().unwrap(),
            Some((true, Some(Effort::High))),
            "a role without reasoning keys inherits the parent policy"
        );
    }

    #[tokio::test]
    async fn role_reasoning_settings_override_parent_policy() {
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\nthinking: false\neffort: low\n---\nYou are a scout.",
            "scout",
        ));
        let provider = Arc::new(ModelCapturingProvider {
            provider_name: "openrouter",
            seen_model: std::sync::Mutex::new(None),
            seen_reasoning: std::sync::Mutex::new(None),
        });
        let provider_for_factory = provider.clone();
        let spawner = LocalSpawner::new(
            Arc::new(move || {
                Ok(Box::new(NameForwarding(provider_for_factory.clone())) as Box<dyn Provider>)
            }),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        )
        .with_reasoning(true, Some(Effort::High));
        spawner
            .spawn(
                SpawnRequest::new("scout", "look around"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            *provider.seen_reasoning.lock().unwrap(),
            Some((false, Some(Effort::Low)))
        );
    }

    /// Wraps a shared [`ModelCapturingProvider`] so the spawner's `provider_factory` (which returns
    /// an owned `Box<dyn Provider>` per sub-agent) can still forward calls into one shared instance
    /// the test asserts against afterwards.
    struct NameForwarding(Arc<ModelCapturingProvider>);
    #[async_trait]
    impl Provider for NameForwarding {
        fn name(&self) -> &str {
            self.0.name()
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            self.0.stream(req).await
        }
    }

    /// A-41: a role's `model:` naming a DIFFERENT provider than the parent must fail fast at spawn
    /// time with a diagnostic naming both providers — never reach the wire as a raw spec.
    #[tokio::test]
    async fn spawn_rejects_role_model_naming_a_different_provider() {
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\nmodel: anthropic/claude-sonnet-4-6\n---\nYou are a scout.",
            "scout",
        ));
        let provider = Arc::new(ModelCapturingProvider {
            provider_name: "openrouter",
            seen_model: std::sync::Mutex::new(None),
            seen_reasoning: std::sync::Mutex::new(None),
        });
        let spawner = LocalSpawner::new(
            Arc::new(move || Ok(Box::new(NameForwarding(provider.clone())) as Box<dyn Provider>)),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        );
        let err = spawner
            .spawn(
                SpawnRequest::new("scout", "look around"),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("openrouter"),
            "names the parent provider: {msg}"
        );
        assert!(
            msg.contains("anthropic"),
            "names the requested provider: {msg}"
        );
    }

    #[tokio::test]
    async fn restricted_sub_agent_runs_native_tool_with_loop_machinery() {
        use flux_runtime::{Tool, ToolContext};
        use std::sync::atomic::{AtomicUsize, Ordering};

        // A minimal read-only op the role is allowed to use; writes a marker iff it executes.
        struct Ping;
        #[async_trait]
        impl Tool for Ping {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only("ping", "p", json!({"type": "object"}))
            }
            async fn execute(&self, ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
                ctx.system.write_file("PINGED.marker", "1").await?;
                Ok(ToolResult::ok("pong"))
            }
        }

        // Intent, one native `ping` call, then prose. This fails unless `register_agent_ops`
        // re-added the adaptive/evidence machinery after the role's subset dropped it.
        struct PlanMock {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for PlanMock {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, request: Request) -> Result<ChunkStream> {
                if request_has_tool(&request, "declare_intent") {
                    return Ok(Box::pin(futures::stream::iter(
                        intent_chunks("ping the workspace", &["core"])
                            .into_iter()
                            .map(Ok),
                    )));
                }
                let n = self.calls.fetch_add(1, Ordering::Relaxed);
                let chunks = if n == 0 {
                    native_call("ping-1", "ping", json!({}))
                } else {
                    prose_chunks("done scouting")
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        let system = temp_system();
        let mut base = ToolRegistry::new();
        base.register(Arc::new(Ping));
        let mut roles = RoleRegistry::default();
        // Restricted role: only `ping`. The subset drops the loop-machinery ops, which
        // `register_agent_ops` must re-add for the flux-lang loop to run.
        roles.insert(parse_role(
            "---\ntools: [ping]\n---\nYou are a scout.",
            "scout",
        ));
        let spawner = LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(PlanMock {
                    calls: AtomicUsize::new(0),
                }))
            }),
            Arc::new(roles),
            base,
            system.clone(),
            "mock",
            1024,
        );
        let out = spawner
            .spawn(
                SpawnRequest::new("scout", "scout the repo"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.text, "done scouting");
        assert!(
            system.read_file("PINGED.marker").await.is_ok(),
            "the native op executed through the loop"
        );
    }

    // ---- sub-agent capability-scope intersection (L-11 acceptance #4) ----
    //
    // Reuses the module-level `Ping` (a marker-writing op) / `PingPlanMock` (plans one `ping` call then
    // finishes with prose) defined below for `injected_approver_governs_the_sub_agent` — same shape
    // `restricted_sub_agent_runs_a_plan_with_loop_machinery` uses locally, promoted to module scope
    // there already, so these tests just reuse it instead of redefining a third copy.

    /// A role whose OWN `tools` grant `ping` can still be blocked from using it once `spawn_scoped` is
    /// called with an active capability scope that excludes it — the sub-agent intersection: effective
    /// tools = `role.tools ∩ active_block_scope`, not just `role.tools`.
    #[tokio::test]
    async fn spawn_scoped_intersects_the_active_block_scope_with_the_roles_tools() {
        // A unique workspace (not the shared `temp_system()`) — this test asserts the marker file is
        // ABSENT, which a `Ping`-using test running concurrently in the shared dir would falsify.
        let system = unique_system("cap-scope-narrows");
        let mut base = ToolRegistry::new();
        base.register(Arc::new(Ping));
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ntools: [ping]\n---\nYou are a scout.",
            "scout",
        ));
        let spawner = LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(PingPlanMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }))
            }),
            Arc::new(roles),
            base,
            system.clone(),
            "mock",
            1024,
        );

        // The block scope `["read_only_other"]` does not include `ping` — even though the role grants
        // it — so the child's registry must not contain `ping` and the marker must never be written.
        let scope = vec!["read_only_other".to_string()];
        let out = spawner
            .spawn(
                SpawnRequest {
                    cap_scope: Some(scope),
                    ..SpawnRequest::new("scout", "scout the repo")
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.text, "done");
        assert!(
            system.read_file("PINGED.marker").await.is_err(),
            "the block scope must narrow the role's own tools away from `ping`"
        );
    }

    /// The counterpart: when the active scope DOES include the tool, the role ∩ scope intersection
    /// still lets it through — proving the intersection doesn't accidentally deny everything.
    #[tokio::test]
    async fn spawn_scoped_allows_a_tool_present_in_both_role_and_scope() {
        let system = unique_system("cap-scope-allows");
        let mut base = ToolRegistry::new();
        base.register(Arc::new(Ping));
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ntools: [ping]\n---\nYou are a scout.",
            "scout",
        ));
        let spawner = LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(PingPlanMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }))
            }),
            Arc::new(roles),
            base,
            system.clone(),
            "mock",
            1024,
        );
        let scope = vec!["ping".to_string()];
        let out = spawner
            .spawn(
                SpawnRequest {
                    cap_scope: Some(scope),
                    ..SpawnRequest::new("scout", "scout the repo")
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.text, "done");
        assert!(
            system.read_file("PINGED.marker").await.is_ok(),
            "ping is in both the role's tools and the active scope, so it must run"
        );
    }

    /// End-to-end through the real seam: `TaskTool::execute` reads the active scope off the live
    /// `ToolContext` (the same one `Executor::dispatch`'s gate reads) and passes it to `spawn_scoped` —
    /// so a `task` call issued from inside a `with_tools` block that excludes `ping` cannot let a
    /// `tools: [ping]` role touch the filesystem, with NO code in the flow itself naming the restriction
    /// beyond the surrounding scope.
    #[tokio::test]
    async fn task_tool_forwards_the_contexts_active_cap_scope_to_the_spawner() {
        let system = unique_system("cap-scope-task-tool");
        let mut base = ToolRegistry::new();
        base.register(Arc::new(Ping));
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ntools: [ping]\n---\nYou are a scout.",
            "scout",
        ));
        let spawner: Arc<dyn Spawner> = Arc::new(LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(PingPlanMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }))
            }),
            Arc::new(roles),
            base,
            system.clone(),
            "mock",
            1024,
        ));

        // Build an executor whose dispatch we can push a cap scope onto, then read that SAME context
        // via `TaskTool::execute` — proving `task` sees exactly the scope `dispatch` would enforce.
        let ctx = ToolContext::new(system.clone()).with_spawner(spawner);
        let mut task_registry = ToolRegistry::new();
        task_registry.register(Arc::new(TaskTool));
        let executor = Executor::new(
            task_registry,
            PermissionManager::new(),
            Arc::new(flux_runtime::AllowApprover),
            ctx,
        );

        // The active scope includes `task` itself (so the `task` call reaches `TaskTool::execute`) but
        // NOT `ping` — the tool the role would otherwise grant the child.
        let _scope = executor.push_cap_scope(&["task".to_string()]);
        let r = executor
            .dispatch("task", json!({"role": "scout", "task": "recon"}))
            .await;
        assert!(!r.is_error, "task itself succeeds: {}", r.content);
        assert_eq!(r.content, "done");
        assert!(
            system.read_file("PINGED.marker").await.is_err(),
            "the with_tools scope active at the `task` call site must narrow the child, \
             even though the role alone would allow `ping`"
        );
    }

    #[tokio::test]
    async fn task_tool_delegates_via_spawner() {
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\n---\nscout prompt", "scout"));
        let spawner: Arc<dyn Spawner> = Arc::new(LocalSpawner::new(
            Arc::new(|| Ok(Box::new(MockProvider))),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        ));
        let ctx = ToolContext::new(temp_system()).with_spawner(spawner);
        let r = TaskTool
            .execute(&ctx, json!({"role": "scout", "task": "recon"}))
            .await
            .unwrap();
        assert!(!r.is_error);
        assert_eq!(r.content, "scouted: 3 files");

        // No spawner → graceful error.
        let r2 = TaskTool
            .execute(
                &ToolContext::new(temp_system()),
                json!({"role": "scout", "task": "x"}),
            )
            .await
            .unwrap();
        assert!(r2.is_error);
    }

    /// C-06 sub-agent rollup, at the `TaskTool` seam: a spawned sub-agent's token usage rides back as
    /// a `subagent.usage` observation on the SHARED evidence log (the side-channel a `task` call uses
    /// to report structured usage `ToolResult` — a plain string — can't carry). A spawner whose child
    /// billed nothing (no usage returned) must record no observation at all, so a `mock`/free sub-agent
    /// doesn't pollute the log with a zero entry.
    #[tokio::test]
    async fn task_tool_records_subagent_usage_on_the_shared_evidence_log() {
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\n---\nscout prompt", "scout"));
        let usage = Usage {
            input_tokens: 321,
            output_tokens: 42,
            ..Default::default()
        };
        let spawner: Arc<dyn Spawner> = Arc::new(LocalSpawner::new(
            Arc::new({
                let usage = usage.clone();
                move || Ok(Box::new(MockProviderWithUsage(usage.clone())) as Box<dyn Provider>)
            }),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        ));
        let ctx = ToolContext::new(temp_system()).with_spawner(spawner);
        let r = TaskTool
            .execute(&ctx, json!({"role": "scout", "task": "recon"}))
            .await
            .unwrap();
        assert!(!r.is_error);

        let recorded: Vec<_> = ctx
            .evidence
            .lock()
            .unwrap()
            .by_kind("subagent.usage")
            .cloned()
            .collect();
        assert_eq!(
            recorded.len(),
            1,
            "one observation for the one sub-agent call"
        );
        assert_eq!(recorded[0].data["model"], "mock");
        assert_eq!(recorded[0].data["usage"]["input_tokens"], 321);
        assert_eq!(recorded[0].data["usage"]["output_tokens"], 42);

        // A sub-agent that bills nothing (MockProvider, no Usage chunk) records NO observation.
        let mut roles2 = RoleRegistry::default();
        roles2.insert(parse_role("---\n---\nscout prompt", "scout"));
        let free_spawner: Arc<dyn Spawner> = Arc::new(LocalSpawner::new(
            Arc::new(|| Ok(Box::new(MockProvider))),
            Arc::new(roles2),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        ));
        let ctx2 = ToolContext::new(temp_system()).with_spawner(free_spawner);
        TaskTool
            .execute(&ctx2, json!({"role": "scout", "task": "recon"}))
            .await
            .unwrap();
        assert_eq!(
            ctx2.evidence
                .lock()
                .unwrap()
                .by_kind("subagent.usage")
                .count(),
            0,
            "no usage reported by the child ⇒ no observation recorded"
        );
    }

    /// C-06 sub-agent rollup, end-to-end: a parent adaptive turn that calls `task` to delegate to a
    /// sub-agent must include the sub-agent's token spend in the parent turn's own `TurnEnded.usage`
    /// total — the failing-first acceptance test named by the story
    /// (`parent_turn_includes_subagent_usage`). The parent's own model-stage calls bill nothing,
    /// isolating the assertion to whether
    /// child's tokens actually reach the parent's total.
    #[tokio::test]
    async fn parent_turn_includes_subagent_usage() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // The child sub-agent bills real tokens.
        let child_usage = Usage {
            input_tokens: 1000,
            output_tokens: 200,
            ..Default::default()
        };
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\n---\nworker prompt", "worker"));
        let spawner: Arc<dyn flux_runtime::Spawner> = Arc::new(LocalSpawner::new(
            Arc::new({
                let usage = child_usage.clone();
                move || Ok(Box::new(MockProviderWithUsage(usage.clone())) as Box<dyn Provider>)
            }),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        ));

        // The parent declares delegation intent, captures and finalizes `task`, then answers from
        // the execution report (no parent usage — the point is to isolate the child's contribution).
        struct ParentMock {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for ParentMock {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, request: Request) -> Result<ChunkStream> {
                if request_has_tool(&request, "declare_intent") {
                    return Ok(Box::pin(futures::stream::iter(
                        intent_chunks("delegate the task", &["process"])
                            .into_iter()
                            .map(Ok),
                    )));
                }
                let n = self.calls.fetch_add(1, Ordering::Relaxed);
                let chunks = if n == 0 {
                    native_call("task-1", "task", json!({"role": "worker", "task": "do it"}))
                } else if n == 1 {
                    native_call(
                        "finalize-1",
                        "finalize_plan",
                        json!({"instructions": "Report the delegated task result."}),
                    )
                } else {
                    prose_chunks("delegated to the worker")
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        let system = temp_system();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TaskTool));
        register_agent_ops(&mut registry);
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["task".into()], &[]),
            Arc::new(flux_runtime::AllowApprover),
            ToolContext::new(system.clone()).with_spawner(spawner),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = flux_flow::state::FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = flux_flow::engine::FlowEngine::assemble(
            Arc::new(ParentMock {
                calls: AtomicUsize::new(0),
            }),
            executor,
            events.clone(),
            flow,
            "mock".into(),
            "test".into(),
            1024,
            5,
            Vec::new(),
            0,
            Vec::new(),
            std::env::temp_dir().join(format!("flux-orch-parent-{}", std::process::id())),
        )
        .unwrap();

        let sid = events.create_session("mock").unwrap();
        struct NullSink;
        impl AgentSink for NullSink {}
        let mut sink = NullSink;
        engine
            .run_turn(&sid, "delegate this", &mut sink)
            .await
            .unwrap();

        let turns = events.turns(&sid).unwrap();
        assert_eq!(turns.len(), 1);
        let usage = turns[0]
            .usage
            .as_ref()
            .expect("the parent turn's usage must be Some — the sub-agent billed tokens");
        assert_eq!(
            usage.input_tokens, 1000,
            "the sub-agent's input tokens reached the PARENT turn's total"
        );
        assert_eq!(
            usage.output_tokens, 200,
            "the sub-agent's output tokens reached the PARENT turn's total"
        );

        // The sub-agent's spend is ALSO individually attributed via CallUsage, to its own model —
        // so cost_summary prices it under the model that actually generated it.
        let raw = events.load_stream(&sid, None).unwrap();
        let call_usages: Vec<_> = raw
            .iter()
            .filter_map(|e| match &e.kind {
                flux_events::EventKind::CallUsage { model, usage } => {
                    Some((model.clone(), usage.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            call_usages.len(),
            1,
            "one CallUsage for the sub-agent's call (the parent's own planner billed nothing): {call_usages:?}"
        );
        assert_eq!(call_usages[0].0, "mock");
        assert_eq!(call_usages[0].1.input_tokens, 1000);
    }

    #[tokio::test]
    async fn sub_agent_refuses_destructive_native_action_batch() {
        use flux_policy::{Caller, CallerKind, Principal, Trust, TrustKind, TrustLevel};
        use flux_runtime::{Tool, ToolContext};
        use flux_spec::{
            Effect, Intent, IntentBehavior, IntentCertainty, IntentRole, IntentSet, IntentTarget,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        // A tool with a destructive intent that writes a marker iff it actually executes.
        struct FakeDestructive;
        #[async_trait]
        impl Tool for FakeDestructive {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only("danger", "d", json!({"type": "object"}))
                    .with_effects(vec![Effect::Process])
            }
            fn intents(&self, _p: &Value) -> IntentSet {
                let mut s = IntentSet::new();
                s.push(Intent {
                    behavior: IntentBehavior::CommandExecution,
                    target: IntentTarget::Process {
                        command: "rm -rf x".into(),
                    },
                    role: IntentRole::ProcessCommand,
                    certainty: IntentCertainty::Certain,
                });
                s
            }
            async fn execute(&self, ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
                ctx.system.write_file("EXECUTED.marker", "1").await?;
                Ok(ToolResult::ok("ran"))
            }
        }

        // The native call is captured into a host-built batch and finalized for aggregate approval.
        struct DestructiveMock {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for DestructiveMock {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, request: Request) -> Result<ChunkStream> {
                if request_has_tool(&request, "declare_intent") {
                    return Ok(Box::pin(futures::stream::iter(
                        intent_chunks("run a destructive command", &["process"])
                            .into_iter()
                            .map(Ok),
                    )));
                }
                let n = self.calls.fetch_add(1, Ordering::Relaxed);
                let chunks = if n == 0 {
                    native_call("danger-1", "danger", json!({}))
                } else if n == 1 {
                    native_call(
                        "finalize-1",
                        "finalize_plan",
                        json!({"instructions": "Report whether the command ran."}),
                    )
                } else {
                    prose_chunks("done")
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        let system = temp_system();
        let mut base = ToolRegistry::new();
        base.register(Arc::new(FakeDestructive));
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\n---\nworker", "worker"));

        let caller = Caller {
            principal: Principal {
                id: "t".into(),
                name: "t".into(),
                kind: CallerKind::User,
            },
            groups: Vec::new(),
            source: "test".into(),
        };
        let trust = Trust {
            kind: TrustKind::Invocation,
            level: TrustLevel::Privileged,
            scopes: Vec::new(),
        };
        let spawner = LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(DestructiveMock {
                    calls: AtomicUsize::new(0),
                }))
            }),
            Arc::new(roles),
            base,
            system.clone(),
            "mock",
            1024,
        )
        .with_authorization(flux_policy::default_local_grants(), caller, trust);

        let out = spawner
            .spawn(
                SpawnRequest::new("worker", "delete things"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            out.text.contains("not approved"),
            "unexpected result: {}",
            out.text
        );
        // The destructive tool was refused → its marker was never written.
        assert!(system.read_file("EXECUTED.marker").await.is_err());
    }

    #[test]
    fn parse_subtasks_tolerates_prose_and_fences() {
        let text = "Here is the plan:\n```json\n[{\"id\":\"a\",\"task\":\"x\",\"depends_on\":[]}]\n```\ndone";
        let subs = parse_subtasks(text).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].id, "a");
        assert!(parse_subtasks("no json here").is_err());
    }

    #[test]
    fn topo_waves_orders_by_dependency() {
        let subs = vec![
            Subtask {
                id: "c".into(),
                task: "c".into(),
                depends_on: vec!["a".into(), "b".into()],
            },
            Subtask {
                id: "a".into(),
                task: "a".into(),
                depends_on: vec![],
            },
            Subtask {
                id: "b".into(),
                task: "b".into(),
                depends_on: vec!["a".into()],
            },
        ];
        let waves = topo_waves(&subs).unwrap();
        assert_eq!(waves.len(), 3);
        assert_eq!(
            waves[0].iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["a"]
        );
        assert_eq!(
            waves[1].iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["b"]
        );
        assert_eq!(
            waves[2].iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["c"]
        );

        // a cycle is rejected
        let cyclic = vec![
            Subtask {
                id: "x".into(),
                task: "x".into(),
                depends_on: vec!["y".into()],
            },
            Subtask {
                id: "y".into(),
                task: "y".into(),
                depends_on: vec!["x".into()],
            },
        ];
        assert!(topo_waves(&cyclic).is_err());
    }

    #[tokio::test]
    async fn dispatch_waves_runs_subtasks_in_dependency_order() {
        // A spawner that returns a fixed plan from the planner and echoes the worker subtask.
        struct ScriptedSpawner;
        #[async_trait]
        impl Spawner for ScriptedSpawner {
            async fn spawn(
                &self,
                request: SpawnRequest,
                _cancel: &CancellationToken,
            ) -> Result<SpawnOutcome> {
                let (role, task) = (request.role.as_str(), request.task.as_str());
                match role {
                    "planner" => Ok(text_outcome(
                        r#"[
                        {"id":"a","task":"first","depends_on":[]},
                        {"id":"b","task":"second","depends_on":["a"]}
                    ]"#,
                    )),
                    "worker" => {
                        // report whether the dependency's result reached us
                        let saw_dep = task.contains("[a]");
                        Ok(text_outcome(format!("worker(saw_dep={saw_dep})")))
                    }
                    other => Err(Error::Other(format!("unknown role {other}"))),
                }
            }
        }
        let out = plan_and_dispatch_waves(&ScriptedSpawner, "goal", &CancellationToken::new())
            .await
            .unwrap();
        let a_at = out.find("── a ──").unwrap();
        let b_at = out.find("── b ──").unwrap();
        assert!(a_at < b_at, "a must complete before b");
        // b's prompt included a's result (dependency threading)
        assert!(out.contains("worker(saw_dep=true)"));
    }

    #[tokio::test]
    async fn dispatch_waves_stops_on_cancel_between_waves() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // The first worker cancels the shared token; the second wave must then be skipped.
        struct CancelSpawner {
            cancel: CancellationToken,
            workers: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl Spawner for CancelSpawner {
            async fn spawn(
                &self,
                request: SpawnRequest,
                _c: &CancellationToken,
            ) -> Result<SpawnOutcome> {
                match request.role.as_str() {
                    "planner" => Ok(text_outcome(
                        r#"[
                        {"id":"a","task":"x","depends_on":[]},
                        {"id":"b","task":"y","depends_on":["a"]}
                    ]"#,
                    )),
                    "worker" => {
                        self.workers.fetch_add(1, Ordering::SeqCst);
                        self.cancel.cancel();
                        Ok(text_outcome("did work"))
                    }
                    other => Err(Error::Other(format!("unknown role {other}"))),
                }
            }
        }

        let cancel = CancellationToken::new();
        let workers = Arc::new(AtomicUsize::new(0));
        let spawner = CancelSpawner {
            cancel: cancel.clone(),
            workers: workers.clone(),
        };
        let out = plan_and_dispatch_waves(&spawner, "goal", &cancel)
            .await
            .unwrap();
        assert_eq!(
            workers.load(Ordering::SeqCst),
            1,
            "only the wave-0 worker should run"
        );
        assert!(out.contains("── a ──"));
        assert!(
            !out.contains("── b ──"),
            "wave 1 must be skipped after cancel"
        );
        assert!(out.contains("interrupted"));
    }

    #[tokio::test]
    async fn dispatch_waves_keeps_sibling_results_when_one_worker_fails() {
        // Two independent subtasks in one wave: one fails, the other succeeds. The failure must not
        // discard the successful sibling or abort the whole dispatch.
        struct FlakySpawner;
        #[async_trait]
        impl Spawner for FlakySpawner {
            async fn spawn(
                &self,
                request: SpawnRequest,
                _c: &CancellationToken,
            ) -> Result<SpawnOutcome> {
                let (role, task) = (request.role.as_str(), request.task.as_str());
                match role {
                    "planner" => Ok(text_outcome(
                        r#"[
                        {"id":"a","task":"ok-one","depends_on":[]},
                        {"id":"b","task":"will-fail","depends_on":[]}
                    ]"#,
                    )),
                    "worker" if task.contains("will-fail") => Err(Error::Other("boom".into())),
                    "worker" => Ok(text_outcome("ok-one done")),
                    other => Err(Error::Other(format!("unknown role {other}"))),
                }
            }
        }
        let out = plan_and_dispatch_waves(&FlakySpawner, "goal", &CancellationToken::new())
            .await
            .unwrap();
        assert!(out.contains("ok-one done"), "sibling result kept: {out}");
        assert!(
            out.contains("(failed"),
            "failure recorded, not dropped: {out}"
        );
    }

    #[tokio::test]
    async fn plan_and_dispatch_runs_planner_then_worker() {
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\n---\nplanner prompt", "planner"));
        roles.insert(parse_role("---\n---\nworker prompt", "worker"));
        let spawner: Arc<dyn Spawner> = Arc::new(LocalSpawner::new(
            Arc::new(|| Ok(Box::new(MockProvider))),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        ));
        let out = plan_and_dispatch(spawner.as_ref(), "ship feature", &CancellationToken::new())
            .await
            .unwrap();
        assert!(out.contains("── plan ──"));
        assert!(out.contains("── result ──"));
        assert!(out.contains("scouted: 3 files"));
    }

    // ----- D-05 hardening -----

    /// A clean, per-test workspace (unique dir, wiped first) so marker files from one test can't leak
    /// into another running in parallel or a stale prior run.
    fn unique_system(tag: &str) -> Arc<System> {
        let dir = std::env::temp_dir().join(format!("flux-orch-d05-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(System::new(Workspace::new(&dir).unwrap()))
    }

    /// A provider that hangs forever on its first call — stands in for a runaway/stuck sub-agent.
    struct HangProvider;
    #[async_trait]
    impl Provider for HangProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _r: Request) -> Result<ChunkStream> {
            futures::future::pending::<()>().await;
            unreachable!()
        }
    }

    /// WS2: a wall-clock deadline fires the child's cancel token (cooperative termination) and surfaces
    /// a typed timeout error, instead of letting a stuck sub-agent run forever.
    #[tokio::test]
    async fn wall_clock_deadline_aborts_a_hung_sub_agent() {
        #[derive(Default)]
        struct Capture(std::sync::Mutex<Vec<SpawnActivity>>);
        impl SpawnActivitySink for Capture {
            fn emit(&self, activity: SpawnActivity) {
                self.0.lock().unwrap().push(activity);
            }
        }

        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\ntools: []\n---\nYou stall.", "sloth"));
        let spawner = LocalSpawner::new(
            Arc::new(|| Ok(Box::new(HangProvider))),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        )
        .with_limits(SpawnLimits {
            max_iterations: 30,
            max_tokens: 1024,
            wall_clock: Some(std::time::Duration::from_millis(100)),
        });

        // The 5s guard fails the test (rather than hanging CI) if the deadline doesn't fire.
        let activity = Arc::new(Capture::default());
        let mut request = SpawnRequest::new("sloth", "spin forever");
        request.activity = Some(activity.clone());
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            spawner.spawn(request, &CancellationToken::new()),
        )
        .await
        .expect("spawn should return by its wall-clock deadline, not hang");
        let err = out.expect_err("a hung sub-agent past its deadline must error");
        assert!(
            err.to_string().contains("wall-clock"),
            "expected a wall-clock timeout error, got: {err}"
        );
        let terminals: Vec<_> = activity
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|activity| match &activity.event {
                SpawnActivityEvent::Finished { is_error, .. } => Some(*is_error),
                _ => None,
            })
            .collect();
        assert_eq!(
            terminals,
            vec![true],
            "a timeout must emit exactly one failure terminal"
        );
    }

    /// WS2: cancelling the parent turn cancels the sub-agent. The `task` tool threads a child of the
    /// context's cancel token into the spawner — so a cancelled parent token stops a stuck child rather
    /// than the old orphan-token behaviour that let it run on regardless.
    #[tokio::test]
    async fn parent_cancellation_propagates_to_the_sub_agent() {
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\ntools: []\n---\nYou stall.", "sloth"));
        let spawner: Arc<dyn Spawner> = Arc::new(LocalSpawner::new(
            Arc::new(|| Ok(Box::new(HangProvider))),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        ));

        // A pre-cancelled parent token, installed on the context the way the engine installs it per
        // turn. With the orphan-token bug, `task` ignored it and the hung child ran forever.
        let cancel = CancellationToken::new();
        cancel.cancel();
        let ctx = ToolContext::new(temp_system()).with_spawner(spawner);
        ctx.set_cancel(cancel);

        let r = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            TaskTool.execute(&ctx, json!({"role": "sloth", "task": "spin forever"})),
        )
        .await;
        assert!(
            r.is_ok(),
            "task hung despite a cancelled parent token (orphan-token regression)"
        );
    }

    /// An op that writes a marker iff it actually executes (used to prove an approver blocked it).
    struct Ping;
    #[async_trait]
    impl Tool for Ping {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("ping", "p", json!({"type": "object"}))
        }
        async fn execute(&self, ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
            ctx.system.write_file("PINGED.marker", "1").await?;
            Ok(ToolResult::ok("pong"))
        }
    }

    /// A provider that selects and calls `ping`, then finishes with prose.
    struct PingPlanMock {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl Provider for PingPlanMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, request: Request) -> Result<ChunkStream> {
            if request_has_tool(&request, "declare_intent") {
                return Ok(Box::pin(futures::stream::iter(
                    intent_chunks("ping the workspace", &["core"])
                        .into_iter()
                        .map(Ok),
                )));
            }
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunks = if n == 0 {
                native_call("ping-1", "ping", json!({}))
            } else {
                prose_chunks("done")
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// WS3: an injected approver governs the sub-agent's tool calls. A deny-everything approver blocks
    /// the child's `ping` — which the default `SubAgentApprover` would have allowed.
    #[tokio::test]
    async fn injected_approver_governs_the_sub_agent() {
        struct DenyAll;
        #[async_trait]
        impl Approver for DenyAll {
            async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
                ApprovalChoice::Deny
            }
        }

        let system = unique_system("approver");
        let mut base = ToolRegistry::new();
        base.register(Arc::new(Ping));
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ntools: [ping]\n---\nYou are a scout.",
            "scout",
        ));
        let spawner = LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(PingPlanMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }))
            }),
            Arc::new(roles),
            base,
            system.clone(),
            "mock",
            1024,
        )
        .with_approver(Arc::new(DenyAll));

        let out = spawner
            .spawn(
                SpawnRequest::new("scout", "scout the repo"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.text, "done");
        assert!(
            system.read_file("PINGED.marker").await.is_err(),
            "the injected deny-all approver must block the child's ping"
        );
    }

    /// WS3 (isolation): a sub-agent inherits the parent's workspace-confined `System`, so a child op
    /// cannot read outside the workspace — the filesystem half of account isolation.
    #[tokio::test]
    async fn sub_agent_is_confined_to_the_parent_workspace() {
        /// Probes a path outside the workspace and records whether the guarded surface denied it.
        struct EscapeProbe;
        #[async_trait]
        impl Tool for EscapeProbe {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only("escape_probe", "p", json!({"type": "object"}))
            }
            async fn execute(&self, ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
                // Record the actual outcome so the test can distinguish a *confinement* denial from a
                // plain not-found (the error rendering carries the workspace-escape reason).
                let outcome = match ctx.system.read_file("../../../../../../etc/hostname").await {
                    Err(e) => format!("denied: {e}"),
                    Ok(_) => "LEAKED".to_string(),
                };
                ctx.system.write_file("PROBE.marker", &outcome).await?;
                Ok(ToolResult::ok("probed"))
            }
        }

        struct ProbePlanMock {
            calls: std::sync::atomic::AtomicUsize,
        }
        #[async_trait]
        impl Provider for ProbePlanMock {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, request: Request) -> Result<ChunkStream> {
                if request_has_tool(&request, "declare_intent") {
                    return Ok(Box::pin(futures::stream::iter(
                        intent_chunks("probe workspace confinement", &["core"])
                            .into_iter()
                            .map(Ok),
                    )));
                }
                let n = self
                    .calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let chunks = if n == 0 {
                    native_call("probe-1", "escape_probe", json!({}))
                } else {
                    prose_chunks("done")
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        let system = unique_system("escape");
        let mut base = ToolRegistry::new();
        base.register(Arc::new(EscapeProbe));
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ntools: [escape_probe]\n---\nYou probe.",
            "prober",
        ));
        let spawner = LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(ProbePlanMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }))
            }),
            Arc::new(roles),
            base,
            system.clone(),
            "mock",
            1024,
        );
        spawner
            .spawn(
                SpawnRequest::new("prober", "try to escape"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let marker = system.read_file("PROBE.marker").await.unwrap();
        assert!(
            marker.contains("escapes the workspace"),
            "the read must be denied as a workspace escape (not a not-found): got {marker:?}"
        );
    }

    /// WS4: with an audit store, the child's run (and its inner tool call) persists into the shared
    /// store the parent reads — instead of a throwaway in-memory one.
    #[tokio::test]
    async fn audit_store_captures_child_run_events() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        assert!(store.latest_session().unwrap().is_none());

        let mut base = ToolRegistry::new();
        base.register(Arc::new(Ping));
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ntools: [ping]\n---\nYou are a scout.",
            "scout",
        ));
        let spawner = LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(PingPlanMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }))
            }),
            Arc::new(roles),
            base,
            temp_system(),
            "mock",
            1024,
        )
        .with_audit(store.clone());

        spawner
            .spawn(
                SpawnRequest::new("scout", "scout the repo"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        let child = store
            .latest_session()
            .unwrap()
            .expect("child session created in the shared audit store");
        let trace = store.run_trace(&child).unwrap();
        assert!(
            !trace.is_empty(),
            "the child's run events should land in the shared audit store"
        );
    }

    /// WS4: auditing is gated on `with_audit`. Against **one** shared store: a non-audited spawn leaves
    /// it untouched, then an audited spawn (same store) routes the child into it — so the negative half
    /// can't pass vacuously (a broken gate that always/never wrote would fail one of the asserts).
    #[tokio::test]
    async fn audit_is_gated_on_with_audit() {
        fn scout_spawner() -> LocalSpawner {
            let mut roles = RoleRegistry::default();
            roles.insert(parse_role("---\n---\nscout prompt", "scout"));
            LocalSpawner::new(
                Arc::new(|| Ok(Box::new(MockProvider))),
                Arc::new(roles),
                ToolRegistry::new(),
                temp_system(),
                "mock",
                1024,
            )
        }

        let store = Arc::new(EventStore::in_memory().unwrap());

        // No `with_audit` → the child uses a throwaway store; the shared one stays empty.
        scout_spawner()
            .spawn(
                SpawnRequest::new("scout", "recon"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            store.latest_session().unwrap().is_none(),
            "a non-audited spawn must not write to the shared store"
        );

        // `with_audit(store)` → the same store now receives the child's session.
        scout_spawner()
            .with_audit(store.clone())
            .spawn(
                SpawnRequest::new("scout", "recon"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            store.latest_session().unwrap().is_some(),
            "with_audit must route the child's session into the shared store"
        );
    }

    /// A-08: an audited child's stream is CORRELATED — its session context names the role
    /// (`agent_id = subagent:<role>`) and points back at the parent session (`correlation_id`), so
    /// the shared store answers "what did the sub-agents of turn X do" with one indexed read.
    #[tokio::test]
    async fn sub_agent_run_lands_in_shared_audit_store_with_correlation() {
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\n---\nscout prompt", "scout"));
        let store = Arc::new(EventStore::in_memory().unwrap());
        let parent = store.create_session("mock").unwrap();

        let spawner = LocalSpawner::new(
            Arc::new(|| Ok(Box::new(MockProvider))),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        )
        .with_audit(store.clone());
        let outcome = spawner
            .spawn(
                SpawnRequest {
                    parent_session: Some(parent.clone()),
                    ..SpawnRequest::new("scout", "recon")
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_ne!(outcome.session_id, parent, "the child gets its own stream");
        let info = store.info(&outcome.session_id).unwrap();
        assert_eq!(
            info.context.agent_id.as_deref(),
            Some("subagent:scout"),
            "the child stream names its role"
        );
        assert_eq!(
            info.context.correlation_id.as_deref(),
            Some(parent.as_str()),
            "the child stream correlates back to the parent session"
        );
        // And the child's activity is durably there — its conversation landed in the shared store.
        assert!(
            !store.conversation(&outcome.session_id).unwrap().is_empty(),
            "the child's turn persisted under its own correlated stream"
        );
    }

    /// WS5: roles register in memory (no shared `.flux/agents` directory) and spawn.
    #[tokio::test]
    async fn in_memory_roles_spawn() {
        let roles = RoleRegistry::from_roles([Role {
            name: "scout".into(),
            description: "recon".into(),
            model: None,
            thinking: None,
            effort: None,
            agent_loop: None,
            tools: Some(Vec::new()),
            prompt: "You are a scout.".into(),
        }]);
        assert_eq!(roles.names(), vec!["scout"]);
        let spawner = LocalSpawner::new(
            Arc::new(|| Ok(Box::new(MockProvider))),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        );
        let out = spawner
            .spawn(
                SpawnRequest::new("scout", "look around"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.text, "scouted: 3 files");
    }

    /// WS5: `max_depth` bounds nested delegation. With the default (1) a child is a leaf and cannot
    /// delegate; with `max_depth = 2` it can — the grandchild runs and leaves its marker. A-25: an
    /// ancestor's active `with_tools` ceiling must carry down through that nested delegation too — a
    /// grandchild two hops away must not be able to resurrect a tool the ceiling excluded, even though
    /// the grandchild's own role declares it.
    #[tokio::test]
    async fn max_depth_bounds_nested_delegation() {
        /// The grandchild op: writes a marker iff a second-level sub-agent actually ran.
        struct GrandPing;
        #[async_trait]
        impl Tool for GrandPing {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only("ping", "p", json!({"type": "object"}))
            }
            async fn execute(&self, ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
                ctx.system.write_file("GRANDCHILD.marker", "1").await?;
                Ok(ToolResult::ok("pong"))
            }
        }

        /// Role-discriminating adaptive mock: a "DELEGATE" role proposes `task("inner", …)`; a
        /// leaf "inner" role gathers with `ping`. Each sub-agent gets a fresh provider instance.
        struct DepthMock {
            calls: std::sync::atomic::AtomicUsize,
        }
        #[async_trait]
        impl Provider for DepthMock {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, req: Request) -> Result<ChunkStream> {
                let is_delegator = req.system_text().unwrap_or_default().contains("DELEGATE");
                if request_has_tool(&req, "declare_intent") {
                    let families = if is_delegator {
                        vec!["process"]
                    } else {
                        vec!["core"]
                    };
                    return Ok(Box::pin(futures::stream::iter(
                        intent_chunks("complete the assigned role", &families)
                            .into_iter()
                            .map(Ok),
                    )));
                }
                let n = self
                    .calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let chunks = if n == 0 {
                    if is_delegator {
                        native_call(
                            "task-1",
                            "task",
                            json!({"role": "inner", "task": "do the thing"}),
                        )
                    } else {
                        native_call("ping-1", "ping", json!({}))
                    }
                } else if n == 1 && is_delegator {
                    native_call(
                        "finalize-1",
                        "finalize_plan",
                        json!({"instructions": "Report the nested task result."}),
                    )
                } else {
                    prose_chunks("done")
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        fn build(system: Arc<System>, max_depth: usize) -> LocalSpawner {
            let mut base = ToolRegistry::new();
            base.register(Arc::new(GrandPing));
            let mut roles = RoleRegistry::default();
            // Declares `task` among its own tools (A-25): `task` is gated on `task ∈ effective_tools`
            // like any other tool, so a role must actually declare it to delegate at all — this lets
            // scenario 3 below hand it an active `with_tools` scope that includes `task` but excludes
            // `ping`, isolating "the ceiling carries down" from "this role can't delegate at all".
            roles.insert(parse_role(
                "---\ntools: [task, ping]\n---\nYou DELEGATE to a sub-agent.",
                "delegator",
            ));
            roles.insert(parse_role(
                "---\ntools: [ping]\n---\nYou are a leaf.",
                "inner",
            ));
            LocalSpawner::new(
                Arc::new(|| {
                    Ok(Box::new(DepthMock {
                        calls: std::sync::atomic::AtomicUsize::new(0),
                    }))
                }),
                Arc::new(roles),
                base,
                system,
                "mock",
                1024,
            )
            .with_max_depth(max_depth)
        }

        // Default depth (1): the delegator is a leaf — its `task` call finds no tool, so no grandchild.
        let sys1 = unique_system("depth-leaf");
        build(sys1.clone(), 1)
            .spawn(
                SpawnRequest::new("delegator", "go"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            sys1.read_file("GRANDCHILD.marker").await.is_err(),
            "default max_depth=1 must keep children leaves (no nested delegation)"
        );

        // max_depth=2: the delegator may spawn the inner leaf, which runs `ping` and leaves its marker.
        let sys2 = unique_system("depth-nested");
        build(sys2.clone(), 2)
            .spawn(
                SpawnRequest::new("delegator", "go"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            sys2.read_file("GRANDCHILD.marker").await.is_ok(),
            "max_depth=2 must allow one level of nested delegation"
        );

        // max_depth=2, but the *caller's* active `with_tools` scope excludes `ping` (keeping `task` and
        // `read`): the delegator can still delegate (`task` is in scope), but the excluded `ping` must
        // not resurface at the grandchild two hops down even though the grandchild's own role (`inner`,
        // `tools: [ping]`) would otherwise grant it. Before the fix the child got a fresh, empty
        // cap-scope stack and the nested spawner re-subset the full base registry, so this ceiling was
        // dropped and the grandchild ran `ping` anyway.
        let sys3 = unique_system("depth-nested-scoped");
        build(sys3.clone(), 2)
            .spawn(
                SpawnRequest {
                    role: "delegator".into(),
                    task: "go".into(),
                    cap_scope: Some(vec!["task".into(), "read".into()]),
                    parent_session: None,
                    activity: None,
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            sys3.read_file("GRANDCHILD.marker").await.is_err(),
            "an active with_tools scope excluding `ping` must carry down to the grandchild two hops away"
        );
    }
}
