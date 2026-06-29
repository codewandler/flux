//! `flux-orchestrate` — multi-agent orchestration: markdown agent roles, a sub-agent spawner,
//! and a `task` tool that delegates a subtask to a role and returns its result.
//!
//! A role is `.flux/agents/<name>.md` with frontmatter (`description`/`model`/`tools`) and a body
//! used as the sub-agent's system prompt. [`LocalSpawner`] runs a role as an isolated sub-agent
//! (fresh in-memory session, scoped toolset, auto-approved within its sandboxed tools) and returns
//! its final text. Plan-and-dispatch builds on this (follow-up).

// Agent roles + definitions live in the Agent-pillar crate (`flux-agent`); re-exported here so
// `flux_orchestrate::{Role, RoleRegistry, parse_role}` keep resolving for consumers.
pub use flux_agent::{parse_role, Role, RoleRegistry};

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_agent::register_agent_ops;
use flux_core::{Error, Result, Usage};
use flux_events::EventStore;
use flux_flow::AgentSink;
use flux_policy::{AuthorizationPolicy, Caller, Trust};
use flux_provider::Provider;
use flux_runtime::{
    ApprovalChoice, Approver, Executor, PermissionManager, Spawner, Tool, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_spec::{Idempotency, IntentSet, Risk, ToolSpec};
use flux_system::System;
use tokio_util::sync::CancellationToken;

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
}

/// Produces a fresh provider per sub-agent (sub-agents can't share one `Box<dyn Provider>`).
pub type ProviderFactory = Arc<dyn Fn() -> Result<Box<dyn Provider>> + Send + Sync>;

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
    /// Optional wall-clock deadline. On expiry the child's cancel token is **fired** (cooperative,
    /// valid-history termination) and `spawn` returns a typed timeout error — it never drops the
    /// child future mid-turn, which could otherwise leave a split tool_use/tool_result pair in a
    /// shared audit store.
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
    limits: SpawnLimits,
    /// Approver the sub-agent's tool calls dispatch through. `None` → the default [`SubAgentApprover`]
    /// (auto-approve non-destructive, deny destructive). A multi-tenant consumer injects an approver
    /// that approval-gates its mutations.
    approver: Option<Arc<dyn Approver>>,
    /// Authorization the sub-agents inherit (policy floor + caller/trust). When unset, sub-agents
    /// still run under the headless approver but without the policy gate.
    auth: Option<(AuthorizationPolicy, Caller, Trust)>,
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
            limits: SpawnLimits::new(max_tokens),
            approver: None,
            auth: None,
            audit: None,
            depth: 0,
            max_depth: 1,
        }
    }

    /// Bound spawned sub-agents by an authorization policy + resolved identity (inherited from the
    /// parent). Sub-agents then traverse the same policy floor as the top-level agent.
    pub fn with_authorization(
        mut self,
        policy: AuthorizationPolicy,
        caller: Caller,
        trust: Trust,
    ) -> Self {
        self.auth = Some((policy, caller, trust));
        self
    }

    /// Override the per-sub-agent resource limits (iteration cap, token budget, wall-clock deadline).
    pub fn with_limits(mut self, limits: SpawnLimits) -> Self {
        self.limits = limits;
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

    /// Allow bounded nested delegation: a sub-agent at `depth < max_depth` keeps the `task` tool and a
    /// depth-incremented spawner. Default `1` (children are leaves). `> 1` is an opt-in escape hatch —
    /// the recursion bound is `max_depth`, never unbounded.
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth.max(1);
        self
    }

    /// A clone of this spawner at a deeper delegation level (shares all Arc-held state).
    fn at_depth(&self, depth: usize) -> LocalSpawner {
        LocalSpawner {
            provider_factory: self.provider_factory.clone(),
            roles: self.roles.clone(),
            base_registry: self.base_registry.clone(),
            system: self.system.clone(),
            default_model: self.default_model.clone(),
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
    async fn spawn(
        &self,
        role_name: &str,
        task: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<String> {
        let role = self
            .roles
            .get(role_name)
            .ok_or_else(|| Error::Other(format!("unknown role: {role_name}")))?;

        let provider = (self.provider_factory)()?;

        // Scoped toolset; sub-agents run autonomously under the policy-bounded headless approver
        // (auto-approve scoped, policy-permitted calls; refuse destructive ones — unless an approver
        // is injected). `register_agent_ops` adds the reflexive ops (`plan`/`run_plan`/…) the flux-lang
        // agent loop calls — sub-agents run the same audited loop as the top-level agent.
        let mut registry = self.base_registry.subset(role.tools.as_deref());
        register_agent_ops(&mut registry);

        // Recursion bound: a child at the leaf depth must never spawn further sub-agents, so `task` is
        // stripped from its registry AND no spawner is installed in its context (the two guards that
        // make a sub-agent a leaf). Below the bound, the child keeps `task` and a depth-incremented
        // spawner. With the default `max_depth = 1`, every child is a leaf — today's behaviour exactly.
        let child_depth = self.depth + 1;
        let child_can_delegate = child_depth < self.max_depth;
        let mut ctx = ToolContext::new(self.system.clone());
        if child_can_delegate {
            // Bounded nested delegation: the child keeps both halves of the delegation capability —
            // the `task` tool in its registry AND a depth-incremented spawner in its context.
            registry.register(Arc::new(TaskTool));
            ctx = ctx.with_spawner(Arc::new(self.at_depth(child_depth)));
        } else {
            // Leaf: never spawn further sub-agents. Both guards apply — `task` is stripped from the
            // registry and no spawner is installed in the context.
            registry.remove("task");
        }
        let approver: Arc<dyn Approver> = self
            .approver
            .clone()
            .unwrap_or_else(|| Arc::new(SubAgentApprover));
        let mut executor = Executor::new(registry, PermissionManager::new(), approver, ctx);
        if let Some((policy, caller, trust)) = &self.auth {
            executor = executor
                .with_policy(policy.clone())
                .with_identity(caller.clone(), trust.clone());
        }

        // The role *is* the agent definition: body → system prompt, `tools` already applied to the
        // scoped registry above, model inherits the spawner default when the role doesn't override it.
        let mut spec = role.to_spec(&self.default_model);
        spec.max_tokens = self.limits.max_tokens;
        spec.max_iterations = self.limits.max_iterations;

        // Child runs persist into the shared (tenant) store when auditing; otherwise a throwaway
        // in-memory store keeps the sub-agent ephemeral (the historical default).
        let events = match &self.audit {
            Some(store) => store.clone(),
            None => Arc::new(EventStore::in_memory()?),
        };
        let session_id = events.create_session(&spec.model)?;
        // Share the event store with the flow store so the child's run trace (its inner tool calls)
        // lands in the same log as its conversation — into the shared audit store when one is set.
        let flow = flux_flow::state::FlowStore::in_memory_with_events(events.clone())?;
        let engine = spec.into_engine(Arc::from(provider), executor, events, flow)?;

        // The child runs under a child of the parent's cancel token: cancelling the parent turn
        // cancels the child. A wall-clock deadline fires that same token (cooperative termination) and
        // returns a typed error, rather than dropping the future mid-turn.
        let run_cancel = cancel.child_token();
        let mut sink = TextCollector::default();
        match self.limits.wall_clock {
            Some(dur) => {
                let run = engine.run_turn_cancellable(&session_id, task, &mut sink, &run_cancel);
                tokio::pin!(run);
                tokio::select! {
                    res = &mut run => { res?; }
                    _ = tokio::time::sleep(dur) => {
                        run_cancel.cancel();
                        // Let the child observe the cancel and finalize a valid session shape.
                        let _ = run.await;
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
        Ok(sink.text)
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
    pub limits: SpawnLimits,
    pub approver: Option<Arc<dyn Approver>>,
    pub auth: Option<(AuthorizationPolicy, Caller, Trust)>,
    pub audit: Option<Arc<EventStore>>,
}

impl SubAgents {
    /// A bundle with default limits for `max_tokens`; everything else off (no approver override, no
    /// inherited authorization, no audit store). Set those with the `with_*` methods.
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
            limits: SpawnLimits::new(max_tokens),
            approver: None,
            auth: None,
            audit: None,
        }
    }

    /// Inherit an authorization policy + resolved identity (the parent's floor) for every sub-agent.
    pub fn with_authorization(
        mut self,
        policy: AuthorizationPolicy,
        caller: Caller,
        trust: Trust,
    ) -> Self {
        self.auth = Some((policy, caller, trust));
        self
    }

    /// Override the per-sub-agent resource limits.
    pub fn with_limits(mut self, limits: SpawnLimits) -> Self {
        self.limits = limits;
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
        .with_limits(limits);
        if let Some(approver) = self.approver {
            spawner = spawner.with_approver(approver);
        }
        if let Some((policy, caller, trust)) = self.auth {
            spawner = spawner.with_authorization(policy, caller, trust);
        }
        if let Some(store) = self.audit {
            spawner = spawner.with_audit(store);
        }
        Arc::new(spawner)
    }
}

#[derive(Default)]
struct TextCollector {
    text: String,
}
impl AgentSink for TextCollector {
    fn text_delta(&mut self, t: &str) {
        self.text.push_str(t);
    }
    fn turn_end(&mut self, _u: Option<Usage>) {}
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
            "planner",
            &format!("Goal: {goal}\n\nProduce a concise, ordered plan."),
            cancel,
        )
        .await?;
    if cancel.is_cancelled() {
        return Ok(format!("── plan ──\n{plan}\n\n(interrupted)"));
    }
    let result = spawner
        .spawn(
            "worker",
            &format!("Goal: {goal}\n\nPlan:\n{plan}\n\nExecute the plan and report what you did."),
            cancel,
        )
        .await?;
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
            "planner",
            &format!(
                "Goal: {goal}\n\nBreak this into subtasks. Respond with ONLY a JSON array of \
                 objects with fields `id` (string), `task` (string), and `depends_on` (array of \
                 ids). No prose, no code fences."
            ),
            cancel,
        )
        .await?;
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
            async move { (id, spawner.spawn("worker", &prompt, cancel).await) }
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
            input_schema: json!({
                "type": "object",
                "properties": {
                    "role": {"type": "string", "description": "Sub-agent role name"},
                    "task": {"type": "string", "description": "What the sub-agent should do"}
                },
                "required": ["role", "task"]
            }),
            output_schema: None,
            effects: Vec::new(),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: Vec::new(),
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("role")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let role = params
            .get("role")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("task: `role` required".into()))?;
        let task = params
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("task: `task` required".into()))?;
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
        match spawner.spawn(role, task, &cancel).await {
            Ok(text) => Ok(ToolResult::ok(text)),
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

    /// Mock provider: returns a fixed text reply (one canned turn).
    struct MockProvider;
    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let chunks = vec![
                Chunk::TextDelta("scouted: 3 files".into()),
                Chunk::Block(ContentBlock::Text {
                    text: "scouted: 3 files".into(),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ];
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    fn temp_system() -> Arc<System> {
        let dir = std::env::temp_dir().join(format!("flux-orch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(System::new(Workspace::new(&dir).unwrap()))
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
            .spawn("scout", "look around", &cancel)
            .await
            .unwrap();
        assert_eq!(out, "scouted: 3 files");
        assert!(spawner.spawn("nope", "x", &cancel).await.is_err());
    }

    #[tokio::test]
    async fn restricted_sub_agent_runs_a_plan_with_loop_machinery() {
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

        // Plan (call 0) — a one-op plan, no `complete` — then prose (call 1). Running the plan drives
        // the loop's `observe`, so this fails unless `register_agent_ops` re-added the evidence ops
        // after the role's `tools: [ping]` subset dropped them.
        struct PlanMock {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for PlanMock {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, _r: Request) -> Result<ChunkStream> {
                let n = self.calls.fetch_add(1, Ordering::Relaxed);
                let chunks = if n == 0 {
                    let ast = json!({ "body": [{ "kind": "call", "op": "ping", "args": [] }] });
                    vec![
                        Chunk::Block(ContentBlock::ToolUse {
                            id: "p".into(),
                            name: "emit_plan".into(),
                            input: json!({ "ast": ast }),
                        }),
                        Chunk::Done {
                            stop_reason: Some(StopReason::ToolUse),
                        },
                    ]
                } else {
                    vec![
                        Chunk::Block(ContentBlock::Text {
                            text: "done scouting".into(),
                        }),
                        Chunk::Done {
                            stop_reason: Some(StopReason::EndTurn),
                        },
                    ]
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
            .spawn("scout", "scout the repo", &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out, "done scouting");
        assert!(
            system.read_file("PINGED.marker").await.is_ok(),
            "the plan's op executed through the loop"
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

    #[tokio::test]
    async fn sub_agent_refuses_destructive_command() {
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

        // Mock provider: turn 1 calls `danger`, turn 2 finishes with text.
        struct DestructiveMock {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for DestructiveMock {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, _r: Request) -> Result<ChunkStream> {
                let n = self.calls.fetch_add(1, Ordering::Relaxed);
                let chunks = if n == 0 {
                    vec![
                        Chunk::Block(ContentBlock::ToolUse {
                            id: "b".into(),
                            name: "danger".into(),
                            input: json!({}),
                        }),
                        Chunk::Done {
                            stop_reason: Some(StopReason::ToolUse),
                        },
                    ]
                } else {
                    vec![
                        Chunk::TextDelta("done".into()),
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
            .spawn("worker", "delete things", &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out, "done");
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
                role: &str,
                task: &str,
                _cancel: &CancellationToken,
            ) -> Result<String> {
                match role {
                    "planner" => Ok(r#"[
                        {"id":"a","task":"first","depends_on":[]},
                        {"id":"b","task":"second","depends_on":["a"]}
                    ]"#
                    .into()),
                    "worker" => {
                        // report whether the dependency's result reached us
                        let saw_dep = task.contains("[a]");
                        Ok(format!("worker(saw_dep={saw_dep})"))
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
                role: &str,
                _task: &str,
                _c: &CancellationToken,
            ) -> Result<String> {
                match role {
                    "planner" => Ok(r#"[
                        {"id":"a","task":"x","depends_on":[]},
                        {"id":"b","task":"y","depends_on":["a"]}
                    ]"#
                    .into()),
                    "worker" => {
                        self.workers.fetch_add(1, Ordering::SeqCst);
                        self.cancel.cancel();
                        Ok("did work".into())
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
                role: &str,
                task: &str,
                _c: &CancellationToken,
            ) -> Result<String> {
                match role {
                    "planner" => Ok(r#"[
                        {"id":"a","task":"ok-one","depends_on":[]},
                        {"id":"b","task":"will-fail","depends_on":[]}
                    ]"#
                    .into()),
                    "worker" if task.contains("will-fail") => Err(Error::Other("boom".into())),
                    "worker" => Ok("ok-one done".into()),
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
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            spawner.spawn("sloth", "spin forever", &CancellationToken::new()),
        )
        .await
        .expect("spawn should return by its wall-clock deadline, not hang");
        let err = out.expect_err("a hung sub-agent past its deadline must error");
        assert!(
            err.to_string().contains("wall-clock"),
            "expected a wall-clock timeout error, got: {err}"
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

    /// A provider that plans a single `ping` call (turn 0) then finishes with prose (turn 1).
    struct PingPlanMock {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait]
    impl Provider for PingPlanMock {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _r: Request) -> Result<ChunkStream> {
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let chunks = if n == 0 {
                vec![
                    Chunk::Block(ContentBlock::ToolUse {
                        id: "p".into(),
                        name: "emit_plan".into(),
                        input: json!({ "ast": { "body": [{ "kind": "call", "op": "ping", "args": [] }] } }),
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
            .spawn("scout", "scout the repo", &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out, "done");
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
                let escaped = ctx.system.read_file("../../../../../../etc/hostname").await;
                ctx.system
                    .write_file(
                        "PROBE.marker",
                        if escaped.is_err() { "denied" } else { "LEAKED" },
                    )
                    .await?;
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
            async fn stream(&self, _r: Request) -> Result<ChunkStream> {
                let n = self
                    .calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let chunks = if n == 0 {
                    vec![
                        Chunk::Block(ContentBlock::ToolUse {
                            id: "p".into(),
                            name: "emit_plan".into(),
                            input: json!({ "ast": { "body": [{ "kind": "call", "op": "escape_probe", "args": [] }] } }),
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
            .spawn("prober", "try to escape", &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            system.read_file("PROBE.marker").await.unwrap(),
            "denied",
            "the sub-agent must not read outside the parent's workspace"
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
            .spawn("scout", "scout the repo", &CancellationToken::new())
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

    /// WS4: without an audit store, a shared store handed elsewhere stays untouched (no regression for
    /// the CLI / self-improvement loop, which keep ephemeral in-memory child stores).
    #[tokio::test]
    async fn without_audit_the_shared_store_is_untouched() {
        let store = Arc::new(EventStore::in_memory().unwrap());
        let mut roles = RoleRegistry::default();
        roles.insert(parse_role("---\n---\nscout prompt", "scout"));
        let spawner = LocalSpawner::new(
            Arc::new(|| Ok(Box::new(MockProvider))),
            Arc::new(roles),
            ToolRegistry::new(),
            temp_system(),
            "mock",
            1024,
        );
        spawner
            .spawn("scout", "recon", &CancellationToken::new())
            .await
            .unwrap();
        assert!(
            store.latest_session().unwrap().is_none(),
            "no audit store configured → the shared store must be untouched"
        );
    }

    /// WS5: roles register in memory (no shared `.flux/agents` directory) and spawn.
    #[tokio::test]
    async fn in_memory_roles_spawn() {
        let roles = RoleRegistry::from_roles([Role {
            name: "scout".into(),
            description: "recon".into(),
            model: None,
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
            .spawn("scout", "look around", &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out, "scouted: 3 files");
    }

    /// WS5: `max_depth` bounds nested delegation. With the default (1) a child is a leaf and cannot
    /// delegate; with `max_depth = 2` it can — the grandchild runs and leaves its marker.
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

        /// Role-discriminating mock: a "DELEGATE" role plans `task("inner", …)`; a leaf "inner" role
        /// plans `ping`. Per-instance call counter (a fresh provider is built per sub-agent).
        struct DepthMock {
            calls: std::sync::atomic::AtomicUsize,
        }
        #[async_trait]
        impl Provider for DepthMock {
            fn name(&self) -> &str {
                "mock"
            }
            async fn stream(&self, req: Request) -> Result<ChunkStream> {
                let n = self
                    .calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let is_delegator = req.system.unwrap_or_default().contains("DELEGATE");
                let chunks = if n == 0 {
                    let ast = if is_delegator {
                        json!({ "body": [{ "kind": "call", "op": "task", "args": [
                            { "kind": "lit", "value": "inner" },
                            { "kind": "lit", "value": "do the thing" }
                        ] }] })
                    } else {
                        json!({ "body": [{ "kind": "call", "op": "ping", "args": [] }] })
                    };
                    vec![
                        Chunk::Block(ContentBlock::ToolUse {
                            id: "p".into(),
                            name: "emit_plan".into(),
                            input: json!({ "ast": ast }),
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

        fn build(system: Arc<System>, max_depth: usize) -> LocalSpawner {
            let mut base = ToolRegistry::new();
            base.register(Arc::new(GrandPing));
            let mut roles = RoleRegistry::default();
            roles.insert(parse_role(
                "---\ntools: [ping]\n---\nYou DELEGATE to a sub-agent.",
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
            .spawn("delegator", "go", &CancellationToken::new())
            .await
            .unwrap();
        assert!(
            sys1.read_file("GRANDCHILD.marker").await.is_err(),
            "default max_depth=1 must keep children leaves (no nested delegation)"
        );

        // max_depth=2: the delegator may spawn the inner leaf, which runs `ping` and leaves its marker.
        let sys2 = unique_system("depth-nested");
        build(sys2.clone(), 2)
            .spawn("delegator", "go", &CancellationToken::new())
            .await
            .unwrap();
        assert!(
            sys2.read_file("GRANDCHILD.marker").await.is_ok(),
            "max_depth=2 must allow one level of nested delegation"
        );
    }
}
