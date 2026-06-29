//! `flux-runtime` — the mandatory safety envelope around tool execution.
//!
//! Every tool call goes through [`Executor::dispatch`]: permission-rule check → (if unmatched)
//! approval prompt → execute through the guarded [`System`](flux_system::System). There is no
//! path to IO that skips this. Tools declare their permission *subjects* and pre-execution
//! *intents*; the dispatcher gates on them and redacts secrets from any error surfaced.

mod perm;
pub use perm::{Pattern, PermDecision, PermissionManager};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::Result;
use flux_evidence::{
    DestructiveEscalation, EvidenceLog, Observation, Phase, Reaction, KIND_DESTRUCTIVE,
};
use flux_policy::{
    evaluate, Action, AuthorizationPolicy, Caller, CallerKind, Decision, Principal,
    Request as PolicyRequest, ResourceKind, ResourceRef, Trust, TrustKind, TrustLevel,
};
use flux_secret::Redactor;
use flux_spec::{Effect, IntentSet, Risk, ToolSpec};
use flux_system::System;

/// The result of executing a tool.
///
/// A result has **two faces**. `content` is the *canonical* value: it is what gets bound to a session
/// symbol, spliced into `{{symbol}}` interpolations, and used for `when`/`return` truthiness — i.e.
/// what deterministic execution works with. `view` is an optional *LLM-facing* rendering shown to the
/// model (and the user) — e.g. a line-numbered file, or a status line with a unified diff appended.
/// When `view` is `None` the model sees `content`. Keeping them separate lets a `read` return raw
/// bytes (clean to interpolate) while showing the model a numbered view, and lets `edit`/`write`
/// attach a diff without polluting the canonical value.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub view: Option<String>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            view: None,
            is_error: false,
        }
    }

    /// An OK result whose model-facing `view` differs from the canonical `content`.
    pub fn ok_view(content: impl Into<String>, view: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            view: Some(view.into()),
            is_error: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            view: None,
            is_error: true,
        }
    }

    /// Attach (or replace) the model-facing view.
    pub fn with_view(mut self, view: impl Into<String>) -> Self {
        self.view = Some(view.into());
        self
    }

    /// The model-facing rendering: the explicit `view` if set, else the canonical `content`.
    pub fn view(&self) -> &str {
        self.view.as_deref().unwrap_or(&self.content)
    }
}

/// Runs a sub-agent (by role name) and returns its final text. Implemented by `flux-orchestrate`
/// and injected into [`ToolContext`] so a `task` tool can delegate without `flux-runtime`
/// depending on the agent loop. The `cancel` token aborts the sub-agent turn (so autopilot loops
/// and plan-and-dispatch stay interruptible).
#[async_trait]
pub trait Spawner: Send + Sync {
    async fn spawn(
        &self,
        role: &str,
        task: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> flux_core::Result<String>;
}

/// The reflexive capability: re-enter the planner and the interpreter from *within* a flow. Defined
/// here (L2) and injected into [`ToolContext`] so the `plan`/`run_plan` ops can delegate without
/// `flux-runtime` depending on the engine — the same seam as [`Spawner`]. The engine (L3) installs a
/// concrete `LoopHost` per turn, wired to the live provider + session + sink. This is what lets the
/// agent loop be written in flux-lang: "ask the planner" and "run a plan" become ordinary gated ops
/// that traverse [`Executor::dispatch`] like any other — the LLM stays the planner, never the runtime.
#[async_trait]
pub trait LoopHost: Send + Sync {
    /// Re-enter the planner (the model) to produce a plan from `input` (the working feedback /
    /// conversation) → a `Plan` artifact (`{kind: "chat"|"plan"|"error", text?, ast?, complete?}`) as
    /// JSON. Wraps the engine's compile step.
    async fn plan(&self, input: serde_json::Value) -> flux_core::Result<serde_json::Value>;
    /// Re-enter the interpreter to run an emitted plan in the CURRENT session → an `Outcome` artifact
    /// (`{transcript, result, steps, suspension?}`) as JSON. Bounded by a reentry-depth cap. Wraps the
    /// engine's execute step.
    async fn run_plan(&self, plan: serde_json::Value) -> flux_core::Result<serde_json::Value>;
}

/// What a tool is given at execution time: the guarded IO surface, the secret redactor, an optional
/// sub-agent spawner, and the per-session read-set (file → mtime at last read) used by the
/// read-before-write guard. The read-set is shared (an `Arc<Mutex<…>>`) so every op in a session sees
/// the same map: a `read` in one node records an mtime an `edit` in a later node checks against.
#[derive(Clone)]
pub struct ToolContext {
    pub system: Arc<System>,
    pub redactor: Redactor,
    pub spawner: Option<Arc<dyn Spawner>>,
    /// The reflexive capability (`plan`/`run_plan`), installed per turn by the engine. `None` outside a
    /// model-in-the-loop run — the ops then return a clear error rather than silently doing nothing.
    pub loop_host: Option<Arc<dyn LoopHost>>,
    pub read_times: Arc<Mutex<HashMap<String, std::time::SystemTime>>>,
    /// The append-only evidence log, shared (an `Arc<Mutex<…>>`) so the dispatcher's `tool_call`
    /// markers, externally-recorded observations ([`Executor::observe`]), flow-emitted `observe(…)`
    /// ops, and any sibling run that re-enters this same context all write to **one** audit trail.
    /// Lives here (not Executor-private) so the `observe`/`evidence` ops can read and append to it.
    pub evidence: Arc<Mutex<EvidenceLog>>,
}

impl ToolContext {
    pub fn new(system: Arc<System>) -> Self {
        Self {
            system,
            redactor: Redactor::new(),
            spawner: None,
            loop_host: None,
            read_times: Arc::new(Mutex::new(HashMap::new())),
            evidence: Arc::new(Mutex::new(EvidenceLog::new())),
        }
    }

    /// Record that `path` was read at `mtime` (called by `read`/`read_many`).
    pub fn record_read(&self, path: &str, mtime: std::time::SystemTime) {
        self.read_times
            .lock()
            .unwrap()
            .insert(path.to_string(), mtime);
    }

    /// The mtime `path` had when it was last read this session, if ever.
    pub fn read_mtime(&self, path: &str) -> Option<std::time::SystemTime> {
        self.read_times.lock().unwrap().get(path).copied()
    }

    pub fn with_spawner(mut self, spawner: Arc<dyn Spawner>) -> Self {
        self.spawner = Some(spawner);
        self
    }

    /// Install the reflexive capability (the engine does this per turn before running the loop).
    pub fn with_loop_host(mut self, loop_host: Arc<dyn LoopHost>) -> Self {
        self.loop_host = Some(loop_host);
        self
    }

    /// Set the secret redactor (seeded with known secret values; see [`SecretResolver`]).
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.redactor = redactor;
        self
    }
}

/// Resolves secret references to their materialized values and seeds a [`Redactor`]. Only the
/// `env/KEY` scheme is resolved at runtime today; `plugin`/`kubernetes` refs are resolved by their
/// providers later. Resolution is the only place env secrets are read for redaction.
#[derive(Default, Clone)]
pub struct SecretResolver;

impl SecretResolver {
    pub fn new() -> Self {
        Self
    }

    /// Resolve a single reference to its [`Material`](flux_secret::Material), if available.
    pub fn resolve(&self, r: &flux_secret::Ref) -> Option<flux_secret::Material> {
        match r.scheme {
            flux_secret::Scheme::Env => {
                std::env::var(&r.slot)
                    .ok()
                    .map(|value| flux_secret::Material {
                        reference: r.clone(),
                        kind: flux_secret::Kind::ApiKey,
                        value,
                        media_type: None,
                    })
            }
            _ => None,
        }
    }

    /// Register the values of every resolvable ref in `refs` with `redactor`, so they are scrubbed
    /// from tool output and logs.
    pub fn seed_redactor(&self, redactor: &mut Redactor, refs: &[flux_secret::Ref]) {
        for r in refs {
            if let Some(m) = self.resolve(r) {
                redactor.add_secret(m.value);
            }
        }
    }
}

/// A tool the agent can invoke. Permission metadata and intents are declared here so the
/// dispatcher can gate, render, and audit the call.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    /// Permission subjects for this invocation (e.g. `["src/main.rs"]` for read, `["git:status"]`
    /// for bash). Empty means the tool is gated only by its bare name.
    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        Vec::new()
    }

    /// Pre-execution intents (the approval-risk signal).
    fn intents(&self, _params: &Value) -> IntentSet {
        IntentSet::new()
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult>;
}

/// A registry of tools keyed by name.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Remove a tool by name, returning it if present. Used to scope a sub-agent's registry (e.g.
    /// drop `task` so a sub-agent can't spawn further sub-agents).
    pub fn remove(&mut self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.remove(name)
    }

    /// Specs for every registered tool (e.g. to advertise to the model).
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    /// A registry scoped to a sub-agent's allowed tools. `None` (the role declared no `tools` key)
    /// inherits all parent tools; `Some(names)` keeps only those — so `Some(&[])`, an *explicitly
    /// empty* allowlist, yields an empty registry. (Previously an empty slice meant "all", which
    /// silently turned the most-restrictive declaration into the least-restrictive outcome.)
    pub fn subset(&self, names: Option<&[String]>) -> ToolRegistry {
        let Some(names) = names else {
            return self.clone();
        };
        let tools = self
            .tools
            .iter()
            .filter(|(k, _)| names.iter().any(|n| n == *k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        ToolRegistry { tools }
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Specs for the ops that should be **advertised to the model** given the group manifest and the
    /// active group set: core ops (in no group) always; a grouped op only when its group is active.
    /// See [`is_advertised`]. An empty manifest with no group-tagged specs advertises everything.
    pub fn active_specs(
        &self,
        groups: &[flux_evidence::ToolGroup],
        active: &HashSet<String>,
    ) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|t| t.spec())
            .filter(|s| is_advertised(s, groups, active))
            .collect()
    }
}

/// `FLUX_SURFACE_ALL=1` (or `true`) disables evidence gating — every op is advertised, as before
/// surfacing existed. An escape hatch for debugging and parity.
pub fn surface_all_override() -> bool {
    std::env::var("FLUX_SURFACE_ALL").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Whether the generic `bash` op is opted in: `FLUX_ENABLE_BASH=1` (or `true`). The CLI sets this
/// from config `enable_shell` and the `/shell` toggle; [`detect_signals`] turns it into the `shell`
/// signal that surfaces the off-by-default `shell` group.
pub fn shell_opt_in() -> bool {
    std::env::var("FLUX_ENABLE_BASH").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// The group tag for the reflexive loop-machinery ops (`plan`/`run_plan`). It is never surfaced by a
/// workspace signal, so these ops stay out of the model-facing catalog while remaining dispatchable
/// by the agent loop. Shared so the tag and the catalog filters can't drift.
pub const REFLECT_GROUP: &str = "reflect";

/// The group an op effectively belongs to: a manifest group that lists it in `tools` wins (so config
/// can (re)assign membership), otherwise the op's own [`ToolSpec::group`] tag. `None` ⇒ *core*.
fn effective_group<'a>(
    spec: &'a ToolSpec,
    groups: &'a [flux_evidence::ToolGroup],
) -> Option<&'a str> {
    groups
        .iter()
        .find(|g| g.tools.iter().any(|t| t == &spec.name))
        .map(|g| g.name.as_str())
        .or(spec.group.as_deref())
}

/// Whether `spec` should be advertised to the model: core ops (no effective group) always; a grouped
/// op only when its group is in `active`. `FLUX_SURFACE_ALL` forces everything on. Membership comes
/// from the manifest's `tools` or the op's own [`ToolSpec::group`] tag (see [`effective_group`]).
pub fn is_advertised(
    spec: &ToolSpec,
    groups: &[flux_evidence::ToolGroup],
    active: &HashSet<String>,
) -> bool {
    surface_all_override()
        || match effective_group(spec, groups) {
            None => true,
            Some(g) => active.contains(g),
        }
}

/// The set of op names to advertise to the model — [`is_advertised`] applied across `specs`. Handy
/// for filtering a name-keyed catalog (e.g. the Flux-Lang op catalog in `flux-flow`).
pub fn advertised_op_names(
    specs: &[ToolSpec],
    groups: &[flux_evidence::ToolGroup],
    active: &HashSet<String>,
) -> HashSet<String> {
    specs
        .iter()
        .filter(|s| is_advertised(s, groups, active))
        .map(|s| s.name.clone())
        .collect()
}

/// Probe `cwd` (walking up to the nearest marker) for the workspace signals currently true, as
/// `project.signal` [`Observation`]s. Cheap enough to run every turn — a handful of `exists()`
/// checks. The emitted `signal` strings are the contract that group `surface_when` matches against
/// (see `flux-tools`' `builtin_groups`).
pub fn detect_signals(cwd: &std::path::Path) -> Vec<Observation> {
    let mut out = Vec::new();
    let mut push = |sig: &str| {
        out.push(Observation::new(
            flux_evidence::KIND_SIGNAL,
            Phase::Turn,
            json!({ "signal": sig }),
        ));
    };
    if find_up(cwd, |p| p.join(".git").exists()) {
        push("git_repo");
    }
    if find_up(cwd, |p| p.join("go.mod").exists()) {
        push("go");
    }
    if find_up(cwd, |p| p.join("Cargo.toml").exists()) {
        push("rust");
    }
    if find_up(cwd, |p| p.join("package.json").exists()) {
        push("node");
    }
    if find_up(cwd, |p| {
        p.join("pyproject.toml").exists() || p.join("requirements.txt").exists()
    }) {
        push("python");
    }
    if find_up(cwd, |p| {
        p.join("Makefile").exists() || p.join("makefile").exists()
    }) {
        push("make");
    }
    if find_up(cwd, |p| p.join(".flux").join("evals").is_dir()) {
        push("eval");
    }
    // `shell` is an explicit opt-in, not a filesystem marker: it surfaces the off-by-default `shell`
    // group (the generic `bash` op). The CLI sets `FLUX_ENABLE_BASH` from config `enable_shell`, the
    // `/shell` toggle, or the user exports it directly.
    if shell_opt_in() {
        push("shell");
    }
    out
}

/// Walk up from `start` to the filesystem root, returning true at the first ancestor satisfying
/// `pred` — so a marker in any parent (e.g. running from a repo subdirectory or a git worktree,
/// where `.git` is a *file*) is still found, matching how the rest of the system detects a repo.
fn find_up(start: &std::path::Path, pred: impl Fn(&std::path::Path) -> bool) -> bool {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if pred(d) {
            return true;
        }
        dir = d.parent();
    }
    false
}

/// Cap an oversized tool result for the model transcript: within `cap` chars it is returned
/// unchanged; otherwise it is truncated to `cap` and a one-line notice is appended recording how much
/// was dropped and pointing the model at a follow-up read for the exact bytes. Keeps a single huge
/// `bash`/`read`/`grep` result from blowing the context budget. `cap == 0` disables trimming.
pub fn trim_tool_output(content: String, cap: usize, label: &str) -> String {
    if cap == 0 {
        return content;
    }
    let total = content.chars().count();
    if total <= cap {
        return content;
    }
    let kept: String = content.chars().take(cap).collect();
    let omitted = total - cap;
    format!(
        "{kept}\n…[{label} output truncated: {omitted} of {total} chars omitted — narrow the range \
         or do a follow-up read for the full output]"
    )
}

/// The per-result transcript cap (chars) for [`trim_tool_output`], from `FLUX_TOOL_OUTPUT_CAP`
/// (default 20000). `0` disables per-result trimming. Mirrors the session-compaction knob but acts on
/// a single tool/op result so one huge output can't blow the budget before compaction runs.
pub fn tool_output_cap() -> usize {
    std::env::var("FLUX_TOOL_OUTPUT_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000)
}

/// The user's response to an approval request.
#[derive(Debug, Clone)]
pub enum ApprovalChoice {
    Allow,
    /// Allow and remember this rule (added to the allow list).
    AllowAlways(String),
    Deny,
}

/// How the runtime asks for human approval when a call isn't covered by a rule.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn request(&self, tool: &str, subjects: &[String], intents: &IntentSet)
        -> ApprovalChoice;

    /// Approve a whole compiled plan as one unit (the "approve the graph, not each node" path). The
    /// plan itself has already been rendered for the user (the `flow.plan` observation); this is just
    /// the single confirm. `AllowAlways` here means "trust every plan for the rest of the session".
    /// The default delegates to [`request`](Self::request) so existing approvers keep working.
    async fn request_plan(&self, summary: &str, ops: usize) -> ApprovalChoice {
        let subject = format!("{ops} op(s) · {summary}");
        self.request("run plan", &[subject], &IntentSet::default())
            .await
    }
}

/// A headless approver that denies anything not pre-allowed by rules.
pub struct DenyApprover;

#[async_trait]
impl Approver for DenyApprover {
    async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
        ApprovalChoice::Deny
    }
}

/// A headless approver that allows everything (e.g. `flux run --yes`). Use with care.
pub struct AllowApprover;

#[async_trait]
impl Approver for AllowApprover {
    async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
        ApprovalChoice::Allow
    }
}

/// The outcome of a pre-tool hook.
pub enum HookOutcome {
    /// Proceed unchanged.
    Continue,
    /// Replace the tool input with this value, then proceed.
    Modify(serde_json::Value),
    /// Block the call with this reason.
    Deny(String),
}

/// A hook run before a tool executes — may observe, modify the input, or deny the call. Engine-
/// agnostic so `flux-runtime` doesn't depend on a JS runtime; `flux-hooks` provides a JS impl.
pub trait PreToolHook: Send + Sync {
    fn pre_tool(&self, tool: &str, input: &serde_json::Value) -> HookOutcome;
}

/// A local single-user caller used when no identity is supplied (matches `flux-auth`'s
/// `LocalIdentity`, duplicated here so the runtime needn't depend on the auth layer).
fn default_local_caller() -> Caller {
    Caller {
        principal: Principal {
            id: "local".into(),
            name: "local".into(),
            kind: CallerKind::User,
        },
        groups: Vec::new(),
        source: "local".into(),
    }
}

fn default_local_trust() -> Trust {
    Trust {
        kind: TrustKind::Invocation,
        level: TrustLevel::Privileged,
        scopes: Vec::new(),
    }
}

/// Translate a tool's declared effects + permission subjects into the (action, resource) pairs the
/// authorization policy is evaluated against. Filesystem read/write map onto path resources (one
/// per subject); process/network/browser map onto a kind-wide resource (their subjects are gated
/// by the coder-style permission rules, not the policy).
fn effect_requests(spec: &ToolSpec, subjects: &[String]) -> Vec<(Action, ResourceRef)> {
    let mut reqs = Vec::new();
    let has = |e: Effect| spec.effects.contains(&e);
    let path_resources = || -> Vec<ResourceRef> {
        if subjects.is_empty() {
            vec![ResourceRef::path("")] // matches a `*` path glob
        } else {
            subjects
                .iter()
                .map(|s| ResourceRef::path(s.as_str()))
                .collect()
        }
    };
    if has(Effect::Write) {
        for r in path_resources() {
            reqs.push((Action::from("workspace.write"), r));
        }
    } else if has(Effect::Read) || has(Effect::Filesystem) {
        for r in path_resources() {
            reqs.push((Action::from("workspace.read"), r));
        }
    }
    if has(Effect::Process) || has(Effect::LocalSystem) {
        reqs.push((
            Action::from("process.exec"),
            ResourceRef::any(ResourceKind::Process),
        ));
    }
    if has(Effect::Network) {
        reqs.push((
            Action::from("network.fetch"),
            ResourceRef::any(ResourceKind::Network),
        ));
    }
    if has(Effect::Browser) {
        // ResourceKind has no Browser variant; browser navigation is gated as network egress.
        reqs.push((
            Action::from("browser.navigate"),
            ResourceRef::any(ResourceKind::Network),
        ));
    }
    reqs
}

/// The dispatcher: runs pre-tool hooks, enforces the authorization policy + permission rules +
/// approval, then executes through the guarded system.
pub struct Executor {
    registry: ToolRegistry,
    perms: Mutex<PermissionManager>,
    /// Interior-mutable so a surface can swap the approver (e.g. the TUI's modal) even when the executor
    /// is shared as an `Arc<Executor>` — which it is once the reflexive loop host re-enters it.
    approver: Mutex<Arc<dyn Approver>>,
    ctx: ToolContext,
    hooks: Vec<Arc<dyn PreToolHook>>,
    /// The authorization floor. `None` disables the policy layer (permission rules only).
    policy: Option<AuthorizationPolicy>,
    caller: Caller,
    trust: Trust,
    /// Depth of the active "pre-approved plan" scope. `>0` means the ops being dispatched belong to a
    /// plan the user already approved as a whole, so the per-op approval gate is skipped (deny rules
    /// still win). A depth (not a bool) so a plan that runs a nested plan stays approved throughout.
    plan_scope: AtomicU32,
    /// Set when the user answered `always` at a plan prompt: every subsequent plan this session runs
    /// without asking.
    trust_all: AtomicBool,
}

/// Holds an approved-plan scope open. While alive, [`Executor::dispatch`] skips the per-op approval
/// prompt; `Drop` closes the scope (decrementing the depth so re-planning asks again next round).
pub struct PlanScopeGuard<'a>(&'a AtomicU32);

impl Drop for PlanScopeGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Executor {
    pub fn new(
        registry: ToolRegistry,
        perms: PermissionManager,
        approver: Arc<dyn Approver>,
        ctx: ToolContext,
    ) -> Self {
        Self {
            registry,
            perms: Mutex::new(perms),
            approver: Mutex::new(approver),
            ctx,
            hooks: Vec::new(),
            policy: None,
            caller: default_local_caller(),
            trust: default_local_trust(),
            plan_scope: AtomicU32::new(0),
            trust_all: AtomicBool::new(false),
        }
    }

    /// Whether we're currently executing the ops of an already-approved plan (or the user trusts all
    /// plans). When true, [`dispatch`](Self::dispatch) skips the per-op approval prompt.
    pub fn in_approved_scope(&self) -> bool {
        self.trust_all.load(Ordering::SeqCst) || self.plan_scope.load(Ordering::SeqCst) > 0
    }

    /// Open a pre-approved scope for the duration of the returned guard — used when the act of running
    /// *is* the approval (the REPL `/run`, where the human already reviewed the plan). Inner ops dispatch
    /// without prompting; the guard closes the scope on drop.
    pub fn enter_approved_scope(&self) -> PlanScopeGuard<'_> {
        self.plan_scope.fetch_add(1, Ordering::SeqCst);
        PlanScopeGuard(&self.plan_scope)
    }

    /// Approve a whole plan once, then keep it pre-approved while the returned guard is held. If already
    /// inside an approved scope (a nested `run_plan`) or the user trusts all plans, returns a guard
    /// without prompting. `None` means the user rejected the plan. `summary`/`ops` come from the plan's
    /// risk preview — the plan tree itself was already rendered (the `flow.plan` observation).
    pub async fn approve_plan(&self, summary: &str, ops: usize) -> Option<PlanScopeGuard<'_>> {
        if self.in_approved_scope() {
            return Some(self.enter_approved_scope());
        }
        let approver = self.approver.lock().unwrap().clone();
        match approver.request_plan(summary, ops).await {
            ApprovalChoice::Allow => Some(self.enter_approved_scope()),
            ApprovalChoice::AllowAlways(_) => {
                self.trust_all.store(true, Ordering::SeqCst);
                Some(self.enter_approved_scope())
            }
            ApprovalChoice::Deny => None,
        }
    }

    /// Attach ordered pre-tool hooks (run before the permission gate).
    pub fn with_hooks(mut self, hooks: Vec<Arc<dyn PreToolHook>>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Replace the approval handler (e.g. a surface installing its own interactive approver before
    /// driving turns — the TUI swaps in a modal approver).
    pub fn set_approver(&self, approver: Arc<dyn Approver>) {
        *self.approver.lock().unwrap() = approver;
    }

    /// Install the reflexive [`LoopHost`] capability onto this executor's [`ToolContext`], so the
    /// `plan`/`run_plan` ops dispatched through it can re-enter the planner/interpreter. Done by the
    /// engine once per turn, after the executor is built (the host holds a `Weak` back to this same
    /// executor, so it can only be wired in afterwards). Mirrors [`set_approver`](Self::set_approver).
    pub fn set_loop_host(&mut self, loop_host: Arc<dyn LoopHost>) {
        self.ctx.loop_host = Some(loop_host);
    }

    /// Pre-allow these op names (they dispatch without an approval prompt). The engine uses this to
    /// whitelist its own loop machinery (`plan`/`run_plan`/`observe`/…) — internal control flow, not
    /// user-facing actions. A `deny` rule still wins, and the *inner* ops a plan runs gate individually.
    pub fn allow(&self, rules: &[&str]) {
        let mut perms = self.perms.lock().unwrap();
        for r in rules {
            perms.add_allow(r);
        }
    }

    /// The current approver (used by flow nodes such as `confirm` that need to request approval
    /// outside of a full tool dispatch). Returns a clone of the `Arc` (the approver is interior-mutable).
    pub fn approver(&self) -> Arc<dyn Approver> {
        self.approver.lock().unwrap().clone()
    }

    /// Enable the authorization-policy floor: every tool call's effects are evaluated against
    /// `policy` (default-deny) before the permission rules run.
    pub fn with_policy(mut self, policy: AuthorizationPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Set the resolved caller + trust the policy evaluates against (default: the local
    /// single-user identity). Surfaces resolve this via `flux-auth` before constructing the agent.
    pub fn with_identity(mut self, caller: Caller, trust: Trust) -> Self {
        self.caller = caller;
        self.trust = trust;
        self
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// The execution context (guarded system, redactor, spawner). Lets a caller derive a sibling
    /// executor over the *same* guarded surface — e.g. a read-only research executor scoped to a
    /// subset of tools for the planner.
    pub fn context(&self) -> &ToolContext {
        &self.ctx
    }

    /// The current allow rules (for persistence by the caller).
    pub fn allow_rules(&self) -> Vec<String> {
        self.perms.lock().unwrap().allow_rules()
    }

    /// Record an externally-derived observation (e.g. a startup toolchain scan) into the shared log.
    pub fn observe(&self, observation: Observation) {
        self.ctx.evidence.lock().unwrap().record(observation);
    }

    /// A snapshot of the evidence log accumulated so far (shared with the context, so flow-emitted
    /// `observe(…)` observations are part of this same trail).
    pub fn evidence(&self) -> EvidenceLog {
        self.ctx.evidence.lock().unwrap().clone()
    }

    /// Run a tool call through the full safety envelope.
    pub async fn dispatch(&self, name: &str, params: Value) -> ToolResult {
        let Some(tool) = self.registry.get(name) else {
            return ToolResult::error(format!("unknown tool: {name}"));
        };

        // Pre-tool hooks (system-priority first): may modify the input or deny the call.
        let mut params = params;
        for hook in &self.hooks {
            match hook.pre_tool(name, &params) {
                HookOutcome::Continue => {}
                HookOutcome::Modify(p) => params = p,
                HookOutcome::Deny(reason) => {
                    return ToolResult::error(format!("`{name}` blocked by hook: {reason}"));
                }
            }
        }

        let spec = tool.spec();
        let subjects = tool.permission_subjects(&params);
        let intents = tool.intents(&params);

        // 1. Authorization-policy floor (if configured): default-deny on any ungranted effect. A
        //    `Deny` short-circuits; an `ApprovalRequired` (e.g. a grant marked `requires_approval`,
        //    like the default `process.exec`) forces the approval gate below even if a permissive
        //    allow-rule would otherwise satisfy it — the policy is the floor, rules can't widen it.
        let mut policy_requires_approval = false;
        if let Some(policy) = &self.policy {
            for (action, resource) in effect_requests(&spec, &subjects) {
                let req = PolicyRequest {
                    caller: &self.caller,
                    trust: &self.trust,
                    action: &action,
                    resource: &resource,
                };
                match evaluate(policy, &req).decision {
                    Decision::Deny => {
                        return ToolResult::error(format!(
                            "`{name}` denied by policy ({} on {:?})",
                            action.0, resource.kind
                        ));
                    }
                    Decision::ApprovalRequired => policy_requires_approval = true,
                    Decision::Allow => {}
                }
            }
        }

        // 2. Permission rules (coder-style): deny wins; otherwise allow/ask for tool + subjects.
        let perm = self.perms.lock().unwrap().check(name, &subjects);
        if perm == PermDecision::Deny {
            return ToolResult::error(format!("`{name}` denied by permission rules"));
        }

        // 3. Evidence + reactions: record this call (and a destructive marker when matched), then
        //    let the built-in escalation reaction decide whether approval must be forced.
        let mut observations = vec![Observation::new(
            "tool_call",
            Phase::Turn,
            json!({ "tool": name, "subjects": subjects }),
        )];
        if intents.is_destructive() {
            observations.push(Observation::new(
                KIND_DESTRUCTIVE,
                Phase::Turn,
                json!({ "tool": name, "subjects": subjects }),
            ));
        }
        let escalate = observations
            .iter()
            .any(|o| !DestructiveEscalation.react(o).is_empty());
        self.ctx.evidence.lock().unwrap().extend(observations);

        // 4. Approval gate. Destructive operations — and any effect the policy marked
        //    `requires_approval` — are forced to approval even under a permissive allow-rule;
        //    everything else asks only when the rules didn't already allow it. A write tool that
        //    reports no path subjects is also forced to prompt: its effect would otherwise resolve
        //    to an unscoped (`path:"*"`-matching) authorization rather than a specific file.
        let unscoped_write = spec.effects.contains(&Effect::Write) && subjects.is_empty();
        let force_approval = escalate
            || spec.risk == Risk::Destructive
            || policy_requires_approval
            || unscoped_write;
        //    Inside an approved-plan scope the prompt is skipped entirely — the user approved the plan as
        //    a whole (its risk badge disclosed every op). Hard denies (steps 1-2 above) still apply.
        if !self.in_approved_scope() && (force_approval || perm != PermDecision::Allow) {
            let approver = self.approver.lock().unwrap().clone();
            match approver.request(name, &subjects, &intents).await {
                ApprovalChoice::Allow => {}
                ApprovalChoice::AllowAlways(rule) => {
                    self.perms.lock().unwrap().add_allow(&rule);
                }
                ApprovalChoice::Deny => {
                    return ToolResult::error(format!("`{name}` denied by user"));
                }
            }
        }

        // 5. System boundary: the only place real IO happens. Redact secrets from the result —
        //    both the success content and any error — before it reaches the model or the logs.
        let result = match tool.execute(&self.ctx, params).await {
            Ok(mut r) => {
                // Redact BOTH faces: the view can carry file content / diffs that include secrets.
                r.content = self.ctx.redactor.redact(&r.content);
                r.view = r.view.map(|v| self.ctx.redactor.redact(&v));
                r
            }
            Err(e) => ToolResult::error(self.ctx.redactor.redact(&e.to_string())),
        };
        // 6. Record a `tool_error` observation on a failed call (an op that ran and errored), so
        //    `metrics()`/`evidence` give a model-in-the-loop the failure signal to retry/stop on. The
        //    matching `tool_call` was already recorded above, so the shared log carries both.
        if result.is_error {
            self.ctx.evidence.lock().unwrap().record(Observation::new(
                "tool_error",
                Phase::Turn,
                json!({ "tool": name }),
            ));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::Workspace;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_ctx() -> ToolContext {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-rt-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    /// A tool that echoes a `text` param, with the value as its permission subject.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("echo", "echo text", json!({"type": "object"}))
        }
        fn permission_subjects(&self, params: &Value) -> Vec<String> {
            params
                .get("text")
                .and_then(|v| v.as_str())
                .map(|s| vec![s.to_string()])
                .unwrap_or_default()
        }
        async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                params["text"].as_str().unwrap_or("").to_string(),
            ))
        }
    }

    /// Records whether it was asked, and returns a fixed choice.
    struct RecordingApprover {
        asked: AtomicBool,
        choice: fn() -> ApprovalChoice,
    }
    #[async_trait]
    impl Approver for RecordingApprover {
        async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
            self.asked.store(true, Ordering::Relaxed);
            (self.choice)()
        }
    }

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(EchoTool));
        r
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            test_ctx(),
        );
        let r = ex.dispatch("nope", json!({})).await;
        assert!(r.is_error);
        assert!(r.content.contains("unknown tool"));
    }

    #[tokio::test]
    async fn ask_then_allow_executes() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Allow,
        });
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            approver.clone(),
            test_ctx(),
        );
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(!r.is_error);
        assert_eq!(r.content, "hi");
        assert!(approver.asked.load(Ordering::Relaxed), "should have asked");
    }

    #[tokio::test]
    async fn deny_rule_blocks_without_asking() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Allow,
        });
        let perms = PermissionManager::from_rules(&[], &["echo".into()]);
        let ex = Executor::new(registry(), perms, approver.clone(), test_ctx());
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(r.is_error);
        assert!(r.content.contains("denied by permission rules"));
        assert!(!approver.asked.load(Ordering::Relaxed), "deny must not ask");
    }

    #[tokio::test]
    async fn approved_scope_skips_the_per_op_prompt() {
        // The approver would DENY if asked, so a skipped prompt is the only way the op can run.
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Deny,
        });
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            approver.clone(),
            test_ctx(),
        );

        // Outside any approved scope: the op prompts (and is denied).
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(r.is_error, "outside a scope the op prompts and is denied");
        assert!(approver.asked.load(Ordering::Relaxed));

        // Inside an approved-plan scope: no prompt, the op runs.
        approver.asked.store(false, Ordering::Relaxed);
        let r = {
            let _scope = ex.enter_approved_scope();
            ex.dispatch("echo", json!({"text": "hi"})).await
        };
        assert!(
            !r.is_error,
            "inside an approved scope the op runs: {}",
            r.content
        );
        assert_eq!(r.content, "hi");
        assert!(
            !approver.asked.load(Ordering::Relaxed),
            "no per-op prompt inside an approved scope"
        );

        // Scope closed (guard dropped): prompts again next time.
        approver.asked.store(false, Ordering::Relaxed);
        let _ = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "scope closed → prompts again"
        );
    }

    #[tokio::test]
    async fn approved_scope_still_respects_deny_rules() {
        let perms = PermissionManager::from_rules(&[], &["echo".into()]);
        let ex = Executor::new(registry(), perms, Arc::new(AllowApprover), test_ctx());
        let _scope = ex.enter_approved_scope();
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(
            r.is_error,
            "a deny rule still blocks inside an approved plan"
        );
        assert!(r.content.contains("denied by permission rules"));
    }

    #[tokio::test]
    async fn approve_plan_opens_scope_and_always_trusts_the_session() {
        // `RecordingApprover` only implements `request`; `request_plan` uses the trait default that
        // delegates to it, so this also covers the default delegation.
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::AllowAlways("*plans*".into()),
        });
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            approver.clone(),
            test_ctx(),
        );
        assert!(!ex.in_approved_scope());
        {
            let scope = ex.approve_plan("medium · mutating", 2).await;
            assert!(scope.is_some(), "Allow/AllowAlways opens a scope");
            assert!(ex.in_approved_scope());
        }
        // `always` set the session-wide trust, so we stay approved after the guard drops.
        assert!(
            ex.in_approved_scope(),
            "`always` trusts every plan for the rest of the session"
        );
        approver.asked.store(false, Ordering::Relaxed);
        let _ = ex.approve_plan("low", 1).await;
        assert!(
            !approver.asked.load(Ordering::Relaxed),
            "a trusted session does not prompt again"
        );
    }

    #[tokio::test]
    async fn approve_plan_deny_returns_none() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Deny,
        });
        let ex = Executor::new(
            registry(),
            PermissionManager::new(),
            approver.clone(),
            test_ctx(),
        );
        assert!(
            ex.approve_plan("medium", 1).await.is_none(),
            "Deny → no scope"
        );
        assert!(!ex.in_approved_scope());
    }

    #[tokio::test]
    async fn allow_rule_executes_without_asking() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Deny, // would deny if asked
        });
        let perms = PermissionManager::from_rules(&["echo".into()], &[]);
        let ex = Executor::new(registry(), perms, approver.clone(), test_ctx());
        let r = ex.dispatch("echo", json!({"text": "hi"})).await;
        assert!(!r.is_error);
        assert!(
            !approver.asked.load(Ordering::Relaxed),
            "allow must not ask"
        );
    }

    /// A tool that echoes a fixed string back as successful content (used to test redaction).
    struct LeakTool(String);
    #[async_trait]
    impl Tool for LeakTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("leak", "echo content", json!({"type": "object"}))
        }
        async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(self.0.clone()))
        }
    }

    #[tokio::test]
    async fn secrets_redacted_from_success_output() {
        let secret = "sk-ant-supersecretvalue123456";
        let mut ctx = test_ctx();
        ctx.redactor.add_secret(secret);

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(LeakTool(format!("the key is {secret} ok"))));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["leak".into()], &[]),
            Arc::new(DenyApprover),
            ctx,
        );
        let r = ex.dispatch("leak", json!({})).await;
        assert!(!r.is_error);
        assert!(!r.content.contains(secret), "secret leaked: {}", r.content);
        assert!(r.content.contains("[redacted]"));
    }

    #[test]
    fn secret_resolver_reads_env_and_seeds_redactor() {
        let key = format!("FLUX_TEST_SECRET_{}", std::process::id());
        std::env::set_var(&key, "topsecretvalue");
        let mut redactor = Redactor::new();
        SecretResolver::new().seed_redactor(&mut redactor, &[flux_secret::Ref::env(&key)]);
        assert_eq!(redactor.redact("x topsecretvalue y"), "x [redacted] y");
        std::env::remove_var(&key);
    }

    /// A tool that declares a destructive command intent (but does nothing).
    struct DestructiveTool;
    #[async_trait]
    impl Tool for DestructiveTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("danger", "destructive", json!({"type": "object"}))
                .with_effects(vec![Effect::Process])
                .with_risk(Risk::High)
        }
        fn intents(&self, _p: &Value) -> IntentSet {
            use flux_spec::{Intent, IntentBehavior, IntentCertainty, IntentRole, IntentTarget};
            let mut s = IntentSet::new();
            s.push(Intent {
                behavior: IntentBehavior::CommandExecution,
                target: IntentTarget::Process {
                    command: "rm -rf /tmp/x".into(),
                },
                role: IntentRole::ProcessCommand,
                certainty: IntentCertainty::Certain,
            });
            s
        }
        async fn execute(&self, _ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("ran"))
        }
    }

    #[tokio::test]
    async fn destructive_op_is_escalated_and_recorded_even_under_allow_rule() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Deny, // user declines the forced prompt
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(DestructiveTool));
        // A bare allow-rule that would normally skip the approval prompt entirely.
        let perms = PermissionManager::from_rules(&["danger".into()], &[]);
        let ex = Executor::new(reg, perms, approver.clone(), test_ctx());

        let r = ex.dispatch("danger", json!({})).await;
        assert!(r.is_error, "the forced approval was declined → denied");
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "a destructive op must ask for approval despite the allow-rule"
        );
        let ev = ex.evidence();
        assert_eq!(ev.by_kind(KIND_DESTRUCTIVE).count(), 1);
        assert!(ev.by_kind("tool_call").count() >= 1);
    }

    /// A tool that declares a filesystem-write effect (used to test the policy floor).
    struct WriteishTool;
    #[async_trait]
    impl Tool for WriteishTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("save", "save", json!({"type": "object"}))
                .with_effects(vec![Effect::Write, Effect::Filesystem])
        }
        fn permission_subjects(&self, _p: &Value) -> Vec<String> {
            vec!["out.txt".into()]
        }
        async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("saved"))
        }
    }

    #[tokio::test]
    async fn policy_denies_op_outside_grant_set_even_when_rules_allow() {
        use flux_policy::{Grant, SubjectKind, SubjectRef};
        // A policy that grants only reads — write is outside the grant set (default-deny).
        let read_only = AuthorizationPolicy {
            grants: vec![Grant {
                subjects: vec![SubjectRef {
                    kind: SubjectKind::User,
                    id: "*".into(),
                }],
                resources: vec![ResourceRef::path("*")],
                actions: vec![Action::from("workspace.read")],
                required_trust: TrustLevel::Untrusted,
                required_scopes: Vec::new(),
                requires_approval: false,
            }],
        };
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(WriteishTool));
        // A permissive allow-rule + auto-approver would normally let the write through.
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["save".into()], &[]),
            Arc::new(AllowApprover),
            test_ctx(),
        )
        .with_policy(read_only);
        let r = ex.dispatch("save", json!({})).await;
        assert!(r.is_error);
        assert!(r.content.contains("denied by policy"), "got: {}", r.content);
    }

    #[test]
    fn subset_none_inherits_all_some_empty_grants_none() {
        let r = registry(); // contains "echo"
        assert_eq!(r.subset(None).names(), vec!["echo".to_string()]);
        assert!(
            r.subset(Some(&[])).names().is_empty(),
            "an explicit empty allowlist (tools: []) must grant zero tools"
        );
        assert_eq!(
            r.subset(Some(&["echo".to_string()])).names(),
            vec!["echo".to_string()]
        );
        assert!(r.subset(Some(&["nope".to_string()])).names().is_empty());
    }

    /// A non-destructive tool with a Process effect (gated only by the policy floor).
    struct ProcTool;
    #[async_trait]
    impl Tool for ProcTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("proc", "run", json!({"type": "object"}))
                .with_effects(vec![Effect::Process])
        }
        async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("ran"))
        }
    }

    #[tokio::test]
    async fn policy_requires_approval_forces_prompt_even_under_allow_rule() {
        use flux_policy::{Grant, SubjectKind, SubjectRef};
        // A grant that permits process.exec but marks it requires_approval (mirrors the default
        // local grant for process exec). The op is non-destructive, so only this flag should force
        // the prompt.
        let policy = AuthorizationPolicy {
            grants: vec![Grant {
                subjects: vec![SubjectRef {
                    kind: SubjectKind::User,
                    id: "*".into(),
                }],
                resources: vec![ResourceRef::any(ResourceKind::Process)],
                actions: vec![Action::from("process.exec")],
                required_trust: TrustLevel::Untrusted,
                required_scopes: Vec::new(),
                requires_approval: true,
            }],
        };
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Allow,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ProcTool));
        // A permissive allow-rule would normally skip the prompt entirely.
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["proc".into()], &[]),
            approver.clone(),
            test_ctx(),
        )
        .with_policy(policy);
        let r = ex.dispatch("proc", json!({})).await;
        assert!(!r.is_error, "approved → executes: {}", r.content);
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "a policy grant marked requires_approval must force a prompt despite the allow-rule"
        );
    }

    /// A write-effect tool that reports no path subjects (the unscoped-write case).
    struct UnscopedWriteTool;
    #[async_trait]
    impl Tool for UnscopedWriteTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("blindwrite", "write", json!({"type": "object"}))
                .with_effects(vec![Effect::Write])
        }
        async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("wrote"))
        }
    }

    #[tokio::test]
    async fn write_without_subjects_forces_approval() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::Allow,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(UnscopedWriteTool));
        // A bare allow-rule would normally skip the prompt entirely.
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["blindwrite".into()], &[]),
            approver.clone(),
            test_ctx(),
        );
        let r = ex.dispatch("blindwrite", json!({})).await;
        assert!(!r.is_error);
        assert!(
            approver.asked.load(Ordering::Relaxed),
            "a write tool reporting no path subjects must force an approval prompt"
        );
    }

    #[tokio::test]
    async fn hook_deny_short_circuits_before_policy_and_execution() {
        use std::sync::atomic::AtomicBool;

        struct DenyHook;
        impl PreToolHook for DenyHook {
            fn pre_tool(&self, _tool: &str, _input: &Value) -> HookOutcome {
                HookOutcome::Deny("blocked for test".into())
            }
        }
        static EXECUTED: AtomicBool = AtomicBool::new(false);
        struct FlagTool;
        #[async_trait]
        impl Tool for FlagTool {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only("flag", "flag", json!({"type": "object"}))
            }
            async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
                EXECUTED.store(true, Ordering::Relaxed);
                Ok(ToolResult::ok("ran"))
            }
        }

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(FlagTool));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["flag".into()], &[]),
            Arc::new(AllowApprover),
            test_ctx(),
        )
        .with_hooks(vec![Arc::new(DenyHook)]);
        let r = ex.dispatch("flag", json!({})).await;
        assert!(r.is_error);
        assert!(r.content.contains("blocked by hook"), "got: {}", r.content);
        assert!(
            !EXECUTED.load(Ordering::Relaxed),
            "a hook deny must short-circuit before the tool executes"
        );
    }

    #[test]
    fn observe_records_into_log() {
        let ex = Executor::new(
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            test_ctx(),
        );
        ex.observe(Observation::new(
            "toolchain",
            Phase::Startup,
            json!({"tools": ["read"]}),
        ));
        assert_eq!(ex.evidence().by_kind("toolchain").count(), 1);
    }

    #[tokio::test]
    async fn allow_always_persists_rule() {
        let approver = Arc::new(RecordingApprover {
            asked: AtomicBool::new(false),
            choice: || ApprovalChoice::AllowAlways("echo".into()),
        });
        let ex = Executor::new(registry(), PermissionManager::new(), approver, test_ctx());
        let _ = ex.dispatch("echo", json!({"text": "a"})).await;
        assert_eq!(ex.allow_rules(), vec!["echo".to_string()]);
    }

    /// A tool standing in for a grouped op (e.g. a git op) in surfacing tests.
    struct GitishTool;
    #[async_trait]
    impl Tool for GitishTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("git_status", "git status", json!({"type": "object"}))
        }
        fn permission_subjects(&self, _p: &Value) -> Vec<String> {
            Vec::new()
        }
        async fn execute(&self, _ctx: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("clean"))
        }
    }

    fn git_group() -> Vec<flux_evidence::ToolGroup> {
        vec![flux_evidence::ToolGroup {
            name: "git".into(),
            tools: vec!["git_status".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: "project.signal".into(),
                signal: Some("git_repo".into()),
            }],
            ..Default::default()
        }]
    }

    #[test]
    fn advertised_op_names_gates_grouped_ops() {
        let specs = vec![
            ToolSpec::read_only("read", "read", json!({"type": "object"})),
            ToolSpec::read_only("git_status", "git status", json!({"type": "object"})),
        ];
        // Inactive group → only the core op is advertised.
        let none = advertised_op_names(&specs, &git_group(), &HashSet::new());
        assert!(none.contains("read") && !none.contains("git_status"));
        // Active group → both.
        let active: HashSet<String> = ["git".to_string()].into_iter().collect();
        let both = advertised_op_names(&specs, &git_group(), &active);
        assert!(both.contains("read") && both.contains("git_status"));
        // Empty manifest, no group-tagged specs → everything (no gating).
        let all_set = advertised_op_names(&specs, &[], &HashSet::new());
        assert!(all_set.contains("read") && all_set.contains("git_status"));
    }

    #[test]
    fn spec_group_tag_is_honored_without_a_manifest_tools_list() {
        // A spec tagged via ToolSpec::with_group (the committed field) is gated even when the manifest
        // group lists no `tools` (membership falls back to the spec's own tag).
        let tagged =
            ToolSpec::read_only("git_status", "s", json!({"type": "object"})).with_group("git");
        let group = vec![flux_evidence::ToolGroup {
            name: "git".into(),
            surface_when: vec![flux_evidence::SignalMatch {
                kind: "project.signal".into(),
                signal: Some("git_repo".into()),
            }],
            ..Default::default()
        }];
        assert!(!is_advertised(&tagged, &group, &HashSet::new()));
        let active: HashSet<String> = ["git".to_string()].into_iter().collect();
        assert!(is_advertised(&tagged, &group, &active));
    }

    #[test]
    fn active_specs_filters_by_group() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(GitishTool));
        // Group inactive → git op hidden, core op kept.
        let hidden = reg.active_specs(&git_group(), &HashSet::new());
        let names: Vec<&str> = hidden.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"echo") && !names.contains(&"git_status"));
        // Group active → all specs (== specs()).
        let active: HashSet<String> = ["git".to_string()].into_iter().collect();
        assert_eq!(
            reg.active_specs(&git_group(), &active).len(),
            reg.specs().len()
        );
    }

    #[test]
    fn trim_tool_output_caps_and_annotates() {
        // Under cap → unchanged.
        assert_eq!(trim_tool_output("hello".into(), 100, "bash"), "hello");
        // cap 0 → disabled.
        let big = "x".repeat(50);
        assert_eq!(trim_tool_output(big.clone(), 0, "bash"), big);
        // Over cap → truncated + notice.
        let out = trim_tool_output("x".repeat(50), 10, "bash");
        assert!(out.starts_with(&"x".repeat(10)));
        assert!(out.contains("truncated") && out.contains("40 of 50"));
    }

    #[test]
    fn detect_signals_finds_markers_walking_up() {
        let base = std::env::temp_dir().join(format!("flux-detect-{}", std::process::id()));
        let sub = base.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(base.join(".git")).unwrap();
        std::fs::write(base.join("go.mod"), "module x\n").unwrap();
        let sigs = detect_signals(&sub);
        let has = |s: &str| {
            sigs.iter()
                .any(|o| o.data.get("signal").and_then(|v| v.as_str()) == Some(s))
        };
        // Found from a nested subdirectory (walk-up).
        assert!(has("git_repo") && has("go"));
        assert!(!has("python"));
        std::fs::remove_dir_all(&base).ok();
    }
}
