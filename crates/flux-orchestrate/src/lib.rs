//! `flux-orchestrate` — multi-agent orchestration: markdown agent roles, a sub-agent spawner,
//! and a `task` tool that delegates a subtask to a role and returns its result.
//!
//! A role is `.flux/agents/<name>.md` with frontmatter (`description`/`model`/`tools`) and a body
//! used as the sub-agent's system prompt. [`LocalSpawner`] runs a role as an isolated sub-agent
//! (fresh in-memory session, scoped toolset, auto-approved within its sandboxed tools) and returns
//! its final text. Plan-and-dispatch builds on this (follow-up).

mod role;
pub use role::{parse_role, Role, RoleRegistry};

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_agent::{Agent, AgentSink};
use flux_core::{Error, Result, Usage};
use flux_policy::{AuthorizationPolicy, Caller, Trust};
use flux_provider::Provider;
use flux_runtime::{
    ApprovalChoice, Approver, Executor, PermissionManager, Spawner, Tool, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_session::SessionStore;
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

/// Spawns sub-agents from roles, locally and in-process.
pub struct LocalSpawner {
    provider_factory: ProviderFactory,
    roles: Arc<RoleRegistry>,
    base_registry: ToolRegistry,
    system: Arc<System>,
    default_model: String,
    max_tokens: u32,
    max_iterations: usize,
    /// Authorization the sub-agents inherit (policy floor + caller/trust). When unset, sub-agents
    /// still run under [`SubAgentApprover`] but without the policy gate.
    auth: Option<(AuthorizationPolicy, Caller, Trust)>,
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
            max_tokens,
            max_iterations: 15,
            auth: None,
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

        let model = role
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let provider = (self.provider_factory)()?;

        // Scoped toolset; sub-agents run autonomously under the policy-bounded headless approver
        // (auto-approve scoped, policy-permitted calls; refuse destructive ones).
        // `task` is always excluded: sub-agents are leaves and must never spawn further sub-agents
        // (that causes unbounded recursion — each sub-agent calls task → more sub-agents → …).
        let mut registry = self.base_registry.subset(role.tools.as_deref());
        registry.remove("task");
        let mut executor = Executor::new(
            registry,
            PermissionManager::new(),
            Arc::new(SubAgentApprover),
            ToolContext::new(self.system.clone()),
        );
        if let Some((policy, caller, trust)) = &self.auth {
            executor = executor
                .with_policy(policy.clone())
                .with_identity(caller.clone(), trust.clone());
        }

        let store = Arc::new(SessionStore::in_memory()?);
        let session_id = store.create_session(&model)?;

        let agent = Agent {
            provider,
            executor,
            store,
            model,
            system_prompt: role.prompt.clone(),
            max_tokens: self.max_tokens,
            max_iterations: self.max_iterations,
            skills: Vec::new(),
            compact_threshold_chars: 0,
        };

        let mut sink = TextCollector::default();
        agent
            .run_turn_cancellable(&session_id, task, &mut sink, cancel)
            .await?;
        Ok(sink.text)
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
        // The `task` tool isn't separately interruptible from the parent turn (the executor doesn't
        // thread a token into tools); use a fresh token so the sub-agent runs to completion.
        let cancel = CancellationToken::new();
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
}
