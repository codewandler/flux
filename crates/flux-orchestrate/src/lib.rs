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
#[allow(deprecated)]
pub use flux_agent::{parse_role, try_parse_role, Role, RoleRegistry};

pub mod fleet;
pub mod worker;

pub use fleet::{A2aSpawner, FleetCancelTool, FleetDispatchTool, FleetStatusTool};
pub use worker::{
    ExternalRuntime, FleetStartTool, FleetStopTool, FleetWorkerStatusTool, ProcessRuntime,
    DEFAULT_WORKER_BASE_PORT,
};

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_agent::{register_agent_ops, AdaptiveLoopPolicy};
use flux_core::{Error, Result, Usage};
use flux_events::EventStore;
use flux_flow::AgentSink;
use flux_policy::{AuthorizationPolicy, Caller, Trust};
use flux_provider::{Effort, Provider};
use flux_runtime::{
    active_runtime_turn_context, scope_runtime_turn, ApprovalChoice, Approver,
    AuthorityRequirement, ExecutionAuthorization, Executor, IdentityCell, PermissionManager,
    ResourceLimits, SpawnActivity, SpawnActivityEvent, SpawnActivitySink, SpawnOutcome,
    SpawnRequest, Spawner, Tool, ToolContext, ToolRegistry, ToolResult, SPAWN_CLEANUP_GRACE,
};
use flux_spec::{tool_input_schema, AccessKind, Effect, Idempotency, IntentSet, Risk, ToolSpec};
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
    /// finalizing assistant message) in a shared audit store. (The bounded
    /// [`SPAWN_CLEANUP_GRACE`] backstops a child that somehow doesn't observe the cancel.)
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
    /// Complete native intent/explore policy assigned to every role-derived child spec. Kept
    /// separate from `SpawnLimits`: those bound the whole child/outer loop, while this bounds the
    /// adaptive model stages inside one logical run.
    adaptive_policy: AdaptiveLoopPolicy,
    limits: SpawnLimits,
    /// Approver the sub-agent's tool calls dispatch through. `None` → the default [`SubAgentApprover`]
    /// (auto-approve non-destructive, deny destructive). A multi-tenant consumer injects an approver
    /// that approval-gates its mutations.
    approver: Option<Arc<dyn Approver>>,
    /// Authorization the sub-agents inherit (policy floor + immutable assembly-time identity).
    /// A lexical parent-turn identity takes precedence at spawn time, so multi-principal surfaces
    /// propagate the exact request principal without mutating this shared fallback.
    auth: Option<(AuthorizationPolicy, IdentityCell)>,
    /// When set, child runs persist into this shared (tenant) event store instead of a throwaway
    /// in-memory one, so a sub-agent's inner tool calls land in the audit log the parent reads.
    audit: Option<Arc<EventStore>>,
    /// C-299: the parent's resource ceilings, installed on every child executor as an
    /// [`ResourceLimits::independent_copy`] (same numbers, own concurrency budget — a shared one
    /// deadlocks). Default (unconfigured) leaves children unbounded, exactly as before.
    resource_limits: ResourceLimits,
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
            adaptive_policy: AdaptiveLoopPolicy::default(),
            limits: SpawnLimits::new(max_tokens),
            approver: None,
            auth: None,
            audit: None,
            resource_limits: ResourceLimits::new(),
            depth: 0,
            max_depth: 1,
        }
    }

    /// Give every sub-agent the parent runtime's resource ceilings (C-299) — before this, a
    /// `task`-delegated child ran on a fresh, **unbounded** executor.
    ///
    /// The ceiling is **per agent, not one shared budget**: each child gets a
    /// [`ResourceLimits::independent_copy`] with the same numbers and its own concurrency semaphore.
    /// So `max_concurrent_tool_calls = N` bounds each agent at N, and k live children may run up to
    /// N×(k+1) tool calls at once. Sharing one semaphore across the `task` boundary deadlocks — the
    /// agent-loop op driving the delegation (`execute_batch`) holds a permit for the child's whole
    /// turn, and the task-local exemption that covers the nested `task` does not cross the spawn the
    /// child is reached through. That reasoning, and why marking delegating ops does not fix it, is on
    /// [`ResourceLimits::independent_copy`].
    ///
    /// **k is bounded separately (C-444).** [`ResourceLimits::with_max_live_agents`] caps how many
    /// agents in this tree may be live at once, and unlike the semaphore that census *is* shared with
    /// every child — [`LocalSpawner::spawn`] takes a place from it and holds it for the child's whole
    /// turn. Sharing is sound there because it refuses rather than queues. With both set the tree total
    /// is `N × max_live_agents`; with only the per-agent number set, it is still unbounded.
    pub fn with_resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.resource_limits = limits;
        self
    }

    /// Bound spawned sub-agents by an authorization policy + resolved identity (inherited from the
    /// parent). Sub-agents then traverse the same policy floor as the top-level agent. Wraps the
    /// immutable fallback identity in a fresh handle; lexical turn identity still takes priority.
    pub fn with_authorization(
        self,
        policy: AuthorizationPolicy,
        caller: Caller,
        trust: Trust,
    ) -> Self {
        self.with_authorization_cell(policy, IdentityCell::new(caller, trust))
    }

    /// Like [`with_authorization`](Self::with_authorization), but sharing an externally-owned
    /// immutable fallback identity (typically the parent executor's).
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

    /// Set the complete adaptive intent/explore cognition policy inherited by every spawned role.
    /// This is independent of [`SpawnLimits`]: it selects stage models/effort and bounds native
    /// model calls, while spawn limits bound the child's outer iterations, fallback output tokens,
    /// and wall clock. The default is [`AdaptiveLoopPolicy::default`].
    pub fn with_adaptive_policy(mut self, policy: AdaptiveLoopPolicy) -> Self {
        self.adaptive_policy = policy;
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
    /// `system` is the CHILD's own active-system snapshot (C-100), so grandchildren inherit the
    /// root the child was spawned into, never the grandparent's assembly-time system.
    fn at_depth(
        &self,
        depth: usize,
        base_registry: ToolRegistry,
        system: Arc<System>,
    ) -> LocalSpawner {
        LocalSpawner {
            provider_factory: self.provider_factory.clone(),
            roles: self.roles.clone(),
            base_registry,
            system,
            default_model: self.default_model.clone(),
            default_thinking: self.default_thinking,
            default_effort: self.default_effort,
            adaptive_policy: self.adaptive_policy.clone(),
            limits: self.limits.clone(),
            approver: self.approver.clone(),
            auth: self.auth.clone(),
            audit: self.audit.clone(),
            // C-299: the ceiling descends through nested delegation too — a grandchild counts
            // against the same budget as the agent that started the chain.
            resource_limits: self.resource_limits.clone(),
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

        // C-444: claim this child's place in the TREE-WIDE agent census before building anything.
        // `max_concurrent_tool_calls` is per agent (see `ResourceLimits::independent_copy` for the
        // deadlock that forces it), so without a bound on the agent count the tree's total was
        // unbounded — N per agent × k children. The census is the bound on k, and it is shared across
        // this boundary rather than copied. Held for the whole child turn: `_census_slot` drops when
        // this function returns, on the error paths and the cancel path alike.
        let _census_slot = self
            .resource_limits
            .admit_agent()
            .map_err(|refusal| Error::Other(refusal.message()))?;

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
        register_agent_ops(&mut registry)?;

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
        // C-100: seed the child's own independent WorkspaceContext from the parent's active-system
        // snapshot when the request carries one (a parent inside a worktree session hands its
        // children the transitioned root). Fall back to the spawner's assembly-time system for
        // direct spawns that predate the snapshot (e.g. bare `SpawnRequest::new`). Either way the
        // child gets a FRESH WorkspaceContext — its own enter/leave never touches the parent.
        let child_system = request
            .system
            .clone()
            .unwrap_or_else(|| self.system.clone());
        let mut ctx = ToolContext::new(child_system.clone());
        if child_can_delegate {
            // Bounded nested delegation: the child keeps both halves of the delegation capability —
            // the `task` tool in its registry AND a depth-incremented spawner in its context. The
            // depth-next spawner is rebased on THIS child's own narrowed registry (base ∩
            // `effective_tools`), never the unrestricted `base_registry` — so a `with_tools` ceiling is
            // transitive across nested delegation: a grandchild role's `tools` allowlist can only ever
            // draw from a pool this ancestor has already narrowed, no matter how many hops down
            // (capabilities only ever narrow on descent, see `push_cap_scope`'s doc).
            // Nested delegation intentionally restores the canonical task handler after role
            // narrowing. Use the explicit replacement seam so an injected same-name handler can
            // never survive silently.
            registry.replace_from(
                "flux-orchestrate canonical nested task operation",
                Arc::new(TaskTool),
            )?;
            let child_base = self.base_registry.subset(effective_tools.as_deref());
            ctx = ctx.with_spawner(Arc::new(self.at_depth(
                child_depth,
                child_base,
                child_system.clone(),
            )));
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
        let authorization = if let Some((policy, cell)) = &self.auth {
            // Prefer the immutable identity scoped by the parent engine turn. The shared cell is
            // only an assembly-time fallback for direct spawns outside a driven turn.
            let (caller, trust) = active_runtime_turn_context()
                .and_then(|turn| turn.identity())
                .unwrap_or_else(|| cell.snapshot())
                .into_parts();
            ExecutionAuthorization::new(policy.clone(), caller, trust)
        } else {
            ExecutionAuthorization::local()
        };
        // C-299: the child runs under the parent's ceilings instead of the unbounded default it got
        // before. `independent_copy` — NOT `clone` — is load-bearing: a clone would share the
        // parent's semaphore, and the agent-loop op driving this delegation (`execute_batch`, and
        // equally `explore` / `flow_run` / a model stage) is holding a permit for this child's whole
        // turn. The nested `task` adds no second permit — same Tokio task, so `HELD_SLOTS` exempts
        // it — but one held permit is enough. The CHILD is reached across
        // `SpawnTaskSupervisor::spawn`, which that task-local does not cross, so a shared semaphore
        // would make the child queue behind the very call awaiting it: a deadlock bounded only by
        // the queue timeout. Hence per agent — same numbers, own budget.
        // See `ResourceLimits::independent_copy`.
        let executor = Executor::new_with_authorization(
            registry,
            PermissionManager::new(),
            approver,
            ctx,
            authorization,
        )
        .with_resource_limits(self.resource_limits.independent_copy());

        // The role *is* the agent definition: body → system prompt, `tools` already applied to the
        // scoped registry above, model inherits the spawner default when the role doesn't override it.
        let mut spec = role.to_spec(&self.default_model)?;
        // C-100: `role.to_spec` leaves `AgentSpec::default().cwd == "."`, which made the child
        // engine probe the PROCESS cwd for evidence surfacing — a latent bug even before
        // worktrees. Root the child at its own system's workspace so per-turn surfacing probes the
        // root it actually operates in (only when the spec still carries the "." default).
        if spec.cwd == std::path::Path::new(".") {
            spec.cwd = child_system.workspace().root().to_path_buf();
        }
        spec.thinking = role.thinking.unwrap_or(self.default_thinking);
        spec.effort = role.effort.or(self.default_effort);
        spec.adaptive_policy = self.adaptive_policy.clone();
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

        // The child runs under a child of the parent's cancel token. Parent cancellation and the
        // optional wall-clock deadline both fire that token, then bounded-await this SAME engine
        // future so its cancellation path persists the assistant terminal before ownership ends.
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
        enum StopTrigger {
            ParentCancellation,
            Deadline(std::time::Duration),
        }
        enum RunRace {
            Finished(Result<()>),
            Stopped(StopTrigger),
        }
        let (run_result, stop) = {
            let run = engine.run_turn_cancellable(&session_id, task, &mut sink, &run_cancel);
            tokio::pin!(run);
            let race = match self.limits.wall_clock {
                Some(dur) => tokio::select! {
                    biased;
                    _ = cancel.cancelled() => RunRace::Stopped(StopTrigger::ParentCancellation),
                    result = &mut run => RunRace::Finished(result),
                    _ = tokio::time::sleep(dur) => RunRace::Stopped(StopTrigger::Deadline(dur)),
                },
                None => tokio::select! {
                    biased;
                    _ = cancel.cancelled() => RunRace::Stopped(StopTrigger::ParentCancellation),
                    result = &mut run => RunRace::Finished(result),
                },
            };
            match race {
                RunRace::Finished(result) => (result, None),
                RunRace::Stopped(trigger) => {
                    run_cancel.cancel();
                    let cleanup = tokio::time::timeout(SPAWN_CLEANUP_GRACE, &mut run)
                        .await
                        .unwrap_or_else(|_| {
                            Err(Error::Other(format!(
                                "sub-agent '{role_name}' did not stop within the \
                                 {SPAWN_CLEANUP_GRACE:?} cancellation grace"
                            )))
                        });
                    (cleanup, Some(trigger))
                }
            }
        };
        // The child's accumulated per-turn usage (C-06 sub-agent rollup): `TaskTool` folds this into
        // the PARENT turn's tally and records it as a `CallUsage` attributed to the child's own model,
        // so the sub-agent's spend counts toward the parent's total without being double-attributed to
        // the parent's model. `None` when the child billed nothing (e.g. `mock`, or no usage reported).
        let usage = engine.loop_host.turn_usage();
        let usage = (usage.total() > 0).then_some(usage);
        let cancelled = sink.cancelled;
        sink.finish(
            stop.is_some() || cancelled || run_result.is_err(),
            usage.clone(),
        );
        if let Some(stop) = stop {
            let cleanup = run_result
                .err()
                .map(|error| format!("; cleanup failed: {error}"));
            return Err(Error::Other(match stop {
                StopTrigger::ParentCancellation => format!(
                    "sub-agent '{role_name}' was cancelled{}",
                    cleanup.as_deref().unwrap_or_default()
                ),
                StopTrigger::Deadline(dur) => format!(
                    "sub-agent '{role_name}' exceeded its {dur:?} wall-clock limit{}",
                    cleanup.as_deref().unwrap_or_default()
                ),
            }));
        }
        run_result?;
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
/// knobs. [`SubAgents::into_spawner`] keeps the standard child cognition policy, while
/// [`SubAgents::into_spawner_with_adaptive_policy`] is the explicit-policy sibling; the surface then
/// registers [`TaskTool`] into its own catalog and installs the returned spawner via
/// `ToolContext::with_spawner`.
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
    /// C-299: the resource ceilings every sub-agent inherits (per agent, own budget). Set it with
    /// [`with_resource_limits`](Self::with_resource_limits) — the SDK's client builders and the CLI
    /// fill it in from what the host/operator configured, so a delegating host does not have to.
    pub resource_limits: ResourceLimits,
}

impl SubAgents {
    /// A bundle with default limits for `max_tokens`; everything else off (no approver override,
    /// documented local authorization, no audit store, children are leaves). Set those with the
    /// `with_*` methods.
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
            resource_limits: ResourceLimits::new(),
        }
    }

    /// Give every sub-agent the parent runtime's resource ceilings (C-299) — **per agent**, see
    /// [`LocalSpawner::with_resource_limits`].
    pub fn with_resource_limits(mut self, limits: ResourceLimits) -> Self {
        self.resource_limits = limits;
        self
    }

    /// Inherit an authorization policy + resolved identity (the parent's floor) for every sub-agent.
    /// Wraps the immutable fallback identity in a fresh handle; lexical turn identity still takes
    /// priority.
    pub fn with_authorization(
        self,
        policy: AuthorizationPolicy,
        caller: Caller,
        trust: Trust,
    ) -> Self {
        self.with_authorization_cell(policy, IdentityCell::new(caller, trust))
    }

    /// Like [`with_authorization`](Self::with_authorization), but sharing an externally-owned
    /// immutable fallback identity (typically the parent executor's).
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
        self.into_spawner_with_adaptive_policy(system, AdaptiveLoopPolicy::default())
    }

    /// Build the spawner with an explicit native intent/explore policy for every child. This is an
    /// additive alternative to [`into_spawner`](Self::into_spawner), whose defaults remain
    /// unchanged for existing callers. The policy propagates through bounded nested delegation.
    pub fn into_spawner_with_adaptive_policy(
        self,
        system: Arc<System>,
        adaptive_policy: AdaptiveLoopPolicy,
    ) -> Arc<dyn Spawner> {
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
        .with_adaptive_policy(adaptive_policy)
        .with_limits(limits)
        .with_max_depth(self.max_depth)
        // C-299: carry the parent's ceilings down. Unset is `ResourceLimits::new()` (unbounded),
        // so a host that configured nothing sees no behaviour change.
        .with_resource_limits(self.resource_limits);
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
            // Backstop panics or a supervisor abort after the cooperative grace. Ordinary success,
            // error, timeout, and cancellation all call `finish` explicitly in `LocalSpawner`.
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
        // Every node kind, keys included — the tree's one total walk (C-323, consolidated in
        // C-338). A registered all-digit credential has no other protection, so a skipped `Number`
        // node is a hole in `add_secret`'s guarantee rather than an optimization.
        flux_core::redact_json_total(&mut input, &|text| self.redactor.redact(text));
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
        flux_core::redact_json_total(&mut observation.data, &|text| self.redactor.redact(text));
        self.emit(SpawnActivityEvent::Observation { observation });
    }

    fn turn_end(&mut self, usage: Option<Usage>) {
        // Cache engine usage, but defer the terminal event until `LocalSpawner::spawn` knows
        // whether the overall operation succeeded (a timeout may occur after engine finalization).
        self.terminal_usage = usage;
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
            // A sub-agent invokes a provider and may run arbitrary work (on its own executor) over
            // the SHARED workspace. The Process effect bumps the parent's op-cache invalidation
            // generation, so post-task reads never replay pre-task state (L-54
            // review, 2026-07-09); Provider is the exact authority family, not OS-process access.
            effects: vec![Effect::Process],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Provider],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        // Best-effort: a missing/invalid `role` yields no subjects rather than failing here.
        serde_json::from_value::<TaskInput>(params.clone())
            .map(|args| vec![args.role])
            .unwrap_or_default()
    }

    fn authority_requirements(
        &self,
        _params: &Value,
        subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        let role = subjects.first().map(String::as_str).unwrap_or("sub-agent");
        Ok(vec![AuthorityRequirement::provider_invoke(role)])
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: TaskInput = parse_params(params, "task")?;
        let Some(spawner) = &ctx.spawner else {
            return Ok(ToolResult::error("no sub-agent spawner configured"));
        };
        // Thread a child of the parent turn's cancellation token (installed on the context per turn by
        // the engine) so cancelling the parent turn cancels the sub-agent. Outside a cancellable driver
        // (e.g. the one-shot SDK path) no token is installed and the sub-agent runs to completion.
        let supervisor = ctx.spawn_supervisor();
        let cancel = supervisor
            .as_ref()
            .map(|owner| owner.child_token())
            .or_else(|| ctx.cancel_token().map(|token| token.child_token()))
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
            // C-100: snapshot the parent context's ACTIVE system at delegation time so a child
            // spawned inside a worktree session inherits the transitioned root (with its own
            // independent WorkspaceContext — a child transition never affects the parent).
            system: Some(ctx.system()),
        };
        let spawned = if let Some(supervisor) = supervisor {
            let spawner = spawner.clone();
            let child_cancel = cancel.clone();
            let runtime_turn = ctx.runtime_turn_context();
            supervisor
                .spawn(scope_runtime_turn(runtime_turn, async move {
                    spawner.spawn(request, &child_cancel).await
                }))
                .await
                .map_err(|error| {
                    Error::Other(format!("sub-agent supervisor task failed: {error}"))
                })?
        } else {
            spawner.spawn(request, &cancel).await
        };
        match spawned {
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

    fn parse_role(content: &str, name_fallback: &str) -> Role {
        try_parse_role(content, name_fallback).unwrap()
    }

    fn request_has_tool(request: &Request, name: &str) -> bool {
        request.tools.iter().any(|tool| tool.name == name)
    }

    fn request_has_intent_family(request: &Request, family: &str) -> bool {
        request
            .tools
            .iter()
            .find(|tool| tool.name == "declare_intent")
            .and_then(|tool| {
                tool.input_schema
                    .pointer("/properties/capability_families/items/enum")
            })
            .and_then(Value::as_array)
            .is_some_and(|families| families.iter().any(|value| value.as_str() == Some(family)))
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

        flux_core::redact_json_total(&mut value, &|text| redactor.redact(text));

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

    /// C-323 — the same walker skipped `Value::Number`, and an all-digit credential has no recourse
    /// but registration, so a skipped node kind is a hole in `add_secret`'s guarantee. The second
    /// half is the anti-censorship posture: an ordinary number keeps its value *and its type*.
    #[test]
    fn child_activity_redaction_reaches_a_registered_numeric_credential() {
        const NUMERIC: &str = "216216216216216218";
        let redactor = flux_secret::Redactor::new();
        redactor.add_secret(NUMERIC);
        let mut value = json!({
            "account_id": 216_216_216_216_216_218_i64,
            "nested": [216_216_216_216_216_218_i64],
            "port": 8080,
            "ok": true,
            "none": null,
        });

        flux_core::redact_json_total(&mut value, &|text| redactor.redact(text));

        let encoded = value.to_string();
        assert!(
            !encoded.contains(NUMERIC),
            "a registered numeric credential crossed activity: {encoded}"
        );
        assert_eq!(value["account_id"], "[redacted]");
        assert_eq!(value["nested"][0], "[redacted]");
        assert_eq!(value["port"], 8080);
        assert!(
            value["port"].is_number(),
            "an unregistered number keeps its type: {encoded}"
        );
        assert_eq!(value["ok"], true);
        assert!(value["none"].is_null());
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

    /// C-117 failing-first (the live repro): a persisted composite requiring ops outside a
    /// `tools: [read]` role's narrowed registry must not fail `LocalSpawner::spawn` — before the
    /// fix, EVERY spawn of EVERY role died at child-engine assembly with
    /// `composite validation failed: unknown operation: …`. The unresolvable definition is simply
    /// pruned from the child's catalog and the turn completes.
    #[tokio::test]
    async fn spawn_survives_unresolvable_persisted_composite() {
        let dir = std::env::temp_dir().join(format!("flux-orch-c117-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".flux/flows")).unwrap();
        std::fs::write(
            dir.join(".flux/flows/mr_update.flux"),
            "op mr_update() -> any\n  $x = gitlab_mr_show()\n  return $x\n",
        )
        .unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ndescription: recon\ntools: [read]\n---\nYou are a scout.",
            "scout",
        ));
        let spawner = LocalSpawner::new(
            Arc::new(|| Ok(Box::new(MockProvider))),
            Arc::new(roles),
            ToolRegistry::new(),
            system,
            "mock",
            1024,
        );
        let cancel = CancellationToken::new();
        let out = spawner
            .spawn(SpawnRequest::new("scout", "look around"), &cancel)
            .await
            .expect("spawn must not fail on an unresolvable persisted composite");
        assert_eq!(out.text, "scouted: 3 files");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A-82 failing-first: the reusable `SubAgents` path must put the host's complete adaptive
    /// policy on the role-derived child `AgentSpec` before engine assembly. Request capture proves
    /// the stage-local wire settings; two refusal cases prove the stage and logical call ceilings
    /// stop before an extra provider request rather than merely being stored on the spawner.
    #[tokio::test]
    async fn explicit_child_adaptive_policy_reaches_both_native_stages() {
        #[derive(Default)]
        struct CaptureProvider {
            requests: Arc<std::sync::Mutex<Vec<Request>>>,
            invalid_first_intent: bool,
        }

        #[async_trait]
        impl Provider for CaptureProvider {
            fn name(&self) -> &str {
                "mock"
            }

            async fn stream(&self, request: Request) -> Result<ChunkStream> {
                let request_index = {
                    let mut requests = self.requests.lock().unwrap();
                    let index = requests.len();
                    requests.push(request.clone());
                    index
                };
                let chunks = if request_has_tool(&request, "declare_intent") {
                    if self.invalid_first_intent && request_index == 0 {
                        prose_chunks("I forgot to declare intent")
                    } else {
                        intent_chunks("answer the child task", &[])
                    }
                } else {
                    prose_chunks("child done")
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        fn roles() -> RoleRegistry {
            let mut roles = RoleRegistry::default();
            roles.insert(parse_role("---\n---\nYou are a worker.", "worker"));
            roles
        }

        let policy = flux_agent::AdaptiveLoopPolicy {
            max_model_calls: 2,
            intent: flux_agent::AgentStagePolicy {
                model: Some("intent-fast".into()),
                effort: Some(Effort::Low),
                max_tokens: Some(111),
                max_calls: Some(1),
            },
            explore: flux_agent::AgentStagePolicy {
                model: Some("explore-deep".into()),
                effort: Some(Effort::High),
                max_tokens: Some(222),
                max_calls: Some(1),
            },
        };
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider_requests = requests.clone();
        let spawner = SubAgents::new(
            roles(),
            ToolRegistry::new(),
            Arc::new(move || {
                Ok(Box::new(CaptureProvider {
                    requests: provider_requests.clone(),
                    invalid_first_intent: false,
                }) as Box<dyn Provider>)
            }),
            "child-default",
            4096,
        )
        .into_spawner_with_adaptive_policy(temp_system(), policy.clone());
        let outcome = spawner
            .spawn(
                SpawnRequest::new("worker", "do the work"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.text, "child done");

        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].trace.as_ref().unwrap().stage, "intent");
            assert_eq!(requests[0].model, "intent-fast");
            assert_eq!(requests[0].effort, Some(Effort::Low));
            assert_eq!(requests[0].max_tokens, 111);
            assert_eq!(requests[1].trace.as_ref().unwrap().stage, "explore");
            assert_eq!(requests[1].model, "explore-deep");
            assert_eq!(requests[1].effort, Some(Effort::High));
            assert_eq!(requests[1].max_tokens, 222);
        }

        // A per-stage cap of one refuses intent repair before a second wire request.
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider_requests = requests.clone();
        let spawner = LocalSpawner::new(
            Arc::new(move || {
                Ok(Box::new(CaptureProvider {
                    requests: provider_requests.clone(),
                    invalid_first_intent: true,
                }) as Box<dyn Provider>)
            }),
            Arc::new(roles()),
            ToolRegistry::new(),
            temp_system(),
            "child-default",
            4096,
        )
        .with_adaptive_policy(policy.clone());
        let stopped = spawner
            .spawn(
                SpawnRequest::new("worker", "do the work"),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            stopped.contains("adaptive `intent` model-call cap exhausted"),
            "{stopped}"
        );
        assert_eq!(requests.lock().unwrap().len(), 1);

        // The logical total spans stages: one allowed intent call means exploration is refused
        // before the provider sees a second request.
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider_requests = requests.clone();
        let mut total_capped = policy;
        total_capped.max_model_calls = 1;
        let spawner = LocalSpawner::new(
            Arc::new(move || {
                Ok(Box::new(CaptureProvider {
                    requests: provider_requests.clone(),
                    invalid_first_intent: false,
                }) as Box<dyn Provider>)
            }),
            Arc::new(roles()),
            ToolRegistry::new(),
            temp_system(),
            "child-default",
            4096,
        )
        .with_adaptive_policy(total_capped);
        let stopped = spawner
            .spawn(
                SpawnRequest::new("worker", "do the work"),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(stopped.contains("model-call budget exhausted"), "{stopped}");
        assert!(stopped.contains("1/1"), "{stopped}");
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn sub_agent_adaptive_policy_defaults_and_nested_inheritance_are_stable() {
        let spawner = LocalSpawner::new(
            Arc::new(|| Ok(Box::new(MockProvider))),
            Arc::new(RoleRegistry::default()),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        );
        assert_eq!(
            spawner.adaptive_policy,
            flux_agent::AdaptiveLoopPolicy::default()
        );

        let policy = flux_agent::AdaptiveLoopPolicy {
            max_model_calls: 3,
            ..flux_agent::AdaptiveLoopPolicy::default()
        };
        let spawner = spawner.with_adaptive_policy(policy.clone());
        let nested = spawner.at_depth(1, ToolRegistry::new(), temp_system());
        assert_eq!(nested.adaptive_policy, policy);
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
        register_agent_ops(&mut registry).unwrap();
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

    /// Cancelling a parent while its `task` call is still awaiting a child must surface the
    /// child's drop-time failure terminal before the parent engine tears down its activity channel.
    #[tokio::test]
    async fn parent_cancellation_delivers_the_child_terminal_event() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct PendingCollectorSpawner {
            entered: tokio::sync::mpsc::UnboundedSender<()>,
        }
        #[async_trait]
        impl Spawner for PendingCollectorSpawner {
            async fn spawn(
                &self,
                request: SpawnRequest,
                cancel: &CancellationToken,
            ) -> Result<SpawnOutcome> {
                let collector = TextCollector::new(
                    request.activity,
                    77,
                    request.role,
                    "child-77".into(),
                    request.parent_session,
                    1,
                    flux_secret::Redactor::new(),
                );
                let _ = self.entered.send(());
                cancel.cancelled().await;
                drop(collector);
                Err(Error::Other("cancelled test child".into()))
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
                        json!({"role": "sloth", "task": "wait for cancellation"}),
                    ),
                    1 => native_call(
                        "finalize-1",
                        "finalize_plan",
                        json!({"instructions": "Wait for the delegated result."}),
                    ),
                    _ => prose_chunks("unexpected parent continuation"),
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        #[derive(Default)]
        struct ParentSink {
            children: Vec<SpawnActivity>,
        }
        impl AgentSink for ParentSink {
            fn observation(&mut self, observation: &flux_evidence::Observation) {
                if let Some(activity) = SpawnActivity::from_observation(observation) {
                    self.children.push(activity);
                }
            }
        }

        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let child_spawner: Arc<dyn Spawner> = Arc::new(PendingCollectorSpawner {
            entered: entered_tx,
        });

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TaskTool));
        register_agent_ops(&mut registry).unwrap();
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["task".into()], &[]),
            Arc::new(flux_runtime::AllowApprover),
            ToolContext::new(temp_system()).with_spawner(child_spawner),
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
            std::env::temp_dir().join(format!(
                "flux-orch-parent-cancel-activity-{}",
                std::process::id()
            )),
        )
        .unwrap();
        let session = events.create_session("mock").unwrap();
        let cancel = CancellationToken::new();
        let mut sink = ParentSink::default();

        {
            let turn = engine.run_turn_cancellable(&session, "delegate this", &mut sink, &cancel);
            tokio::pin!(turn);
            tokio::select! {
                entered = entered_rx.recv() => {
                    assert!(entered.is_some(), "child provider closed before its task started");
                }
                result = &mut turn => {
                    panic!("parent turn ended before the child was in flight: {result:?}");
                }
            }
            cancel.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(5), &mut turn)
                .await
                .expect("parent cancellation must finish promptly")
                .unwrap();
        }

        let terminals: Vec<_> = sink
            .children
            .iter()
            .filter_map(|activity| match activity.event {
                SpawnActivityEvent::Finished { is_error, .. } => Some(is_error),
                _ => None,
            })
            .collect();
        assert_eq!(
            terminals,
            vec![true],
            "the cancelled child must leave exactly one visible failure terminal: {:?}",
            sink.children
        );
    }

    #[tokio::test]
    async fn parent_cancellation_finalizes_an_audited_hanging_child() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Default)]
        struct HangState {
            active: AtomicUsize,
            entered: tokio::sync::Notify,
            dropped: tokio::sync::Notify,
        }

        impl HangState {
            async fn wait_until_entered(&self) {
                loop {
                    let entered = self.entered.notified();
                    if self.active.load(Ordering::SeqCst) > 0 {
                        return;
                    }
                    entered.await;
                }
            }

            async fn wait_until_dropped(&self) {
                loop {
                    let dropped = self.dropped.notified();
                    if self.active.load(Ordering::SeqCst) == 0 {
                        return;
                    }
                    dropped.await;
                }
            }
        }

        struct AuditedHangStream {
            state: Arc<HangState>,
        }

        impl futures::Stream for AuditedHangStream {
            type Item = Result<Chunk>;

            fn poll_next(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Self::Item>> {
                std::task::Poll::Pending
            }
        }

        impl Drop for AuditedHangStream {
            fn drop(&mut self) {
                self.state.active.fetch_sub(1, Ordering::SeqCst);
                self.state.dropped.notify_waiters();
            }
        }

        struct AuditedHangProvider {
            state: Arc<HangState>,
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for AuditedHangProvider {
            fn name(&self) -> &str {
                "mock"
            }

            async fn stream(&self, _request: Request) -> Result<ChunkStream> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    let mut chunks = intent_chunks("wait for cancellation", &[]);
                    chunks.insert(
                        chunks.len() - 1,
                        Chunk::Usage(Usage {
                            input_tokens: 7,
                            output_tokens: 3,
                            ..Usage::default()
                        }),
                    );
                    return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
                }
                self.state.active.fetch_add(1, Ordering::SeqCst);
                self.state.entered.notify_waiters();
                Ok(Box::pin(AuditedHangStream {
                    state: self.state.clone(),
                }))
            }
        }

        struct ParentSink;
        impl AgentSink for ParentSink {}

        let audit = Arc::new(EventStore::in_memory().unwrap());
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\ntools: []\n---\nWait forever.", "sloth"));
        let hang = Arc::new(HangState::default());
        let child_hang = hang.clone();
        let spawner: Arc<dyn Spawner> = Arc::new(
            LocalSpawner::new(
                Arc::new(move || {
                    Ok(Box::new(AuditedHangProvider {
                        state: child_hang.clone(),
                        calls: AtomicUsize::new(0),
                    }))
                }),
                Arc::new(roles),
                ToolRegistry::new(),
                temp_system(),
                "mock",
                1_024,
            )
            .with_audit(audit.clone()),
        );

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TaskTool));
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["task".into()], &[]),
            Arc::new(flux_runtime::AllowApprover),
            ToolContext::new(temp_system()).with_spawner(spawner),
        );
        let parent_loop = flux_flow::ast::DraftAst {
            body: vec![flux_flow::ast::Node::Return {
                value: Box::new(flux_flow::ast::Node::Call {
                    op: "task".into(),
                    args: vec![flux_flow::ast::Node::Lit {
                        value: json!({"role": "sloth", "task": "wait"}),
                    }],
                }),
            }],
            ..Default::default()
        };
        let parent_flow =
            flux_flow::state::FlowStore::in_memory_with_events(audit.clone()).unwrap();
        let engine = flux_flow::engine::FlowEngine::assemble_with_loop(
            Arc::new(flux_provider::NullProvider),
            executor,
            audit.clone(),
            parent_flow,
            "mock".into(),
            "Delegate exactly once.".into(),
            1_024,
            5,
            Vec::new(),
            0,
            Vec::new(),
            std::env::temp_dir(),
            flux_flow::engine::AgentLoopSpec::Flux(parent_loop),
        )
        .unwrap();
        let parent = audit.create_session("mock").unwrap();
        let cancel = CancellationToken::new();
        let mut sink = ParentSink;

        let turn = engine.run_turn_cancellable(&parent, "delegate", &mut sink, &cancel);
        tokio::pin!(turn);
        tokio::select! {
            () = hang.wait_until_entered() => {}
            result = &mut turn => panic!("parent ended before its child provider hung: {result:?}"),
        }
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), &mut turn)
            .await
            .expect("parent cancellation must remain bounded")
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), hang.wait_until_dropped())
            .await
            .expect("the hanging provider future must be reaped");

        let children = audit.children_of(&parent).unwrap();
        assert_eq!(children.len(), 1);
        let child = &children[0];
        let history = audit.conversation(child).unwrap();
        assert_eq!(
            history.len(),
            2,
            "child history must close user → assistant"
        );
        assert_eq!(history[0].role, flux_core::Role::User);
        assert_eq!(history[1].role, flux_core::Role::Assistant);
        assert_eq!(history[1].text(), "(turn cancelled)");
        let turns = audit.turns(child).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].outcome, "cancelled");
        let usage = turns[0]
            .usage
            .as_ref()
            .expect("usage observed before cancellation must be flushed");
        assert_eq!((usage.input_tokens, usage.output_tokens), (7, 3));
        assert_eq!(turns[0].calls, 1, "one partial provider call is audited");
        assert_eq!(
            (
                turns[0].call_usage.input_tokens,
                turns[0].call_usage.output_tokens,
            ),
            (7, 3)
        );
        assert_eq!(
            audit
                .observations(child)
                .unwrap()
                .iter()
                .filter(|observation| observation.kind == "turn.cancelled")
                .count(),
            1,
            "the child cancellation terminal must be audited exactly once"
        );
    }

    #[tokio::test]
    async fn parent_cancellation_reaps_an_audited_child_hanging_in_a_tool() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Default)]
        struct ToolHangState {
            active: AtomicUsize,
            entered: tokio::sync::Notify,
            dropped: tokio::sync::Notify,
        }

        impl ToolHangState {
            async fn wait_until_entered(&self) {
                loop {
                    let entered = self.entered.notified();
                    if self.active.load(Ordering::SeqCst) > 0 {
                        return;
                    }
                    entered.await;
                }
            }

            async fn wait_until_dropped(&self) {
                loop {
                    let dropped = self.dropped.notified();
                    if self.active.load(Ordering::SeqCst) == 0 {
                        return;
                    }
                    dropped.await;
                }
            }
        }

        struct ActiveTool(Arc<ToolHangState>);
        impl Drop for ActiveTool {
            fn drop(&mut self) {
                self.0.active.fetch_sub(1, Ordering::SeqCst);
                self.0.dropped.notify_waiters();
            }
        }

        struct HangingTool(Arc<ToolHangState>);
        #[async_trait]
        impl Tool for HangingTool {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only(
                    "hang_forever",
                    "Wait until the parent cancels this test operation.",
                    json!({"type": "object", "additionalProperties": false}),
                )
            }

            async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
                self.0.active.fetch_add(1, Ordering::SeqCst);
                let _active = ActiveTool(self.0.clone());
                self.0.entered.notify_waiters();
                futures::future::pending::<()>().await;
                unreachable!()
            }
        }

        struct HangingToolProvider;
        #[async_trait]
        impl Provider for HangingToolProvider {
            fn name(&self) -> &str {
                "mock"
            }

            async fn stream(&self, request: Request) -> Result<ChunkStream> {
                let chunks = if request_has_tool(&request, "declare_intent") {
                    intent_chunks("wait in the hanging tool", &["core"])
                } else if request_has_tool(&request, "hang_forever") {
                    native_call("hang-1", "hang_forever", json!({}))
                } else {
                    prose_chunks("unexpected child continuation")
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        struct ParentSink;
        impl AgentSink for ParentSink {}

        let audit = Arc::new(EventStore::in_memory().unwrap());
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ntools: [hang_forever]\n---\nCall the hanging tool.",
            "worker",
        ));
        let hang = Arc::new(ToolHangState::default());
        let mut child_tools = ToolRegistry::new();
        child_tools.register(Arc::new(HangingTool(hang.clone())));
        let spawner: Arc<dyn Spawner> = Arc::new(
            LocalSpawner::new(
                Arc::new(|| Ok(Box::new(HangingToolProvider))),
                Arc::new(roles),
                child_tools,
                temp_system(),
                "mock",
                1_024,
            )
            .with_audit(audit.clone()),
        );

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TaskTool));
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["task".into()], &[]),
            Arc::new(flux_runtime::AllowApprover),
            ToolContext::new(temp_system()).with_spawner(spawner),
        );
        let parent_loop = flux_flow::ast::DraftAst {
            body: vec![flux_flow::ast::Node::Return {
                value: Box::new(flux_flow::ast::Node::Call {
                    op: "task".into(),
                    args: vec![flux_flow::ast::Node::Lit {
                        value: json!({"role": "worker", "task": "hang"}),
                    }],
                }),
            }],
            ..Default::default()
        };
        let parent_flow =
            flux_flow::state::FlowStore::in_memory_with_events(audit.clone()).unwrap();
        let engine = flux_flow::engine::FlowEngine::assemble_with_loop(
            Arc::new(flux_provider::NullProvider),
            executor,
            audit.clone(),
            parent_flow,
            "mock".into(),
            "Delegate exactly once.".into(),
            1_024,
            5,
            Vec::new(),
            0,
            Vec::new(),
            std::env::temp_dir(),
            flux_flow::engine::AgentLoopSpec::Flux(parent_loop),
        )
        .unwrap();
        let parent = audit.create_session("mock").unwrap();
        let cancel = CancellationToken::new();
        let mut sink = ParentSink;

        let turn = engine.run_turn_cancellable(&parent, "delegate", &mut sink, &cancel);
        tokio::pin!(turn);
        tokio::select! {
            () = hang.wait_until_entered() => {}
            result = &mut turn => panic!("parent ended before the child tool hung: {result:?}"),
        }
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), &mut turn)
            .await
            .expect("parent cancellation must remain bounded")
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), hang.wait_until_dropped())
            .await
            .expect("the hanging tool future must be reaped");

        let children = audit.children_of(&parent).unwrap();
        assert_eq!(children.len(), 1);
        let child = &children[0];
        let history = audit.conversation(child).unwrap();
        assert_eq!(
            history.len(),
            2,
            "child history must close user → assistant"
        );
        assert_eq!(history[1].role, flux_core::Role::Assistant);
        assert_eq!(history[1].text(), "(turn cancelled)");
        let turns = audit.turns(child).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].outcome, "cancelled");
    }

    #[tokio::test]
    async fn cancellation_transitively_finalizes_an_opt_in_nested_child() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Default)]
        struct LeafHang {
            active: AtomicUsize,
            entered: tokio::sync::Notify,
            dropped: tokio::sync::Notify,
        }

        impl LeafHang {
            async fn wait_until_entered(&self) {
                loop {
                    let entered = self.entered.notified();
                    if self.active.load(Ordering::SeqCst) > 0 {
                        return;
                    }
                    entered.await;
                }
            }

            async fn wait_until_dropped(&self) {
                loop {
                    let dropped = self.dropped.notified();
                    if self.active.load(Ordering::SeqCst) == 0 {
                        return;
                    }
                    dropped.await;
                }
            }
        }

        struct ActiveLeaf(Arc<LeafHang>);
        impl Drop for ActiveLeaf {
            fn drop(&mut self) {
                self.0.active.fetch_sub(1, Ordering::SeqCst);
                self.0.dropped.notify_waiters();
            }
        }

        struct NestedCancelProvider(Arc<LeafHang>);

        #[async_trait]
        impl Provider for NestedCancelProvider {
            fn name(&self) -> &str {
                "mock"
            }

            async fn stream(&self, _request: Request) -> Result<ChunkStream> {
                self.0.active.fetch_add(1, Ordering::SeqCst);
                let _active = ActiveLeaf(self.0.clone());
                self.0.entered.notify_waiters();
                futures::future::pending::<()>().await;
                unreachable!()
            }
        }

        let audit = Arc::new(EventStore::in_memory().unwrap());
        let parent = audit.create_session("mock").unwrap();
        let mut roles = RoleRegistry::default();
        let mut delegator = parse_role(
            "---\ntools: [task]\n---\nDELEGATE to the leaf role.",
            "delegator",
        );
        delegator.agent_loop = Some(
            "flow nested_cancel -> string\n  return task({ role: \"leaf\", task: \"wait forever\" })"
                .into(),
        );
        roles.insert(delegator);
        roles.insert(parse_role("---\ntools: []\n---\nRemain blocked.", "leaf"));
        let leaf = Arc::new(LeafHang::default());
        let child_leaf = leaf.clone();
        let spawner = LocalSpawner::new(
            Arc::new(move || Ok(Box::new(NestedCancelProvider(child_leaf.clone())))),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1_024,
        )
        .with_max_depth(2)
        .with_audit(audit.clone());
        let cancel = CancellationToken::new();
        let mut request = SpawnRequest::new("delegator", "delegate");
        request.parent_session = Some(parent.clone());

        let run = spawner.spawn(request, &cancel);
        tokio::pin!(run);
        tokio::select! {
            () = leaf.wait_until_entered() => {}
            result = &mut run => panic!("nested spawn ended before its leaf hung: {result:?}"),
        }
        cancel.cancel();
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), &mut run)
            .await
            .expect("nested cancellation must remain bounded")
            .expect_err("the cancelled delegator must not report success");
        assert!(error.to_string().contains("cancelled"), "{error}");
        tokio::time::timeout(std::time::Duration::from_secs(1), leaf.wait_until_dropped())
            .await
            .expect("the nested leaf provider must be reaped");

        let children = audit.children_of(&parent).unwrap();
        assert_eq!(children.len(), 1, "one direct child is audited");
        let grandchildren = audit.children_of(&children[0]).unwrap();
        assert_eq!(grandchildren.len(), 1, "one nested child is audited");
        for child in [&children[0], &grandchildren[0]] {
            let history = audit.conversation(child).unwrap();
            assert_eq!(history.len(), 2, "{child} must close user → assistant");
            assert_eq!(history[1].role, flux_core::Role::Assistant);
            assert_eq!(history[1].text(), "(turn cancelled)");
            let turns = audit.turns(child).unwrap();
            assert_eq!(turns.len(), 1);
            assert_eq!(turns[0].outcome, "cancelled");
        }
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
                ctx.system().write_file("PINGED.marker", "1").await?;
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
        register_agent_ops(&mut registry).unwrap();
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
        // Zero-usage attempts are intentionally durable too: the daemon's provider-call circuit
        // breaker must count calls even when a provider omits token usage. Isolate the billed child
        // fact rather than asserting those honest zero-usage facts do not exist.
        let billed: Vec<_> = call_usages
            .iter()
            .filter(|(_, usage)| usage.input_tokens > 0 || usage.output_tokens > 0)
            .collect();
        assert_eq!(
            billed.len(),
            1,
            "one billed CallUsage belongs to the sub-agent: {call_usages:?}"
        );
        assert_eq!(billed[0].0, "mock");
        assert_eq!(billed[0].1.input_tokens, 1000);
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
                    .with_access(vec![AccessKind::Process])
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
                ctx.system().write_file("EXECUTED.marker", "1").await?;
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
            ctx.system().write_file("PINGED.marker", "1").await?;
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
                let outcome = match ctx
                    .system()
                    .read_file("../../../../../../etc/hostname")
                    .await
                {
                    Err(e) => format!("denied: {e}"),
                    Ok(_) => "LEAKED".to_string(),
                };
                ctx.system().write_file("PROBE.marker", &outcome).await?;
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
                ctx.system().write_file("GRANDCHILD.marker", "1").await?;
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
                let can_delegate = request_has_tool(&req, "task");
                if request_has_tool(&req, "declare_intent") {
                    let families = if is_delegator && request_has_intent_family(&req, "process") {
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
                let chunks = if is_delegator && !can_delegate {
                    // At the configured depth ceiling `task` is deliberately absent from the
                    // request. Keep the fixture provider honest and do not call an unsurfaced op.
                    prose_chunks("done")
                } else if n == 0 {
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
                    ..SpawnRequest::new("delegator", "go")
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

    /// A worktree session over `original`, transitioned to `target` (C-100 test fixture).
    fn fake_worktree_session(
        original: &Arc<System>,
        target: &std::path::Path,
    ) -> flux_runtime::WorktreeSession {
        flux_runtime::WorktreeSession {
            original: original.clone(),
            base_commit: "deadbeef".into(),
            branch: "flux/worktree/test".into(),
            checkout: target.to_path_buf(),
            parent_dir: target.to_path_buf(),
            phase: flux_runtime::WorktreePhase::Active,
        }
    }

    /// Records the root the child context actually observes, then transitions the CHILD's own
    /// workspace context into a further worktree — so the parent-side test can assert both
    /// inheritance (the recorded root) and isolation (the parent's root afterwards).
    struct RootProbe {
        seen: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
        transition_to: std::path::PathBuf,
    }
    #[async_trait]
    impl Tool for RootProbe {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("root_probe", "p", json!({"type": "object"}))
        }
        async fn execute(&self, ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
            let system = ctx.system();
            *self.seen.lock().unwrap() = Some(system.workspace().root().to_path_buf());
            // The child transitions ITS OWN context — the parent must never observe this.
            let rerooted = Arc::new(system.rerooted(&self.transition_to)?);
            ctx.workspace_context().enter_worktree(
                fake_worktree_session(&system, &self.transition_to),
                rerooted,
            )?;
            Ok(ToolResult::ok("probed"))
        }
    }

    /// A provider that selects and calls `root_probe`, then finishes with prose.
    struct RootProbePlanMock {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl Provider for RootProbePlanMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, request: Request) -> Result<ChunkStream> {
            if request_has_tool(&request, "declare_intent") {
                return Ok(Box::pin(futures::stream::iter(
                    intent_chunks("probe the workspace root", &["core"])
                        .into_iter()
                        .map(Ok),
                )));
            }
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunks = if n == 0 {
                native_call("probe-1", "root_probe", json!({}))
            } else {
                prose_chunks("done")
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// C-100: a spawned child inherits the PARENT context's transitioned root (the active-system
    /// snapshot carried on `SpawnRequest.system`), not the spawner's assembly-time system — and the
    /// child's own worktree transition never changes the parent's root (independent
    /// `WorkspaceContext` per child).
    #[tokio::test]
    async fn spawned_child_inherits_transitioned_root_but_transitions_independently() {
        let assembly = unique_system("c100-assembly");
        let parent_worktree = unique_system("c100-parent-wt");
        let child_worktree = unique_system("c100-child-wt");
        let parent_worktree_root = parent_worktree.workspace().root().to_path_buf();
        let child_worktree_root = child_worktree.workspace().root().to_path_buf();

        // The parent context enters a worktree (rerooted system, session recorded).
        let parent_ctx = ToolContext::new(assembly.clone());
        let transitioned = Arc::new(assembly.rerooted(&parent_worktree_root).unwrap());
        parent_ctx
            .workspace_context()
            .enter_worktree(
                fake_worktree_session(&assembly, &parent_worktree_root),
                transitioned,
            )
            .unwrap();

        let seen = Arc::new(std::sync::Mutex::new(None));
        let mut base = ToolRegistry::new();
        base.register(Arc::new(RootProbe {
            seen: seen.clone(),
            transition_to: child_worktree_root.clone(),
        }));
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role(
            "---\ntools: [root_probe]\n---\nYou are a scout.",
            "scout",
        ));
        // The spawner holds the ASSEMBLY-TIME system — exactly the bug surface: without the
        // request snapshot the child would probe/operate from `assembly`, not the worktree.
        let spawner = LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(RootProbePlanMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }))
            }),
            Arc::new(roles),
            base,
            assembly.clone(),
            "mock",
            1024,
        );

        // As `TaskTool` would: snapshot the parent context's ACTIVE system onto the request.
        let request = SpawnRequest {
            system: Some(parent_ctx.system()),
            ..SpawnRequest::new("scout", "probe the root")
        };
        let out = spawner
            .spawn(request, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.text, "done");

        let canon = |p: &std::path::Path| p.canonicalize().unwrap();
        // Inheritance: the child observed the parent's TRANSITIONED root, not the assembly root.
        assert_eq!(
            seen.lock().unwrap().clone().expect("probe ran"),
            canon(&parent_worktree_root),
            "child context must be seeded from the parent's active-system snapshot"
        );
        // Isolation: the child's own transition (into `child_worktree_root`) never reached the
        // parent — the parent still sits in ITS worktree, with its session intact.
        assert_eq!(
            parent_ctx.system().workspace().root(),
            canon(&parent_worktree_root),
            "a child transition must never change the parent's root"
        );
        assert_eq!(
            parent_ctx
                .workspace_context()
                .worktree_session()
                .expect("parent session intact")
                .checkout,
            parent_worktree_root
        );
        // And nothing dragged the child back through the parent's state: the child really did move.
        assert_ne!(canon(&parent_worktree_root), canon(&child_worktree_root));
    }

    /// C-100: nested delegation re-bases the depth-incremented spawner on the CHILD's snapshot —
    /// a grandchild inherits the root its parent (the child) was spawned into, never the
    /// grandparent spawner's assembly-time system.
    #[tokio::test]
    async fn nested_spawner_rebases_on_the_child_snapshot() {
        let assembly = unique_system("c100-nested-assembly");
        let worktree = unique_system("c100-nested-wt");
        let worktree_root = worktree.workspace().root().to_path_buf();
        let snapshot = Arc::new(assembly.rerooted(&worktree_root).unwrap());

        let spawner = LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(RootProbePlanMock {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }))
            }),
            Arc::new(RoleRegistry::default()),
            ToolRegistry::new(),
            assembly.clone(),
            "mock",
            1024,
        );
        let nested = spawner.at_depth(1, ToolRegistry::new(), snapshot.clone());
        assert_eq!(
            nested.system.workspace().root(),
            snapshot.workspace().root(),
            "the re-based spawner must carry the child's snapshot, not the grandparent's system"
        );
    }
}
