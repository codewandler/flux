//! The [`App`]: the runtime host that turns a parsed [`Program`] into a running multi-agent system.
//!
//! An app owns three things — the program (agents/channels/triggers/journeys), a [`ToolRegistry`]
//! assembled from the builtins + the orchestration op-pack (+ cognition ops when a provider is wired),
//! and the in-process [`Bus`]. The [`Engine`] behind it is the worker the public surface delegates to
//! (and that the `spawn` op re-enters); it is held in an `Arc` so the orchestration ops can hold a
//! `Weak` back-reference without a cycle.
//!
//! A journey is executed by **reusing flux-flow's engine path**: a real [`Executor`] (the full
//! permission + approval envelope) drives `flux_flow::runtime::execute_flow` over the journey's
//! `DraftAst`, with a [`FlowStore`] for state and an [`AgentSink`] for output. Nothing about the
//! interpreter is reinvented here — the multi-agent layer is pure wiring over the existing engine.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_agent::{AgentSpec, Permissions, DEFAULT_COMPACT_THRESHOLD_CHARS};
use flux_core::{Error, Result, Usage};
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_flow::registry::analyze_composites;
use flux_flow::state::FlowStore;
use flux_flow::AgentSink;
use flux_lang::ast::{Node, SymbolName, Value as FluxValue, Visibility};
use flux_lang::program::{AgentDecl, PermissionDecl, Program};
use flux_lang::runtime::FlowOutcome;
use flux_orchestrate::{SubAgents, TaskTool};
use flux_provider::{Effort, Provider};
use flux_runtime::{
    scope_runtime_turn, AllowApprover, Approver, DenyApprover, ExecutionAuthorization,
    ExecutionEnvironment, Executor, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
};
use flux_secret::Redactor;
use flux_system::{System, Workspace};

use crate::admission::DeliveryLoad;
use crate::bus::{delivery_origin, Bus};
use crate::ops::{self, JourneyHost};
use crate::park::{self, ParkedAsk};
use crate::supervisor::DeliverySupervisor;

/// How deep `spawn`-within-`spawn` may recurse before the engine refuses (cheap guard against a
/// journey that spawns itself unboundedly).
const MAX_SPAWN_DEPTH: u32 = 16;

/// Legacy grants for a journey in a program which declares no capability policy. Kept byte-for-byte
/// compatible; a declared policy replaces this implicit set with an explicit app/agent ceiling.
const LEGACY_JOURNEY_ALLOW: &[&str] = &[
    "emit", "send", "ask", "spawn", "read", "glob", "grep", "search",
];

#[derive(Debug, Clone)]
struct EffectiveCapabilities {
    /// Whether app or agent source declared a capability layer. When false, callers preserve the
    /// legacy journey/agent behavior rather than treating `allow` as a hard registry ceiling.
    declared: bool,
    /// Hard registry ceiling after app/agent intersections and denies.
    allow: Vec<String>,
    /// Calls pre-authorized by source. With only deny declarations this stays at the legacy safe
    /// journey set; `--yes` may approve other calls inside `allow`, never outside it.
    grants: Vec<String>,
    deny: Vec<String>,
}

struct AgentRuntimeProfile {
    spec: AgentSpec,
    registry: ToolRegistry,
    capabilities: EffectiveCapabilities,
}

struct JourneyRuntimeProfile {
    registry: ToolRegistry,
    capabilities: EffectiveCapabilities,
    model: String,
}

/// Host/local coder-style approval rules layered inside source-declared app capabilities. These may
/// include subject-scoped forms such as `Bash(git:*)`. They can approve or deny calls still present
/// in the app registry but can never restore an operation removed by the app/agent ceiling.
#[derive(Debug, Clone, Default)]
pub struct HostPermissionRules {
    /// Allow rules (`read`, `Bash(git:*)`, …) from the host's layered configuration.
    pub allow: Vec<String>,
    /// Deny rules evaluated before allows.
    pub deny: Vec<String>,
}

/// The result of running one journey: which journey, its textual result (the flow's `return`/last view),
/// and how many ops it dispatched.
#[derive(Debug, Clone)]
pub struct JourneyRun {
    pub journey: String,
    pub result: String,
    pub steps: usize,
    /// The turn(s)' accumulated token usage (C-33), when the run drove at least one model call that
    /// reported it — `None` for a run that dispatched no model turn (a pure-op journey). Summed
    /// across every `turn_end` the run's sink saw plus direct cognition calls in an authored
    /// journey, so a journey/agent turn with more than one model call attributes its full cost.
    pub usage: Option<Usage>,
    /// The canonical `provider/model` spec of the engine that drove this run (C-33) — an
    /// `agent`-bound trigger's actual engine (`flux_core::canonical_model_spec` over its provider +
    /// model), or the app's default model for a plain journey (a journey has no single "engine"; the
    /// app default is the honest stand-in, and it's only ever paired with real cost when `usage` is
    /// `Some`). A cost-display surface should ignore this when `usage` is `None`.
    pub model: String,
}

/// The runtime host for a multi-agent [`Program`]. Cheap to clone is *not* a goal — hold one `App` and
/// drive it; clone the [`Bus`] handle (via [`App::bus`]) if another task needs to emit.
///
/// App constructors install the documented local single-user authorization profile. Approval and
/// source-declared capabilities may narrow that profile; `auto_approve` never widens its policy
/// floor.
pub struct App {
    engine: Arc<Engine>,
}

impl App {
    /// Build a host for `program`. When `provider` is `Some`, the model-backed cognition ops
    /// (`ai.*`, `synth`) are registered too, so journeys may plan/extract/judge; with `None` the host
    /// is hermetic (pure ops only — no network, no model).
    pub fn new(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
    ) -> Self {
        Self::with_options(program, provider, model, false)
    }

    /// Fallible counterpart to [`new`](Self::new): validates the complete program against the
    /// assembled runtime catalog before returning, so a surface can fail before starting channels.
    pub fn try_new(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
    ) -> Result<Self> {
        Self::try_with_options(program, provider, model, false)
    }

    /// Build a host, choosing the approval posture. `auto_approve = false` (the safe default) **denies**
    /// any legacy-program op outside the pre-allowed orchestration + read-only set; `true` (the CLI's
    /// `--yes`) approves remaining prompts for trusted programs. A source-declared capability ceiling
    /// is absolute in either posture.
    pub fn with_options(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
    ) -> Self {
        Self::with_tools(program, provider, model, auto_approve, Vec::new())
    }

    /// Validating counterpart to [`with_options`](Self::with_options).
    pub fn try_with_options(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
    ) -> Result<Self> {
        Self::try_with_tools(program, provider, model, auto_approve, Vec::new())
    }

    /// Like [`with_options`](Self::with_options) but also registers `extra_tools` into the host
    /// registry — the seam the CLI uses to give journeys **and** the agent target (`trigger.agent`) the
    /// knowledge datasource retrieval ops (D-07) and the integration plugin tools (D-08), assembled in
    /// the async CLI layer so flux-app stays free of those deps.
    pub fn with_tools(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
        Self::with_sub_agents(
            program,
            provider,
            model,
            auto_approve,
            extra_tools,
            None,
            Redactor::new(),
        )
    }

    /// Validating counterpart to [`with_tools`](Self::with_tools).
    pub fn try_with_tools(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
    ) -> Result<Self> {
        Self::try_with_sub_agents(
            program,
            provider,
            model,
            auto_approve,
            extra_tools,
            None,
            Redactor::new(),
        )
    }

    /// Like [`with_tools`](Self::with_tools) but also wires a sub-agent [`SubAgents`] bundle: the
    /// `task` tool is registered into the host registry and every journey run installs the built
    /// spawner on its executor's [`ToolContext`], so a journey (or a composite op it calls, e.g.
    /// `strict_review`'s bounded reviewer fan-out — flux L-13) can delegate to a named role exactly as
    /// the CLI's `build_agent`/the SDK's `FlowClient::with_sub_agents` do — the same construction path
    /// (`SubAgents::into_spawner`), not a re-implementation.
    ///
    /// `redactor` is the host's shared secret redactor — pass the SAME one `resolve_secrets` seeded
    /// (clones share the value store), so program-declared secrets are scrubbed from every journey's
    /// and agent-target's tool output.
    pub fn with_sub_agents(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
        sub_agents: Option<SubAgents>,
        redactor: Redactor,
    ) -> Self {
        Self::with_events(
            program,
            provider,
            model,
            auto_approve,
            extra_tools,
            sub_agents,
            redactor,
            Arc::new(EventStore::in_memory().expect("flux-app: in-memory event store")),
        )
    }

    /// Validating counterpart to [`with_sub_agents`](Self::with_sub_agents).
    #[allow(clippy::too_many_arguments)]
    pub fn try_with_sub_agents(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
        sub_agents: Option<SubAgents>,
        redactor: Redactor,
    ) -> Result<Self> {
        Self::try_with_events(
            program,
            provider,
            model,
            auto_approve,
            extra_tools,
            sub_agents,
            redactor,
            Arc::new(EventStore::in_memory().map_err(other)?),
        )
    }

    /// Like [`with_sub_agents`](Self::with_sub_agents) but takes the host's [`EventStore`] explicitly
    /// (flux D-65). The seam a surface uses when ITS OWN plugin/endpoint wiring must reach the same
    /// per-run stream `App` records agent-target session memory and sub-agent spawn audit into: build
    /// the store before constructing `App`, install whatever audit/secret-sink hooks the surface needs
    /// on its plugin hosts (e.g. `flux_plugin::SystemHostCaps::with_egress_audit`/`with_secret_sink`,
    /// `flux_capabilities::EndpointBroker::with_cross_plugin_audit`) against a stream id minted from
    /// THIS store, then hand the store to `App` here — so the wiring's own audit trail lands in the
    /// SAME log as everything else the app records, rather than a second, disconnected store.
    /// [`with_sub_agents`](Self::with_sub_agents) is this constructor with a fresh in-memory store, so
    /// every existing caller is unaffected — this is strictly additive.
    #[allow(clippy::too_many_arguments)]
    pub fn with_events(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
        sub_agents: Option<SubAgents>,
        redactor: Redactor,
        events: Arc<EventStore>,
    ) -> Self {
        Self::with_events_and_permission_rules(
            program,
            provider,
            model,
            auto_approve,
            extra_tools,
            sub_agents,
            redactor,
            events,
            HostPermissionRules::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_events_and_permission_rules(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
        sub_agents: Option<SubAgents>,
        redactor: Redactor,
        events: Arc<EventStore>,
        host_permissions: HostPermissionRules,
    ) -> Self {
        Self::try_with_events_and_permission_rules_inner(
            program,
            provider,
            model.into(),
            auto_approve,
            extra_tools,
            sub_agents,
            redactor,
            events,
            host_permissions,
        )
        .expect("flux-app registry assembly failed")
    }

    #[allow(clippy::too_many_arguments)]
    fn try_with_events_and_permission_rules_inner(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: String,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
        sub_agents: Option<SubAgents>,
        redactor: Redactor,
        events: Arc<EventStore>,
        host_permissions: HostPermissionRules,
    ) -> Result<Self> {
        let environment = compatibility_execution_environment(extra_tools, auto_approve, redactor)?;
        Ok(App {
            engine: Engine::new(
                program,
                provider,
                model,
                environment,
                sub_agents,
                events,
                host_permissions,
                Vec::new(),
            )?,
        })
    }

    /// Build an App from one explicitly rooted guarded execution environment.
    ///
    /// This is the preferred surface assembly door. The environment's registry contains
    /// surface-contributed operations (datasources/endpoints/plugins); App composes its built-ins,
    /// cognition, and orchestration ops onto that catalog while retaining the same system,
    /// redactor, approval posture, policy, and identity for both journeys and lazily-created agent
    /// engines. No process current-directory lookup occurs here or during lazy agent construction.
    ///
    /// `disabled` is the raw `[tools] disable` patterns (flux C-183) — exact op names or
    /// `family.*` globs. Resolved exactly once here, against the fully assembled registry
    /// (built-ins, cognition, orchestration ops, and the environment's contributed catalog), before
    /// any journey or agent-target engine is constructed, so the resolved set can never churn
    /// mid-run (the A-95 stability rule) and is installed identically on every derived executor —
    /// both plain journeys (`build_executor`) and lazily-created per-agent engines
    /// (`agent_engine`/`build_agent_engine`) share the one `execution` template this resolves onto.
    /// An entry matching no known op prints a startup warning naming the entry, matching the
    /// interactive CLI path's wording.
    #[allow(clippy::too_many_arguments)]
    pub fn try_with_execution_environment(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        environment: ExecutionEnvironment,
        sub_agents: Option<SubAgents>,
        events: Arc<EventStore>,
        host_permissions: HostPermissionRules,
        disabled: Vec<String>,
    ) -> Result<Self> {
        let app = App {
            engine: Engine::new(
                program,
                provider,
                model.into(),
                environment,
                sub_agents,
                events,
                host_permissions,
                disabled,
            )?,
        };
        app.engine.validate()?;
        Ok(app)
    }

    /// Validating counterpart to [`with_events`](Self::with_events). This is the constructor product
    /// surfaces should use: all declarations and recursively nested calls are checked before any
    /// channel begins receiving events.
    #[allow(clippy::too_many_arguments)]
    pub fn try_with_events(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
        sub_agents: Option<SubAgents>,
        redactor: Redactor,
        events: Arc<EventStore>,
    ) -> Result<Self> {
        let app = Self::try_with_events_and_permission_rules_inner(
            program,
            provider,
            model.into(),
            auto_approve,
            extra_tools,
            sub_agents,
            redactor,
            events,
            HostPermissionRules::default(),
        )?;
        app.engine.validate()?;
        Ok(app)
    }

    /// Validating app constructor with host/local approval rules. Source declarations are applied as
    /// a hard registry ceiling first; these rules then decide calls inside that ceiling.
    #[allow(clippy::too_many_arguments)]
    pub fn try_with_events_and_permissions(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
        auto_approve: bool,
        extra_tools: Vec<Arc<dyn Tool>>,
        sub_agents: Option<SubAgents>,
        redactor: Redactor,
        events: Arc<EventStore>,
        host_permissions: HostPermissionRules,
    ) -> Result<Self> {
        let app = Self::try_with_events_and_permission_rules_inner(
            program,
            provider,
            model.into(),
            auto_approve,
            extra_tools,
            sub_agents,
            redactor,
            events,
            host_permissions,
        )?;
        app.engine.validate()?;
        Ok(app)
    }

    /// The program this host runs.
    pub fn program(&self) -> &Program {
        &self.engine.program
    }

    /// The assembled op registry (builtins + orchestration + optional cognition).
    pub fn registry(&self) -> &ToolRegistry {
        &self.engine.registry
    }

    /// A handle to the event bus (clone it to emit from another task).
    pub fn bus(&self) -> &Bus {
        &self.engine.bus
    }

    /// A handle to the host's shared [`EventStore`] — the same log agent-target session memory and
    /// sub-agent spawn audit land in, and (when a surface built this `App` via
    /// [`with_events`](Self::with_events)) the surface's own plugin/endpoint audit trail too (flux
    /// D-65).
    pub fn events(&self) -> Arc<EventStore> {
        self.engine.events.clone()
    }

    /// Build (or fetch the cached) [`FlowEngine`] for a declared agent — the seam the `a2a` channel
    /// uses to serve a program agent over HTTP/A2A. The engine shares this app's `EventStore`, so a
    /// session opened over HTTP and one woken by an agent-bound trigger live in the same log. Errors if
    /// the agent is undeclared or has no model provider.
    pub async fn agent_engine(&self, name: &str) -> Result<Arc<FlowEngine>> {
        self.engine.agent_engine(name).await
    }

    /// Look up a declared agent by name (e.g. for its A2A card metadata).
    pub fn agent_decl(&self, name: &str) -> Option<&AgentDecl> {
        self.engine.program.agents.iter().find(|a| a.name == name)
    }

    /// The program's sole declared agent, if there is exactly one. The `a2a` channel and the `--serve`
    /// flag bind to this when no agent is named explicitly; an ambiguous (multi-agent) or agent-less
    /// program must name its target.
    pub fn sole_agent(&self) -> Option<&AgentDecl> {
        match self.engine.program.agents.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// Inject one event and run every journey its label triggers **to completion**, returning each
    /// run's result. Events the journeys `emit` are processed too as a bounded cascade tree. This is
    /// the unit of work tests and the CLI channels drive; [`App::run`] is the long-running form. The
    /// App's sole delivery supervisor keeps each request and its cascades correlated.
    ///
    /// **Concurrent, not serialized (A-112).** Calls from different tasks run at the same time and
    /// each one sees only its own cascade tree and its own nesting budget, so a long sweep does not
    /// delay the deliveries submitted behind it. Journeys still share the App's mutable state —
    /// agent sessions, ask-parks, the recorded-send log — so two deliveries that touch the same
    /// conversation interleave rather than queue. A journey may not re-enter `deliver` on the App
    /// it is itself running under; that still fails fast.
    ///
    /// **Bounded (A-129).** At most [`DeliveryLoad::limit`] deliveries run at once; a submission
    /// beyond that **waits** for a slot rather than being dropped or rejected, and the wait
    /// propagates backwards — a caller submitting into a saturated App blocks in `deliver`. See
    /// [`App::with_max_inflight_deliveries`] and [`App::delivery_load`].
    pub async fn deliver(
        &self,
        label: impl Into<String>,
        payload: Value,
    ) -> Result<Vec<JourneyRun>> {
        self.engine
            .delivery
            .deliver(&self.engine, label.into(), payload)
            .await
    }

    /// Run as a long-lived supervisor: emit `startup` once, then route public bus events forever.
    /// A second concurrent call fails promptly; cancelling the active call releases that lease so a
    /// surface may resume supervision without creating another event receiver or repeating startup.
    pub async fn run(&self) -> Result<()> {
        self.engine.delivery.run(&self.engine).await
    }

    /// Bound how many deliveries may run at once (A-129), overriding
    /// [`crate::DEFAULT_MAX_INFLIGHT_DELIVERIES`] and the `FLUX_MAX_INFLIGHT_DELIVERIES`
    /// environment override. `0` is clamped to `1`; there is no "unbounded" setting.
    ///
    /// Consumes and returns the App because the limit is read once, when the delivery supervisor
    /// starts on the first [`deliver`](Self::deliver)/[`run`](Self::run) — configure it while you
    /// still hold the App by value, which is exactly the window in which it can have no actor.
    ///
    /// Raise it above your program's fan-out width if journeys deliberately wait on one another:
    /// deliveries at the bound *block*, so `limit` mutually-dependent deliveries can deadlock.
    #[must_use]
    pub fn with_max_inflight_deliveries(self, limit: usize) -> Self {
        self.engine.delivery.set_limit(limit);
        self
    }

    /// A snapshot of delivery admission (A-129): how many deliveries are running, how many are
    /// held by the bound, and what the bound is.
    ///
    /// `waiting > 0` is backpressure — work the App is refusing to start. A delivery counted in
    /// `in_flight` was admitted and is simply taking a long time. Distinguishing the two is the
    /// whole reason this is exposed: they look identical from a caller's latency.
    pub fn delivery_load(&self) -> DeliveryLoad {
        self.engine.delivery.load()
    }

    /// Test-only: how many messages the agent's bound session for `conversation` holds (`0` if none).
    /// Used to assert per-thread agent memory (same conversation reuses one session).
    #[cfg(test)]
    pub(crate) fn agent_session_len(&self, agent: &str, conversation: &str) -> usize {
        let map = self.engine.sessions.lock().expect("sessions map poisoned");
        match map.get(&(agent.to_string(), conversation.to_string())) {
            Some(sid) => self
                .engine
                .events
                .conversation(sid)
                .map(|m| m.len())
                .unwrap_or(0),
            None => 0,
        }
    }
}

/// The worker behind [`App`]. Owns the program, the registry, and the bus; resolves and runs journeys.
/// Held in an `Arc` so the `spawn` op can re-enter it through a `Weak<dyn JourneyHost>`.
pub(crate) struct Engine {
    pub(crate) program: Program,
    pub(crate) registry: ToolRegistry,
    pub(crate) bus: Bus,
    /// The sole trigger-routing owner. Public bus receivers are observation-only; direct deliveries
    /// and the long-running run lease submit roots to this coordinator.
    delivery: DeliverySupervisor,
    /// Fallback `spawn` recursion depth, used only by a journey run with no delivery scope around
    /// it. A run reached through the delivery supervisor counts against its own delivery's budget
    /// ([`crate::bus::DeliveryOrigin::depth`]) so concurrent deliveries stay independent.
    depth: Arc<AtomicU32>,
    /// Monotonic counter giving each journey run a distinct session id.
    runs: AtomicU64,
    /// Shared mechanical execution template. Per-journey/per-agent capability decisions replace
    /// only its catalog, permission rules, or approver; guarded system, redactor, spawner, policy,
    /// and identity remain identical across every derived executor.
    execution: ExecutionEnvironment,
    /// The model provider (when wired); needed to assemble an agent-target engine lazily. An
    /// `agent`-bound trigger with no provider is a clear error.
    provider: Option<Arc<dyn Provider>>,
    /// The host default model (used when an `AgentDecl` declares none, and for new sessions).
    default_model: String,
    /// The append-only store backing agent-target **session memory** (a Slack thread → one session).
    events: Arc<EventStore>,
    /// Lazily-built engines for agents named by an `agent`-bound trigger, keyed by agent name.
    agents: Mutex<HashMap<String, Arc<FlowEngine>>>,
    /// `(agent, conversation)` → persistent session id (in-memory; a restart starts threads fresh).
    sessions: Mutex<HashMap<(String, String), String>>,
    /// Local config approval rules. Applied after source capability narrowing; local deny wins.
    host_permissions: HostPermissionRules,
    /// Journeys parked on an `ask`, oldest first, each waiting for the correlated reply (A-11).
    /// [`Engine::run_triggers`] checks these before routing: a correlated inbound event resumes the
    /// oldest matching park instead of triggering journeys. See [`crate::park`] for the rule.
    parks: Mutex<Vec<ParkedAsk>>,
}

/// Build the app's guarded [`System`] rooted at `workspace`, resolving the OS-sandbox posture from the
/// environment (`FLUX_SANDBOX`/`FLUX_SANDBOX_NET`/`FLUX_SANDBOX_WRITABLE`, which the CLI exports into the
/// process env at startup) via [`flux_system::sandbox::Sandbox::resolve`]. A bare `System::new` defaults
/// to `Sandbox::disabled()` — no confinement and no fail-closed `require` enforcement — so without this
/// the `app run` journey/sub-agent path and the agent-target (`app run --serve`) path would silently
/// ignore `--sandbox`/`require` even though the docs promise those spawn paths inherit the posture.
/// Resolves to off/disabled when nothing is set, so hermetic callers stay unconfined.
fn guarded_system(workspace: Workspace) -> System {
    System::new(workspace).with_sandbox(flux_system::sandbox::Sandbox::resolve(
        flux_system::sandbox::SandboxSettings::from_env(),
    ))
}

/// Compatibility assembly for the pre-C-67 App constructors. New surfaces should build an
/// [`ExecutionEnvironment`] from their already-resolved workspace and call
/// [`App::try_with_execution_environment`]. The current-directory lookup lives only here, happens
/// eagerly, and is fallible; lazy agent creation never consults it again. These shims are planned
/// for removal in the next minor API cleanup once downstream callers migrate.
fn compatibility_execution_environment(
    extra_tools: Vec<Arc<dyn Tool>>,
    auto_approve: bool,
    redactor: Redactor,
) -> Result<ExecutionEnvironment> {
    let cwd = std::env::current_dir().map_err(other)?;
    let workspace = Workspace::from_env(cwd).map_err(other)?;
    let system = Arc::new(guarded_system(workspace));
    let mut registry = ToolRegistry::new();
    for (index, tool) in extra_tools.into_iter().enumerate() {
        registry.try_register_from(format!("app extra tool #{}", index + 1), tool)?;
    }
    let approver: Arc<dyn Approver> = if auto_approve {
        Arc::new(AllowApprover)
    } else {
        Arc::new(DenyApprover)
    };
    Ok(ExecutionEnvironment::new(
        system,
        registry,
        PermissionManager::new(),
        approver,
        ExecutionAuthorization::local(),
    )
    .with_redactor(redactor))
}

fn effective_capabilities(
    available: &[String],
    app: Option<&PermissionDecl>,
    agent: Option<&PermissionDecl>,
) -> EffectiveCapabilities {
    let declared = app.is_some() || agent.is_some();
    let mut allowed: BTreeSet<String> = available.iter().cloned().collect();
    let mut denied = BTreeSet::new();
    let mut explicit_allow = false;
    for layer in [app, agent].into_iter().flatten() {
        if let Some(layer_allow) = &layer.allow {
            explicit_allow = true;
            let layer_allow: HashSet<&str> = layer_allow.iter().map(String::as_str).collect();
            allowed.retain(|name| layer_allow.contains(name.as_str()));
        }
        for name in &layer.deny {
            allowed.remove(name);
            denied.insert(name.clone());
        }
    }
    let grants = if explicit_allow {
        allowed.iter().cloned().collect()
    } else {
        LEGACY_JOURNEY_ALLOW
            .iter()
            .filter(|&&name| allowed.contains(name))
            .map(|name| (*name).to_string())
            .collect()
    };
    EffectiveCapabilities {
        declared,
        allow: allowed.into_iter().collect(),
        grants,
        deny: denied.into_iter().collect(),
    }
}

fn narrowed_registry(
    registry: &ToolRegistry,
    capabilities: &EffectiveCapabilities,
) -> ToolRegistry {
    if capabilities.declared {
        registry.subset(Some(&capabilities.allow))
    } else {
        registry.clone()
    }
}

fn rebind_cognition(
    registry: &mut ToolRegistry,
    provider: Arc<dyn Provider>,
    model: &str,
    persona: &str,
    thinking: bool,
    effort: Option<Effort>,
) -> Result<()> {
    // A declared agent intentionally rebinds the canonical cognition family to its own
    // provider/model/persona. Use the explicit replacement API so this cannot be mistaken for an
    // ordinary pack collision.
    flux_cognition::CognitionPack::new(provider, model)
        .with_system_prefix(persona)
        .with_reasoning(thinking, effort)
        .replace_from("flux-app declared-agent cognition rebind", registry)
}

#[allow(clippy::too_many_arguments)]
async fn resolve_agent_runtime_profile(
    decl: &AgentDecl,
    app_permissions: Option<&PermissionDecl>,
    provider: Option<Arc<dyn Provider>>,
    mut registry: ToolRegistry,
    available: &[String],
    default_model: &str,
    system: Arc<System>,
    host_permissions: &HostPermissionRules,
) -> Result<AgentRuntimeProfile> {
    let root = system.workspace().root().to_path_buf();
    let mut spec = agent_spec_from_decl(decl, default_model, root, &system).await?;
    let capabilities =
        effective_capabilities(available, app_permissions, decl.permissions.as_ref());
    scope_datasource_tools(&mut registry, &decl.datasources)?;
    if let Some(provider) = provider {
        rebind_cognition(
            &mut registry,
            provider,
            &spec.model,
            &spec.effective_system_prompt(),
            spec.thinking,
            spec.effort,
        )?;
    }

    // For an open-ended agent, `tools` remains the visible catalog. A declared capability layer can
    // only remove entries from that catalog; authored journeys use the wider effective set below.
    if capabilities.declared {
        let allowed: HashSet<&str> = capabilities.allow.iter().map(String::as_str).collect();
        let visible: Vec<String> = decl
            .tools
            .iter()
            .filter(|name| allowed.contains(name.as_str()))
            .cloned()
            .collect();
        spec.tools = Some(visible.clone());
        spec.permissions = Permissions {
            allow: visible,
            deny: capabilities.deny.clone(),
        };
    }
    spec.permissions
        .allow
        .extend(host_permissions.allow.iter().cloned());
    spec.permissions
        .deny
        .extend(host_permissions.deny.iter().cloned());

    Ok(AgentRuntimeProfile {
        spec,
        registry,
        capabilities,
    })
}

fn validate_permission_decl(
    owner: &str,
    decl: &PermissionDecl,
    known: &HashSet<String>,
) -> Result<()> {
    for name in decl.allow.iter().flatten().chain(decl.deny.iter()) {
        if name.trim().is_empty() {
            return Err(Error::Other(format!(
                "{owner} declares an empty operation name"
            )));
        }
        if !known.contains(name) {
            return Err(Error::Other(format!(
                "{owner} names unknown operation `{name}`"
            )));
        }
    }
    Ok(())
}

fn validate_body_calls(
    body: &[Node],
    owner: &str,
    capabilities: &EffectiveCapabilities,
    known: &HashSet<String>,
    composites: &HashMap<String, &[Node]>,
    visiting: &mut HashSet<String>,
) -> Result<()> {
    let mut calls = Vec::new();
    flux_lang::analyze::for_each_node(body, &mut |node| {
        if let Node::Call { op, .. } = node {
            calls.push(op.clone());
        }
    });
    for op in calls {
        if !known.contains(&op) {
            return Err(Error::Other(format!(
                "{owner} calls unknown operation `{op}`"
            )));
        }
        if capabilities.declared && !capabilities.allow.contains(&op) {
            return Err(Error::Other(format!(
                "{owner} calls `{op}`, but the effective app/agent capability ceiling denies it"
            )));
        }
        if let Some(composite_body) = composites.get(&op) {
            if visiting.insert(op.clone()) {
                validate_body_calls(
                    composite_body,
                    &format!("{owner} via composite `{op}`"),
                    capabilities,
                    known,
                    composites,
                    visiting,
                )?;
                visiting.remove(&op);
            }
        }
    }
    Ok(())
}

impl Engine {
    /// `disabled` is the raw `[tools] disable` patterns (flux C-183), resolved here against the
    /// fully-assembled registry — see [`App::try_with_execution_environment`] for the full contract.
    #[allow(clippy::too_many_arguments)]
    fn new(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: String,
        environment: ExecutionEnvironment,
        sub_agents: Option<SubAgents>,
        events: Arc<EventStore>,
        host_permissions: HostPermissionRules,
        disabled: Vec<String>,
    ) -> Result<Arc<Self>> {
        let bus = Bus::new();
        let channels = Arc::new(program.channels.clone());
        // `events` backs agent-target session memory (see the field doc): an in-memory store is fine
        // for v1 (a restart starts threads fresh — flagged, pairs with D-02 later), but the store is
        // now always handed in by the caller (`App::with_sub_agents` passes a fresh in-memory one;
        // `App::with_events` lets a surface share its own, flux D-65) rather than created here, so
        // both constructors share one code path.
        // The surface resolves the guarded system exactly once. Lazy journey/agent construction
        // below derives only from this environment and never re-reads process cwd.
        let system = environment.system().clone();
        let contributed_registry = environment.registry().clone();
        // `new_cyclic`: the `spawn` op needs a back-reference to the engine it re-enters, but the
        // engine owns the registry that owns the op — a `Weak` breaks the cycle.
        let registration_error = std::cell::RefCell::new(None);
        let engine = Arc::new_cyclic(|weak: &Weak<Engine>| {
            let mut registry = ToolRegistry::new();
            if let Err(error) = flux_tools::try_register_builtins(&mut registry) {
                registration_error.borrow_mut().get_or_insert(error);
            }
            if let Some(provider) = provider.clone() {
                if let Err(error) = flux_cognition::CognitionPack::new(provider, model.clone())
                    .try_register_from("flux-app cognition pack", &mut registry)
                {
                    registration_error.borrow_mut().get_or_insert(error);
                }
            }
            let host: Weak<dyn JourneyHost> = weak.clone();
            if let Err(error) = ops::register(&mut registry, bus.clone(), channels, host) {
                registration_error.borrow_mut().get_or_insert(error);
            }
            // Surface-contributed datasource/endpoint/plugin operations retain their source labels
            // and fail on collisions with App-owned operations.
            if let Err(error) = registry.try_extend(contributed_registry.clone()) {
                registration_error.borrow_mut().get_or_insert(error);
            }
            // Sub-agents (L-13): register `task` and build the spawner over the shared `system` — the
            // same `SubAgents::into_spawner` construction path the CLI's `build_agent` and the SDK's
            // `FlowClient::with_sub_agents` use, so a journey delegates through the identical envelope.
            // Children audit into the app's own event store by default (A-08), correlated per spawn.
            let spawner = sub_agents.map(|sa| {
                if let Err(error) =
                    registry.try_register_from("app sub-agent task operation", Arc::new(TaskTool))
                {
                    registration_error.borrow_mut().get_or_insert(error);
                }
                sa.with_audit(events.clone()).into_spawner(system.clone())
            });
            // C-183: resolve `[tools] disable` against the now-FULLY-assembled registry (built-ins +
            // cognition + orchestration ops + the surface's contributed catalog + `task`) — the same
            // `ToolRegistry::resolve_disabled` C-162 installs on the interactive CLI path, called
            // exactly once here before any journey or agent-target engine exists. Installing it on
            // `execution` (the shared per-run template every derived executor clones) is what makes
            // BOTH plain journeys (`build_executor`) and lazily-built per-agent engines
            // (`agent_engine`/`build_agent_engine`) enforce the identical set without a second
            // resolution.
            let resolved_disabled = registry.resolve_disabled(&disabled);
            for pattern in &resolved_disabled.unmatched {
                eprintln!("(warning: [tools] disable entry `{pattern}` matches no known op)");
            }
            let mut execution = environment
                .clone()
                .with_registry(registry.clone())
                .with_disabled_ops(resolved_disabled.disabled);
            if let Some(spawner) = &spawner {
                execution = execution.with_spawner(spawner.clone());
            }
            Engine {
                program,
                registry,
                bus,
                delivery: DeliverySupervisor::new(),
                depth: Arc::new(AtomicU32::new(0)),
                runs: AtomicU64::new(0),
                execution,
                provider,
                default_model: model,
                events,
                agents: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
                host_permissions,
                parks: Mutex::new(Vec::new()),
            }
        });
        if let Some(error) = registration_error.into_inner() {
            Err(error)
        } else {
            Ok(engine)
        }
    }

    fn available_op_names(&self) -> Vec<String> {
        let mut names = self.registry.names();
        names.extend(self.program.ops.iter().map(|op| op.name.clone()));
        names.sort();
        names.dedup();
        names
    }

    fn validate(&self) -> Result<()> {
        let available = self.available_op_names();
        let known: HashSet<String> = available.iter().cloned().collect();
        let registered_tools: HashSet<String> = self.registry.names().into_iter().collect();
        let agent_names: HashSet<&str> = self
            .program
            .agents
            .iter()
            .map(|agent| agent.name.as_str())
            .collect();
        let datasource_names: HashSet<&str> = self
            .program
            .datasources
            .iter()
            .map(|source| source.name.as_str())
            .collect();
        let loop_names: HashSet<&str> = self
            .program
            .agent_loops
            .iter()
            .map(|agent_loop| agent_loop.name.as_str())
            .collect();
        if loop_names.len() != self.program.agent_loops.len() {
            return Err(Error::Other(
                "agent loop declaration names must be unique".into(),
            ));
        }
        let composites: HashMap<String, &[Node]> = self
            .program
            .ops
            .iter()
            .map(|op| (op.name.clone(), op.body.body.as_slice()))
            .collect();

        if let Some(permissions) = &self.program.permissions {
            validate_permission_decl("program permissions", permissions, &known)?;
        }
        for agent in &self.program.agents {
            if let Some(agent_loop) = agent.agent_loop.as_deref() {
                if !loop_names.contains(agent_loop) {
                    return Err(Error::Other(format!(
                        "agent `{}` names unknown agent loop `{agent_loop}`",
                        agent.name
                    )));
                }
            }
            if let Some(permissions) = &agent.permissions {
                validate_permission_decl(
                    &format!("agent `{}` permissions", agent.name),
                    permissions,
                    &known,
                )?;
            }
            let effective = effective_capabilities(
                &available,
                self.program.permissions.as_ref(),
                agent.permissions.as_ref(),
            );
            for tool in &agent.tools {
                if !registered_tools.contains(tool) {
                    return Err(Error::Other(format!(
                        "agent `{}` names unknown tool `{tool}`",
                        agent.name
                    )));
                }
                if effective.declared && !effective.allow.contains(tool) {
                    return Err(Error::Other(format!(
                        "agent `{}` exposes tool `{tool}`, but its effective app/agent capability ceiling denies it",
                        agent.name
                    )));
                }
            }
            for datasource in &agent.datasources {
                if !datasource_names.contains(datasource.as_str()) {
                    return Err(Error::Other(format!(
                        "agent `{}` names unknown datasource `{datasource}`",
                        agent.name
                    )));
                }
            }
        }
        for trigger in &self.program.triggers {
            if let Some(agent) = trigger.agent.as_deref() {
                if !agent_names.contains(agent) {
                    return Err(Error::Other(format!(
                        "trigger `{}` names unknown agent `{agent}`",
                        trigger.name
                    )));
                }
            } else if self.program.flow_named(&trigger.run).is_none() {
                return Err(Error::Other(format!(
                    "trigger `{}` names unknown journey/flow `{}`",
                    trigger.name, trigger.run
                )));
            }
        }

        for journey in &self.program.journeys {
            let agent_permissions = match journey.agent.as_deref() {
                Some(name) => {
                    let agent = self
                        .program
                        .agents
                        .iter()
                        .find(|agent| agent.name == name)
                        .ok_or_else(|| {
                            Error::Other(format!(
                                "journey `{}` names unknown agent `{name}`",
                                journey.name
                            ))
                        })?;
                    agent.permissions.as_ref()
                }
                None => None,
            };
            let effective = effective_capabilities(
                &available,
                self.program.permissions.as_ref(),
                agent_permissions,
            );
            validate_body_calls(
                &journey.flow.body,
                &format!("journey `{}`", journey.name),
                &effective,
                &known,
                &composites,
                &mut HashSet::new(),
            )?;
        }
        let app_capabilities =
            effective_capabilities(&available, self.program.permissions.as_ref(), None);
        for flow in &self.program.flows {
            validate_body_calls(
                &flow.body,
                &format!("flow `{}`", flow.name.as_deref().unwrap_or("<anonymous>")),
                &app_capabilities,
                &known,
                &composites,
                &mut HashSet::new(),
            )?;
        }
        for op in &self.program.ops {
            validate_body_calls(
                &op.body.body,
                &format!("composite op `{}`", op.name),
                &app_capabilities,
                &known,
                &composites,
                &mut HashSet::from([op.name.clone()]),
            )?;
        }
        Ok(())
    }

    /// Run every trigger whose `on` label equals `label`, collecting each journey run.
    ///
    /// A pending ask-park takes precedence: an event that correlates with a parked journey (see
    /// [`crate::park`] for the rule) is **consumed** as that journey's reply — it resumes the park
    /// and does not also route through triggers (otherwise the reply line would start a fresh
    /// journey too). Uncorrelated events route normally and leave every park alone.
    pub(crate) async fn run_triggers(
        &self,
        label: &str,
        payload: &Value,
        sink: &mut dyn AgentSink,
    ) -> Result<Vec<JourneyRun>> {
        if let Some(run) = self.try_resume_ask(label, payload, sink).await? {
            return Ok(vec![run]);
        }
        let mut runs = Vec::new();
        for trigger in self.program.triggers.iter().filter(|t| t.on == label) {
            // An `agent`-bound trigger wakes an agent turn (the model drives RAG + granted tools over
            // the thread's persistent session); otherwise it runs a journey (a fixed DAG), unchanged.
            let run = match trigger.agent.as_deref() {
                Some(agent) => self.run_agent(agent, label, payload).await?,
                None => self.run_journey(&trigger.run, payload, sink).await?,
            };
            runs.push(run);
        }
        Ok(runs)
    }

    async fn journey_runtime_profile(&self, owner: Option<&str>) -> Result<JourneyRuntimeProfile> {
        let available = self.available_op_names();
        match owner {
            Some(name) => {
                let decl = self
                    .program
                    .agents
                    .iter()
                    .find(|agent| agent.name == name)
                    .ok_or_else(|| Error::Other(format!("journey names unknown agent `{name}`")))?;
                let profile = resolve_agent_runtime_profile(
                    decl,
                    self.program.permissions.as_ref(),
                    self.provider.clone(),
                    self.registry.clone(),
                    &available,
                    &self.default_model,
                    self.execution.system().clone(),
                    &self.host_permissions,
                )
                .await?;
                let model = flux_core::canonical_model_spec(
                    self.provider.as_ref().map(|provider| provider.name()),
                    &profile.spec.model,
                );
                Ok(JourneyRuntimeProfile {
                    registry: narrowed_registry(&profile.registry, &profile.capabilities),
                    capabilities: profile.capabilities,
                    model,
                })
            }
            None => {
                let capabilities =
                    effective_capabilities(&available, self.program.permissions.as_ref(), None);
                Ok(JourneyRuntimeProfile {
                    registry: narrowed_registry(&self.registry, &capabilities),
                    capabilities,
                    model: self.default_model_spec(),
                })
            }
        }
    }

    /// Execute one named journey to completion, reusing flux-flow's engine path (full envelope).
    async fn run_journey(
        &self,
        name: &str,
        payload: &Value,
        sink: &mut dyn AgentSink,
    ) -> Result<JourneyRun> {
        let (mut ast, owner) = match self
            .program
            .journeys
            .iter()
            .find(|journey| journey.name == name)
        {
            Some(journey) => (journey.flow.clone(), journey.agent.clone()),
            None => (
                self.program
                    .flows
                    .iter()
                    .find(|flow| flow.name.as_deref() == Some(name))
                    .cloned()
                    .ok_or_else(|| Error::Other(format!("unknown journey `{name}`")))?,
                None,
            ),
        };
        let profile = self.journey_runtime_profile(owner.as_deref()).await?;
        // Lower top-level `ask` calls onto the suspension seam (ask + await) — see `crate::park`.
        ast.body = park::rewrite_asks(std::mem::take(&mut ast.body));

        // Depth guard: increment, ensure we decrement on every exit, then check. The budget belongs
        // to the delivery that caused this run, so two deliveries in flight at once never spend
        // each other's nesting allowance; a run outside any delivery falls back to the engine's.
        let budget = delivery_origin()
            .map(|origin| origin.depth)
            .unwrap_or_else(|| self.depth.clone());
        let prev = budget.fetch_add(1, Ordering::SeqCst);
        let _guard = DepthGuard(budget);
        if prev >= MAX_SPAWN_DEPTH {
            return Err(Error::Other(format!(
                "spawn recursion exceeded max depth {MAX_SPAWN_DEPTH}"
            )));
        }

        // `Arc`: an ask-park keeps the run's store alive (prefix symbols + the persisted
        // suspension) until the reply arrives.
        let store = Arc::new(FlowStore::in_memory().map_err(other)?);
        let session_id = format!("{name}#{}", self.runs.fetch_add(1, Ordering::SeqCst));
        seed_payload(&store, &session_id, payload)?;
        let executor = build_executor(
            profile.registry.clone(),
            &profile.capabilities,
            &self.host_permissions,
            self.execution.clone(),
        )?;
        // Preserve any outer cancellation/reporter while making this journey the immediate parent
        // lineage. The snapshot is scoped around the drive below, never retained on the executor.
        let runtime_turn = executor
            .context()
            .runtime_turn_context()
            .with_session(&session_id);
        analyze_composites(&self.program.ops, &self.registry)
            .map_err(|d| Error::Other(format!("composite ops: {}", join_diags(&d))))?;

        // Where the asked channel is read from if this run parks: the expects-reply sends recorded
        // from here on belong to this segment.
        let sent_before = self.bus.sent().len();
        // C-33: capture this run's own usage rather than accumulating into the caller's (possibly
        // shared/reused, and always type-erased) `sink` — see `UsageCapture`'s doc comment.
        let mut capture = UsageCapture {
            inner: sink,
            usage: None,
        };
        let outcome = scope_runtime_turn(runtime_turn, async {
            if self.program.ops.is_empty() {
                flux_flow::runtime::execute_flow(&store, &executor, &session_id, &ast, &mut capture)
                    .await
            } else {
                flux_flow::runtime::execute_flow_with_composites(
                    &store,
                    &executor,
                    &session_id,
                    &ast,
                    &self.program.ops,
                    &mut capture,
                )
                .await
            }
        })
        .await
        .map_err(other)?;
        let mut usage = capture.usage;
        accumulate_usage(
            &mut usage,
            flux_cognition::recorded_usage(&executor.evidence()),
        );
        let model = profile.model;

        if let Some(parked) = self.park_if_asked(
            name,
            &session_id,
            &store,
            &ast.body,
            &outcome,
            sent_before,
            0,
            usage.clone(),
            model.clone(),
        )? {
            return Ok(parked);
        }

        Ok(JourneyRun {
            journey: name.to_string(),
            result: outcome.result,
            steps: outcome.steps,
            usage,
            model,
        })
    }

    /// The canonical `provider/model` spec of the app's default model (C-33) — the "driving engine
    /// spec" attributed to a plain journey run, which (unlike an `agent`-bound trigger's own
    /// [`FlowEngine`], used in [`Self::run_agent`]) has no per-op engine of its own.
    fn default_model_spec(&self) -> String {
        flux_core::canonical_model_spec(
            self.provider.as_ref().map(|p| p.name()),
            &self.default_model,
        )
    }

    /// Park the run when `outcome` suspended on an ask-lowered `await`: persist the resume point on
    /// the run's own [`FlowStore`] (the same suspension latch the interactive engine uses) and queue
    /// a [`ParkedAsk`] keyed by the asked channel. Returns the parked [`JourneyRun`] (empty result —
    /// the question is already on the channel), or `None` on a normal completion. A suspension from
    /// a hand-written `await` (a foreign `source`) is left untouched — flux-app has no resume
    /// surface for those (unchanged pre-A-11 behavior: the partial result is returned).
    #[allow(clippy::too_many_arguments)]
    fn park_if_asked(
        &self,
        journey: &str,
        session_id: &str,
        store: &Arc<FlowStore>,
        body: &[Node],
        outcome: &FlowOutcome,
        sent_before: usize,
        prior_steps: usize,
        usage: Option<Usage>,
        model: String,
    ) -> Result<Option<JourneyRun>> {
        let Some(susp) = &outcome.suspension else {
            return Ok(None);
        };
        if susp.source != park::ASK_REPLY_SOURCE {
            return Ok(None);
        }
        // The asked channel, from runtime truth: the lowered ask call executes immediately before
        // its `await`, so the segment's most recent expects-reply send is necessarily this ask
        // (a dynamically-computed channel name resolves correctly too).
        let sent = self.bus.sent();
        let Some(channel) = sent
            .get(sent_before..)
            .unwrap_or(&[])
            .iter()
            .rev()
            .find(|m| m.expects_reply)
            .map(|m| m.channel.clone())
        else {
            // Defensive: an ask-marked await with no recorded ask send — nothing to correlate on.
            return Ok(None);
        };
        store
            // Journeys execute their flow unnamed (see `run_journey`), so the park persists no
            // flow name — run and resume must derive the same (hash-only) checkpoint key.
            .save_suspension(session_id, None, body, susp.node, &susp.source)
            .map_err(other)?;
        let steps = prior_steps + outcome.steps;
        self.parks.lock().expect("parks poisoned").push(ParkedAsk {
            channel,
            journey: journey.to_string(),
            session_id: session_id.to_string(),
            store: store.clone(),
            steps,
        });
        Ok(Some(JourneyRun {
            journey: journey.to_string(),
            result: String::new(),
            steps,
            usage,
            model,
        }))
    }

    /// If `label`/`payload` is the reply a parked ask waits for, consume it: remove the oldest
    /// correlated park and resume that journey with the reply text. `None` means the event did not
    /// correlate and should route through triggers as usual.
    async fn try_resume_ask(
        &self,
        label: &str,
        payload: &Value,
        sink: &mut dyn AgentSink,
    ) -> Result<Option<JourneyRun>> {
        let park = {
            let mut parks = self.parks.lock().expect("parks poisoned");
            parks
                .iter()
                .position(|p| park::event_correlates(label, &self.program.channels, &p.channel))
                .map(|i| parks.remove(i))
        };
        let Some(park) = park else {
            return Ok(None);
        };
        let run = self.resume_parked(park, reply_text(payload), sink).await?;
        Ok(Some(run))
    }

    /// Resume a parked journey with `reply` bound as the suspended ask's result. Re-enters through
    /// the NORMAL engine path — `flux_flow::runtime::resume_flow*` over a fresh full-envelope
    /// executor and the park's own store — so permission + approval rules apply to the continuation
    /// exactly as to the original run (no side-channel execution of flow bodies). The continuation
    /// may itself `ask` again, in which case it parks again.
    async fn resume_parked(
        &self,
        park: ParkedAsk,
        reply: String,
        sink: &mut dyn AgentSink,
    ) -> Result<JourneyRun> {
        let ParkedAsk {
            journey,
            session_id,
            store,
            steps: prior_steps,
            ..
        } = park;
        let Some((flow_name, body, node, _source)) =
            store.take_suspension(&session_id).map_err(other)?
        else {
            return Err(Error::Other(format!(
                "parked ask for journey `{journey}` has no persisted suspension"
            )));
        };
        let owner = self
            .program
            .journeys
            .iter()
            .find(|decl| decl.name == journey)
            .and_then(|decl| decl.agent.as_deref());
        let profile = self.journey_runtime_profile(owner).await?;
        let executor = build_executor(
            profile.registry.clone(),
            &profile.capabilities,
            &self.host_permissions,
            self.execution.clone(),
        )?;
        let runtime_turn = executor
            .context()
            .runtime_turn_context()
            .with_session(&session_id);
        analyze_composites(&self.program.ops, &self.registry)
            .map_err(|d| Error::Other(format!("composite ops: {}", join_diags(&d))))?;

        let sent_before = self.bus.sent().len();
        let input = FluxValue::String(reply);
        // C-33: same per-run usage capture as `run_journey` — see `UsageCapture`.
        let mut capture = UsageCapture {
            inner: sink,
            usage: None,
        };
        let outcome = scope_runtime_turn(runtime_turn, async {
            if self.program.ops.is_empty() {
                flux_flow::runtime::resume_flow(
                    &store,
                    &executor,
                    &session_id,
                    &body,
                    node,
                    input,
                    &mut capture,
                )
                .await
            } else {
                flux_flow::runtime::resume_flow_with_composites(
                    &store,
                    &executor,
                    &session_id,
                    flow_name.as_deref(),
                    &body,
                    node,
                    input,
                    &self.program.ops,
                    &mut capture,
                )
                .await
            }
        })
        .await
        .map_err(other)?;
        let mut usage = capture.usage;
        accumulate_usage(
            &mut usage,
            flux_cognition::recorded_usage(&executor.evidence()),
        );
        let model = profile.model;

        if let Some(parked) = self.park_if_asked(
            &journey,
            &session_id,
            &store,
            &body,
            &outcome,
            sent_before,
            prior_steps,
            usage.clone(),
            model.clone(),
        )? {
            return Ok(parked);
        }

        Ok(JourneyRun {
            journey,
            result: outcome.result,
            steps: prior_steps + outcome.steps,
            usage,
            model,
        })
    }

    /// Get (build + cache) the [`FlowEngine`] for a declared agent. Built lazily on first use; an
    /// `agent`-bound trigger naming an undeclared agent, or one with no model provider, is a clear error.
    async fn agent_engine(&self, name: &str) -> Result<Arc<FlowEngine>> {
        // Fast path: a cached engine. Scope the guard so it is dropped before the build await — engine
        // construction reads persona files through the guarded System (async IO), and a std `MutexGuard`
        // must never be held across an `.await`.
        if let Some(engine) = self.agents.lock().expect("agents cache poisoned").get(name) {
            return Ok(engine.clone());
        }
        let decl = self
            .program
            .agents
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| Error::Other(format!("trigger names unknown agent `{name}`")))?;
        let provider = self
            .provider
            .clone()
            .ok_or_else(|| Error::Other(format!("agent `{name}` needs a model provider")))?;
        let agent_loop = decl.agent_loop.as_deref().and_then(|name| {
            self.program
                .agent_loops
                .iter()
                .find(|candidate| candidate.name == name)
                .map(|candidate| candidate.flow.clone())
        });
        let engine = Arc::new(
            build_agent_engine(
                decl,
                self.program.permissions.as_ref(),
                provider,
                self.registry.clone(),
                self.events.clone(),
                &self.default_model,
                self.execution.clone(),
                &self.host_permissions,
                agent_loop,
            )
            .await?,
        );
        // A concurrent caller may have built+inserted the same agent while we were off-lock; keep the
        // first-inserted instance so every thread shares one engine.
        let mut cache = self.agents.lock().expect("agents cache poisoned");
        Ok(cache.entry(name.to_string()).or_insert(engine).clone())
    }

    /// Resolve the persistent session for `(agent, conversation)`: reuse the bound session (multi-turn
    /// thread memory) or mint one. A delivery with no conversation id runs in a fresh one-shot session.
    fn session_for(&self, agent: &str, conversation: Option<&str>) -> Result<String> {
        match conversation {
            Some(conv) => {
                let key = (agent.to_string(), conv.to_string());
                let mut map = self.sessions.lock().expect("sessions map poisoned");
                if let Some(sid) = map.get(&key) {
                    return Ok(sid.clone());
                }
                let sid = self
                    .events
                    .create_session(&self.default_model)
                    .map_err(other)?;
                map.insert(key, sid.clone());
                Ok(sid)
            }
            None => self
                .events
                .create_session(&self.default_model)
                .map_err(other),
        }
    }

    /// Run one agent turn for an `agent`-bound trigger: the model drives RAG + granted tools over the
    /// thread's persistent session, and the assistant's reply becomes the run result (the channel posts
    /// it). The conversation id (a Slack thread ts) binds repeated events to one session.
    async fn run_agent(&self, name: &str, label: &str, payload: &Value) -> Result<JourneyRun> {
        let engine = self.agent_engine(name).await?;
        let conversation = payload
            .get("conversation")
            .or_else(|| payload.get("thread"))
            .and_then(|v| v.as_str());
        let session_id = self.session_for(name, conversation)?;
        // The turn's input: a real user message (a Slack mention's `text`) when present; otherwise
        // synthesize the event context so an event-driven agent (a `startup`/schedule trigger carries no
        // `text`) still wakes to a concrete turn naming the trigger that fired it (flux D-11).
        let input = match payload.get("text").and_then(|v| v.as_str()) {
            Some(t) if !t.trim().is_empty() => t.to_string(),
            _ => event_context(label, payload),
        };
        let mut sink = RecordingSink::default();
        engine
            .run_turn(&session_id, &input, &mut sink)
            .await
            .map_err(other)?;
        // C-33: the engine's own provider + model IS the driving engine spec for this run — unlike a
        // plain journey, an agent-bound trigger has exactly one engine, so there is no aggregation
        // question here.
        let model = flux_core::canonical_model_spec(Some(engine.provider.name()), &engine.model);
        Ok(JourneyRun {
            journey: name.to_string(),
            result: sink.text,
            steps: sink.tools.len(),
            usage: sink.usage,
            model,
        })
    }
}

#[async_trait]
impl JourneyHost for Engine {
    async fn run_journey_for_spawn(&self, name: &str, payload: Value) -> Result<String> {
        let mut sink = RecordingSink::default();
        Ok(self.run_journey(name, &payload, &mut sink).await?.result)
    }
}

/// Apply one journey's source/host permission decisions to the App's shared execution template.
/// System, redactor, spawner, policy, identity, and approver are inherited unchanged; only the
/// narrowed registry and rules differ per journey.
fn build_executor(
    registry: ToolRegistry,
    capabilities: &EffectiveCapabilities,
    host_permissions: &HostPermissionRules,
    environment: ExecutionEnvironment,
) -> Result<Executor> {
    let mut allow: Vec<String> = if capabilities.declared {
        capabilities.grants.clone()
    } else {
        LEGACY_JOURNEY_ALLOW
            .iter()
            .map(|name| (*name).into())
            .collect()
    };
    allow.extend(host_permissions.allow.iter().cloned());
    let mut deny = capabilities.deny.clone();
    deny.extend(host_permissions.deny.iter().cloned());
    let perms = PermissionManager::from_rules(&allow, &deny);
    Ok(environment
        .with_registry(registry)
        .with_permissions(perms)
        .into_executor())
}

/// Map a program-level [`AgentDecl`] to an [`AgentSpec`]. Without source-declared permissions its
/// `tools` retain the legacy dual role of visible subset + grants; the shared runtime-profile resolver
/// later intersects them with any app/agent ceiling. The persona is the
/// `description` (or a `settings.system_prompt` string), followed by the contents of any
/// `settings.system_prompt_files` paths — read through the guarded, workspace-confined `system` so a
/// declarative bot can keep a long persona in `bot/PERSONA.md` instead of inlining it (flux D-11). A
/// non-string entry or an unreadable path is a clean error. `model` falls back to the host default.
/// Resolve a served/agentic agent's compaction threshold (A-22). A `run_agent` target binds its
/// conversation to ONE persistent session (`session_for`), so without a working threshold it
/// re-sends the whole growing transcript every turn — linear cost, then a hard provider
/// context-window error. Precedence: per-agent (`settings.compact_threshold_chars`) > env
/// (`FLUX_COMPACT_CHARS`, the same knob the CLI honours) > the sane non-zero
/// [`DEFAULT_COMPACT_THRESHOLD_CHARS`]. A per-agent `0` disables compaction explicitly.
fn compact_threshold_for_decl(decl: &AgentDecl) -> usize {
    decl.settings
        .get("compact_threshold_chars")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize)
        .or_else(|| {
            std::env::var("FLUX_COMPACT_CHARS")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(DEFAULT_COMPACT_THRESHOLD_CHARS)
}

async fn agent_spec_from_decl(
    decl: &AgentDecl,
    default_model: &str,
    cwd: PathBuf,
    system: &System,
) -> Result<AgentSpec> {
    let thinking = match decl.settings.get("thinking") {
        Some(value) => value.as_bool().ok_or_else(|| {
            Error::Other(format!(
                "agent `{}`: settings.thinking must be a boolean",
                decl.name
            ))
        })?,
        None => false,
    };
    let effort = match decl.settings.get("effort") {
        Some(value) => Some(
            serde_json::from_value::<Effort>(value.clone()).map_err(|_| {
                Error::Other(format!(
                    "agent `{}`: settings.effort must be low, medium, high, xhigh, or max",
                    decl.name
                ))
            })?,
        ),
        None => None,
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(base) = decl.description.clone().or_else(|| {
        decl.settings
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }) {
        parts.push(base);
    }
    if let Some(files) = decl
        .settings
        .get("system_prompt_files")
        .and_then(|v| v.as_array())
    {
        for f in files {
            let path = f.as_str().ok_or_else(|| {
                Error::Other(format!(
                    "agent `{}`: settings.system_prompt_files entries must be strings",
                    decl.name
                ))
            })?;
            let text = system.read_file(path).await.map_err(|e| {
                Error::Other(format!(
                    "agent `{}`: read system_prompt_files `{path}`: {e}",
                    decl.name
                ))
            })?;
            parts.push(text);
        }
    }
    if !decl.datasources.is_empty() {
        let sources = decl
            .datasources
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "Knowledge access: this agent may query only the declared datasource(s) {sources}. \
             Before answering a question that depends on this knowledge, call `search` (or another \
             granted retrieval operation) and ground the answer in the returned records. If no \
             relevant record is found, say that the declared datasource does not contain the answer."
        ));
    }
    let system_prompt = if parts.is_empty() {
        AgentSpec::default().system_prompt
    } else {
        parts.join("\n\n")
    };
    // Inline knowledge blocks declared in `settings.context` (A-19) — injected into the system prompt as
    // `<knowledge-base>` sections. A non-list or malformed entry is a clean error.
    let context: Vec<flux_core::ContextBlock> = match decl.settings.get("context") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            Error::Other(format!(
                "agent `{}`: settings.context must be a list of {{id,title,body}} blocks: {e}",
                decl.name
            ))
        })?,
        None => Vec::new(),
    };
    Ok(AgentSpec {
        model: decl
            .model
            .clone()
            .unwrap_or_else(|| default_model.to_string()),
        system_prompt,
        tools: Some(decl.tools.clone()),
        permissions: Permissions {
            allow: decl.tools.clone(),
            deny: Vec::new(),
        },
        cwd,
        context,
        compact_threshold_chars: compact_threshold_for_decl(decl),
        thinking,
        effort,
        ..AgentSpec::default()
    })
}

/// Retrieval op names whose `source` argument is governed by [`AgentDecl::datasources`]. The app's
/// shared registry may serve many program sources, but an agent engine receives a wrapper around
/// each of these ops so its declaration is a real capability boundary rather than prompt-only text.
const DATASOURCE_OPS: &[&str] = &["search", "get", "list", "relation", "batch_get", "sources"];

struct DatasourceScopedTool {
    inner: Arc<dyn Tool>,
    allowed: Vec<String>,
}

impl DatasourceScopedTool {
    fn scoped_params(&self, mut params: Value) -> Result<Value> {
        if self.inner.spec().name == "sources" {
            return Ok(params);
        }
        let obj = params.as_object_mut().ok_or_else(|| {
            Error::Other(format!(
                "{}: input must be an object",
                self.inner.spec().name
            ))
        })?;
        match obj.get("source") {
            Some(Value::String(source)) if self.allowed.contains(source) => Ok(params),
            Some(Value::String(source)) => Err(Error::Other(format!(
                "{}: datasource `{source}` is not declared for this agent (allowed: {})",
                self.inner.spec().name,
                self.allowed_display()
            ))),
            Some(_) => Err(Error::Other(format!(
                "{}: `source` must be a string",
                self.inner.spec().name
            ))),
            None => match self.allowed.as_slice() {
                [only] => {
                    obj.insert("source".to_string(), Value::String(only.clone()));
                    Ok(params)
                }
                [] => Err(Error::Other(format!(
                    "{}: this agent declares no datasources",
                    self.inner.spec().name
                ))),
                _ => Err(Error::Other(format!(
                    "{}: choose a declared `source` ({})",
                    self.inner.spec().name,
                    self.allowed_display()
                ))),
            },
        }
    }

    fn allowed_display(&self) -> String {
        if self.allowed.is_empty() {
            "none".to_string()
        } else {
            self.allowed.join(", ")
        }
    }

    fn filter_sources(&self, text: &str) -> String {
        text.lines()
            .filter(|line| {
                self.allowed
                    .iter()
                    .any(|source| *line == source || line.starts_with(&format!("{source} (")))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait]
impl Tool for DatasourceScopedTool {
    fn spec(&self) -> flux_spec::ToolSpec {
        let mut spec = self.inner.spec();
        if spec.name == "sources" {
            spec.description = format!(
                "List the datasources declared for this agent ({}).",
                self.allowed_display()
            );
            return spec;
        }
        spec.description.push_str(&format!(
            " This agent is limited to source(s): {}.",
            self.allowed_display()
        ));
        if let Some(source) = spec
            .input_schema
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .and_then(|properties| properties.get_mut("source"))
            .and_then(Value::as_object_mut)
        {
            source.insert("enum".to_string(), json!(self.allowed));
            if let [only] = self.allowed.as_slice() {
                source.insert("default".to_string(), Value::String(only.clone()));
            }
        }
        spec
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        self.scoped_params(params.clone())
            .map(|scoped| self.inner.permission_subjects(&scoped))
            .unwrap_or_else(|_| self.inner.permission_subjects(params))
    }

    fn intents(&self, params: &Value) -> flux_spec::IntentSet {
        self.scoped_params(params.clone())
            .map(|scoped| self.inner.intents(&scoped))
            .unwrap_or_else(|_| self.inner.intents(params))
    }

    fn semantic_effects(&self) -> Vec<String> {
        self.inner.semantic_effects()
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        if self.inner.spec().name == "sources" {
            if self.allowed.is_empty() {
                return Ok(ToolResult::ok("no sources"));
            }
            let mut result = self.inner.execute(ctx, params).await?;
            result.content = self.filter_sources(&result.content);
            if result.content.is_empty() && !result.is_error {
                result.content = "no sources".to_string();
            }
            result.view = result.view.as_deref().map(|view| self.filter_sources(view));
            return Ok(result);
        }
        self.inner.execute(ctx, self.scoped_params(params)?).await
    }
}

fn scope_datasource_tools(registry: &mut ToolRegistry, allowed: &[String]) -> Result<()> {
    let mut scoped = registry.clone();
    for name in DATASOURCE_OPS {
        if let Some(inner) = scoped.get(name) {
            scoped.replace_from(
                "flux-app declared datasource-scope adapter",
                Arc::new(DatasourceScopedTool {
                    inner,
                    allowed: allowed.to_vec(),
                }),
            )?;
        }
    }
    *registry = scoped;
    Ok(())
}

/// Assemble an agent-target [`FlowEngine`] from a declaration: a guarded [`System`] rooted at the cwd, the
/// host's op registry (subset to the agent's tools), the spec's grants, and a headless [`DenyApprover`] —
/// so the agent runs only its granted ops with no human at a prompt.
#[allow(clippy::too_many_arguments)]
async fn build_agent_engine(
    decl: &AgentDecl,
    app_permissions: Option<&PermissionDecl>,
    provider: Arc<dyn Provider>,
    registry: ToolRegistry,
    events: Arc<EventStore>,
    default_model: &str,
    environment: ExecutionEnvironment,
    host_permissions: &HostPermissionRules,
    agent_loop: Option<flux_lang::ast::DraftAst>,
) -> Result<FlowEngine> {
    let system = environment.system().clone();
    let available = registry.names();
    let mut profile = resolve_agent_runtime_profile(
        decl,
        app_permissions,
        Some(provider.clone()),
        registry,
        &available,
        default_model,
        system.clone(),
        host_permissions,
    )
    .await?;
    if let Some(agent_loop) = agent_loop {
        profile.spec.agent_loop = flux_agent::AgentLoopSpec::Flux(agent_loop);
    }
    // Adaptive model stages read the turn's conversation via the FlowStore (`store.conversation()`),
    // which delegates to the FlowStore's *internal* event log. Back it with the SAME `events` store the
    // engine records the user message into — otherwise `in_memory()` mints a fresh, empty EventStore and
    // the stages see no conversation, so the model only ever gets the system prompt (never the user's
    // message). This is what makes an `agent`-bound trigger actually answer the inbound mention.
    let flow = FlowStore::in_memory_with_events(events.clone()).map_err(other)?;
    let environment = environment
        .with_registry(profile.registry)
        .with_approver(Arc::new(DenyApprover));
    profile
        .spec
        .assemble_in(provider, environment, events, flow)
        .map_err(other)
}

/// Synthesize a turn input for an `agent`-bound trigger whose event carries no user `text` (a `startup`
/// or a schedule tick, vs. a Slack mention). The agent's system prompt says what to do per event; this
/// hands it a concrete turn naming the trigger that woke it, plus any payload fields (e.g. a tick's
/// `at`), so an event-driven agent acts instead of waking to an empty prompt (flux D-11).
fn event_context(label: &str, payload: &Value) -> String {
    let mut s = format!("You were woken by the `{label}` trigger (event `{label}`).");
    if let Some(obj) = payload.as_object() {
        let fields: Vec<String> = obj
            .iter()
            .filter(|(k, _)| k.as_str() != "text")
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        if !fields.is_empty() {
            s.push_str(&format!(" Event data: {}.", fields.join(", ")));
        }
    }
    s.push_str(" Act according to your instructions for this event.");
    s
}

fn join_diags(diags: &[flux_lang::analyze::Diagnostic]) -> String {
    diags
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Seed an event's payload into the journey's session so the flow can read it: the whole payload binds
/// to `$input`, and each top-level field binds to its own symbol (so a journey body can interpolate
/// `{text}` or reference `$text` directly).
fn seed_payload(store: &FlowStore, session_id: &str, payload: &Value) -> Result<()> {
    bind_symbol(store, session_id, "input", payload)?;
    if let Some(obj) = payload.as_object() {
        for (key, value) in obj {
            bind_symbol(store, session_id, key, value)?;
        }
    }
    Ok(())
}

fn bind_symbol(store: &FlowStore, session_id: &str, name: &str, value: &Value) -> Result<()> {
    let flux_value = FluxValue::from_json(value);
    let value_id = store.put_value(session_id, &flux_value).map_err(other)?;
    store
        .bind(
            session_id,
            &SymbolName(name.to_string()),
            &value_id,
            None,
            &summarize(value),
            Visibility::Visible,
        )
        .map_err(other)?;
    Ok(())
}

/// A short human summary of a seeded value (the raw string for a string; compact JSON otherwise),
/// capped so a large payload doesn't bloat the session view.
fn summarize(value: &Value) -> String {
    let s = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if s.len() > 200 {
        format!("{}…", &s[..200])
    } else {
        s
    }
}

/// The reply text an inbound event carries for a parked ask: the `text` field when present (the
/// shape every channel delivers — the stdin loop, Slack, webhooks), a bare string payload verbatim,
/// otherwise the payload's compact JSON.
fn reply_text(payload: &Value) -> String {
    match payload {
        Value::String(s) => s.clone(),
        v => v
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string()),
    }
}

/// Map any foreign error onto [`flux_core::Error::Other`].
fn other(e: impl std::fmt::Display) -> Error {
    Error::Other(e.to_string())
}

/// Decrements the active spawn depth when a journey run unwinds (success, error, or early return).
struct DepthGuard(Arc<AtomicU32>);

impl Drop for DepthGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A minimal [`AgentSink`] that records streamed text and the op names dispatched. The journey's
/// canonical result is taken from the `FlowOutcome`, so this only needs to capture for inspection.
///
/// C-33: also accumulates every `turn_end`'s [`Usage`] (summed field-by-field — a run may drive
/// more than one model call, e.g. an `agent`-bound trigger's turn), so [`Engine::run_agent`] can
/// attribute the run's real cost onto its [`JourneyRun`].
#[derive(Default)]
pub struct RecordingSink {
    pub text: String,
    pub tools: Vec<String>,
    pub usage: Option<Usage>,
}

impl AgentSink for RecordingSink {
    fn text_delta(&mut self, text: &str) {
        self.text.push_str(text);
    }
    fn tool_call(&mut self, name: &str, _input: &Value) {
        self.tools.push(name.to_string());
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        accumulate_usage(&mut self.usage, usage);
    }
}

/// Fold `next` into `acc` field-by-field (C-33): `None` on either side leaves the other untouched,
/// `Some` on both sums every counter, including `reported_cost_usd` — a run may drive more than one
/// priced model call and the running total must stay additive, not last-write-wins.
fn accumulate_usage(acc: &mut Option<Usage>, next: Option<Usage>) {
    let Some(next) = next else { return };
    match acc {
        Some(acc) => acc.sum_independent(&next),
        None => *acc = Some(next),
    }
}

/// Wraps a caller-supplied [`AgentSink`] to attribute [`Usage`] to exactly the run this wrapper
/// spans (C-33), while forwarding every event through unchanged. `run_journey`/`resume_parked`
/// receive a `&mut dyn AgentSink` that may be shared/reused across several journey runs in one
/// [`Engine::deliver`] call — reading a concrete field off a trait object isn't possible, and
/// summing directly into the shared sink would misattribute one journey's cost to the next. A
/// fresh wrapper per run gives each [`JourneyRun`] its own accurate total without changing what the
/// outer sink observes.
struct UsageCapture<'a> {
    inner: &'a mut dyn AgentSink,
    usage: Option<Usage>,
}

impl AgentSink for UsageCapture<'_> {
    fn text_delta(&mut self, text: &str) {
        self.inner.text_delta(text);
    }
    fn thinking_delta(&mut self, text: &str) {
        self.inner.thinking_delta(text);
    }
    fn planning(&mut self, active: bool) {
        self.inner.planning(active);
    }
    fn tool_call(&mut self, name: &str, input: &Value) {
        self.inner.tool_call(name, input);
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        self.inner.tool_result(name, result);
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.inner.observation(o);
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        self.inner.turn_end(usage.clone());
        accumulate_usage(&mut self.usage, usage);
    }
}

#[cfg(test)]
mod agent_target_tests {
    use super::*;
    use async_trait::async_trait;
    use flux_core::{Chunk, ContentBlock, Role, StopReason};
    use flux_lang::program::Module;
    use flux_provider::{ChunkStream, Request};

    fn intent_or_reply(req: &Request, reply: String, usage: Option<Usage>) -> Vec<Chunk> {
        let mut chunks = if req.tools.len() == 1 && req.tools[0].name == "declare_intent" {
            vec![Chunk::Block(ContentBlock::ToolUse {
                id: "intent-1".into(),
                name: "declare_intent".into(),
                input: json!({
                    "intent": "answer the current message",
                    "capability_families": []
                }),
            })]
        } else {
            vec![Chunk::TextDelta(reply)]
        };
        if let Some(usage) = usage {
            chunks.push(Chunk::Usage(usage));
        }
        chunks.push(Chunk::Done {
            stop_reason: Some(
                if req.tools.len() == 1 && req.tools[0].name == "declare_intent" {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                },
            ),
        });
        chunks
    }

    /// A provider that follows the adaptive intent protocol and then answers with fixed prose —
    /// enough to drive an agent turn hermetically (no network, no real model).
    struct ReplyProvider {
        reply: String,
    }
    #[async_trait]
    impl Provider for ReplyProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let chunks = intent_or_reply(&req, self.reply.clone(), None);
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// Like [`ReplyProvider`] but also reports real token usage (a `Chunk::Usage` before `Done`) —
    /// drives an agent turn whose `turn_end` usage is `Some`, so C-33's `JourneyRun::usage`/`::model`
    /// wiring can be exercised hermetically (no network, no real model).
    struct ReplyWithUsageProvider {
        reply: String,
    }
    #[async_trait]
    impl Provider for ReplyWithUsageProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            let chunks = intent_or_reply(
                &req,
                self.reply.clone(),
                Some(Usage {
                    input_tokens: 100,
                    output_tokens: 40,
                    ..Default::default()
                }),
            );
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// A single-shot cognition provider: unlike the adaptive fixtures above, every request is the
    /// model-backed journey op itself and returns a fixed structured value plus usage.
    struct CognitionUsageProvider(Usage);

    #[async_trait]
    impl Provider for CognitionUsageProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(Chunk::TextDelta("[]".into())),
                Ok(Chunk::Usage(self.0.clone())),
                Ok(Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                }),
            ])))
        }
    }

    struct CognitionErrorProvider(Usage);

    #[async_trait]
    impl Provider for CognitionErrorProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(Chunk::Usage(self.0.clone())),
                Err(Error::Provider("declared cognition failure".into())),
            ])))
        }
    }

    struct PendingCognitionStream {
        usage: Option<Usage>,
        pending: Arc<tokio::sync::Notify>,
    }

    impl futures::Stream for PendingCognitionStream {
        type Item = Result<Chunk>;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            if let Some(usage) = self.usage.take() {
                return std::task::Poll::Ready(Some(Ok(Chunk::Usage(usage))));
            }
            self.pending.notify_one();
            std::task::Poll::Pending
        }
    }

    struct PendingCognitionProvider {
        usage: Usage,
        pending: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl Provider for PendingCognitionProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Ok(Box::pin(PendingCognitionStream {
                usage: Some(self.usage.clone()),
                pending: self.pending.clone(),
            }))
        }
    }

    async fn cognition_engine(provider: Arc<dyn Provider>, events: Arc<EventStore>) -> FlowEngine {
        let decl = AgentDecl {
            name: "worker".into(),
            model: Some("test-model".into()),
            agent_loop: None,
            tools: vec!["ai.extract".into()],
            datasources: Vec::new(),
            description: Some("extract facts".into()),
            permissions: None,
            settings: Value::Null,
        };
        let environment = ExecutionEnvironment::new(
            Arc::new(System::new(Workspace::new(".").unwrap())),
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            ExecutionAuthorization::local(),
        );
        build_agent_engine(
            &decl,
            None,
            provider,
            ToolRegistry::new(),
            events,
            "test-model",
            environment,
            &HostPermissionRules::default(),
            None,
        )
        .await
        .unwrap()
    }

    /// Adaptive child provider that selects `ai.extract` once, then finishes from its tool result.
    /// The actual cognition call is served by [`CognitionUsageProvider`] registered in the child's
    /// base catalog, keeping planner usage at zero so the child total isolates the nested call.
    struct CognitionPlanProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl Provider for CognitionPlanProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            if req.tools.len() == 1 && req.tools[0].name == "declare_intent" {
                return Ok(Box::pin(futures::stream::iter(vec![
                    Ok(Chunk::Block(ContentBlock::ToolUse {
                        id: "intent-1".into(),
                        name: "declare_intent".into(),
                        input: json!({
                            "intent": "extract facts",
                            "capability_families": ["model"]
                        }),
                    })),
                    Ok(Chunk::Done {
                        stop_reason: Some(StopReason::ToolUse),
                    }),
                ])));
            }

            let call = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if call == 0 {
                let native_name = req
                    .tools
                    .iter()
                    .find(|tool| tool.description.contains("Flux operation `ai.extract`"))
                    .map(|tool| tool.name.clone())
                    .ok_or_else(|| Error::Other("ai.extract was not surfaced to child".into()))?;
                return Ok(Box::pin(futures::stream::iter(vec![
                    Ok(Chunk::Block(ContentBlock::ToolUse {
                        id: "extract-1".into(),
                        name: native_name,
                        input: json!({ "from": "Alice" }),
                    })),
                    Ok(Chunk::Done {
                        stop_reason: Some(StopReason::ToolUse),
                    }),
                ])));
            }

            if call == 1 {
                return Ok(Box::pin(futures::stream::iter(vec![
                    Ok(Chunk::Block(ContentBlock::ToolUse {
                        id: "finalize-1".into(),
                        name: "finalize_plan".into(),
                        input: json!({ "instructions": "Report the extracted facts." }),
                    })),
                    Ok(Chunk::Done {
                        stop_reason: Some(StopReason::ToolUse),
                    }),
                ])));
            }

            Ok(Box::pin(futures::stream::iter(vec![
                Ok(Chunk::TextDelta("child done".into())),
                Ok(Chunk::Block(ContentBlock::Text {
                    text: "child done".into(),
                })),
                Ok(Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                }),
            ])))
        }
    }

    /// A provider that echoes the latest user message straight back, so a test can observe the exact
    /// turn input the engine fed the model.
    struct EchoProvider;
    #[async_trait]
    impl Provider for EchoProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            if req.tools.len() == 1 && req.tools[0].name == "declare_intent" {
                let chunks = intent_or_reply(&req, String::new(), None);
                return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
            }
            let echo = req
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::User))
                .map(|m| m.text())
                .unwrap_or_default();
            let chunks = vec![
                Chunk::TextDelta(echo),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ];
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    fn program(src: &str) -> Program {
        match Module::parse_str(src).expect("parse program") {
            Module::Program(p) => p,
            Module::Flow(_) => panic!("expected a program, got a bare flow"),
        }
    }

    #[test]
    fn validating_app_constructor_returns_extra_tool_collision() {
        let duplicate = flux_runtime::tool_fn(
            flux_spec::ToolSpec::read_only("read", "shadow read", json!({"type": "object"})),
            |_input| async { Ok(Value::Null) },
        );
        let error = App::try_with_tools(Program::default(), None, "mock", false, vec![duplicate])
            .err()
            .expect("extra tool must not replace a built-in")
            .to_string();

        assert!(error.contains("duplicate operation `read`"));
        assert!(error.contains("app extra tool #1"));
    }

    /// An app with one agent reachable via an `agent`-bound `slack` trigger.
    fn app_with_agent(reply: &str) -> App {
        let src = "\
agent assistant
  description \"be terse\"
  tools []

trigger t1
  on \"slack\"
  run _
  agent assistant
";
        let provider: Arc<dyn Provider> = Arc::new(ReplyProvider {
            reply: reply.to_string(),
        });
        App::with_options(program(src), Some(provider), "mock", false)
    }

    /// The C-13 wiring: the redactor `resolve_secrets` seeded is the SAME one every journey-run
    /// executor redacts with — a tool result leaking a resolved secret comes back scrubbed.
    #[tokio::test]
    async fn journey_executor_scrubs_resolved_secrets_from_tool_output() {
        // A fixture named `search` (inside build_executor's pre-allowed safe set) that leaks a secret.
        struct LeakyTool;
        #[async_trait]
        impl Tool for LeakyTool {
            fn spec(&self) -> flux_spec::ToolSpec {
                flux_spec::ToolSpec::read_only("search", "leaks", json!({"type": "object"}))
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _params: Value,
            ) -> Result<flux_runtime::ToolResult> {
                Ok(flux_runtime::ToolResult::ok(
                    "found: xoxb-app-secret-987".to_string(),
                ))
            }
        }
        let redactor = Redactor::new();
        redactor.add_secret("xoxb-app-secret-987"); // what resolve_secrets does at load
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(LeakyTool));
        let system = Arc::new(System::new(Workspace::new(".").unwrap()));
        let capabilities = EffectiveCapabilities {
            declared: false,
            allow: Vec::new(),
            grants: Vec::new(),
            deny: Vec::new(),
        };
        let executor = build_executor(
            registry,
            &capabilities,
            &HostPermissionRules::default(),
            ExecutionEnvironment::new(
                system,
                ToolRegistry::new(),
                PermissionManager::new(),
                Arc::new(DenyApprover),
                ExecutionAuthorization::local(),
            )
            .with_redactor(redactor),
        )
        .unwrap();
        let r = executor.dispatch("search", json!({})).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            !r.content.contains("xoxb-app-secret-987"),
            "the resolved secret must be scrubbed from tool output: {}",
            r.content
        );
    }

    /// C-60: `--yes` changes only the approval posture. An explicit authorization denial remains a
    /// hard floor on the App journey executor and the tool never runs.
    #[tokio::test]
    async fn app_auto_approval_cannot_widen_the_authorization_floor() {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool_hits = hits.clone();
        let mut registry = ToolRegistry::new();
        registry.register(flux_runtime::tool_fn(
            flux_spec::ToolSpec::read_only("search", "probe", json!({"type": "object"}))
                .with_access(vec![flux_spec::AccessKind::Filesystem]),
            move |_input| {
                let hits = tool_hits.clone();
                async move {
                    hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(json!("ran"))
                }
            },
        ));
        let capabilities = EffectiveCapabilities {
            declared: false,
            allow: Vec::new(),
            grants: Vec::new(),
            deny: Vec::new(),
        };
        let (caller, trust) = flux_policy::local_identity("app-test");
        let environment = ExecutionEnvironment::new(
            Arc::new(System::new(Workspace::new(".").unwrap())),
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(AllowApprover),
            ExecutionAuthorization::new(flux_policy::AuthorizationPolicy::default(), caller, trust),
        );
        let executor = build_executor(
            registry,
            &capabilities,
            &HostPermissionRules::default(),
            environment,
        )
        .unwrap();

        let result = executor.dispatch("search", json!({})).await;
        assert!(result.is_error && result.content.contains("denied by policy"));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// D-65: a surface (e.g. the `flux app run` CLI path) builds its OWN `EventStore` up front so its
    /// plugin/endpoint wiring can install audit hooks against a stream minted from it, THEN hands that
    /// same store to `App::with_events`. The seam must keep using exactly that store — never silently
    /// swap in a fresh internal one, which would disconnect the wiring's audit trail from the app.
    #[tokio::test]
    async fn with_events_shares_the_given_store_not_a_fresh_one() {
        let src = "\
trigger t1
  on \"ping\"
  run pong

journey pong
  flow
    return \"pong!\"
";
        let events = Arc::new(EventStore::in_memory().unwrap());
        let app = App::with_events(
            program(src),
            None,
            "mock",
            false,
            Vec::new(),
            None,
            Redactor::new(),
            events.clone(),
        );
        assert!(
            Arc::ptr_eq(&app.events(), &events),
            "App::with_events must keep the caller's store, not build its own"
        );
        // The seam doesn't disturb ordinary journey behavior.
        let runs = app.deliver("ping", json!({})).await.expect("deliver");
        assert_eq!(runs[0].result, "pong!");
    }

    #[tokio::test]
    async fn agent_spec_maps_tools_to_grants_and_persona() {
        let decl = AgentDecl {
            name: "a".into(),
            model: None,
            agent_loop: None,
            tools: vec!["search".into(), "now".into()],
            datasources: vec!["handbook".into()],
            description: Some("be terse".into()),
            permissions: None,
            settings: Value::Null,
        };
        let system = System::new(Workspace::new(".").unwrap());
        let spec = agent_spec_from_decl(&decl, "host-model", PathBuf::from("."), &system)
            .await
            .unwrap();
        assert_eq!(spec.model, "host-model"); // falls back to the host default
        assert!(spec.system_prompt.starts_with("be terse"));
        assert!(
            spec.system_prompt.contains("handbook") && spec.system_prompt.contains("search"),
            "declared knowledge must be visible in the model framing: {}",
            spec.system_prompt
        );
        // tools are the visible subset AND the pre-allow grants — under DenyApprover only these run.
        assert_eq!(
            spec.tools.as_deref(),
            Some(&["search".to_string(), "now".to_string()][..])
        );
        assert_eq!(
            spec.permissions.allow,
            vec!["search".to_string(), "now".to_string()]
        );
        assert!(spec.permissions.deny.is_empty());
    }

    /// An app agent's `datasources` list is an actual capability boundary. With one declared source,
    /// an unscoped search is pinned to it automatically; an explicit attempt to query another source
    /// is rejected before the underlying retrieval tool runs.
    #[tokio::test]
    async fn agent_datasource_scope_injects_and_enforces_source() {
        struct EchoSearch;
        #[async_trait]
        impl Tool for EchoSearch {
            fn spec(&self) -> flux_spec::ToolSpec {
                flux_spec::ToolSpec::read_only(
                    "search",
                    "echo search input",
                    json!({
                        "type": "object",
                        "properties": {
                            "query": {"type": "string"},
                            "source": {"type": "string"}
                        },
                        "required": ["query"]
                    }),
                )
            }

            async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
                Ok(ToolResult::ok(params.to_string()))
            }
        }

        struct EchoSources;
        #[async_trait]
        impl Tool for EchoSources {
            fn spec(&self) -> flux_spec::ToolSpec {
                flux_spec::ToolSpec::read_only(
                    "sources",
                    "list sources",
                    json!({"type": "object", "properties": {}}),
                )
            }

            async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
                Ok(ToolResult::ok(
                    "handbook (2 records; entities: file.document)\nprivate-notes (1 record; entities: file.document)",
                ))
            }
        }

        let decl = AgentDecl {
            name: "guide".into(),
            model: None,
            agent_loop: None,
            tools: vec!["search".into(), "sources".into()],
            datasources: vec!["handbook".into()],
            description: Some("answer from docs".into()),
            permissions: None,
            settings: Value::Null,
        };
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoSearch));
        registry.register(Arc::new(EchoSources));
        let environment = ExecutionEnvironment::new(
            Arc::new(System::new(Workspace::new(".").unwrap())),
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            ExecutionAuthorization::local(),
        );
        let engine = build_agent_engine(
            &decl,
            None,
            Arc::new(ReplyProvider { reply: "ok".into() }),
            registry,
            Arc::new(EventStore::in_memory().unwrap()),
            "mock",
            environment,
            &HostPermissionRules::default(),
            None,
        )
        .await
        .unwrap();

        let scoped = engine
            .executor
            .dispatch("search", json!({"query": "support hours"}))
            .await;
        assert!(!scoped.is_error, "{}", scoped.content);
        let input: Value = serde_json::from_str(&scoped.content).unwrap();
        assert_eq!(input["source"], "handbook", "input was not source-scoped");

        let denied = engine
            .executor
            .dispatch(
                "search",
                json!({"query": "secrets", "source": "private-notes"}),
            )
            .await;
        assert!(
            denied.is_error,
            "undeclared source unexpectedly ran: {denied:?}"
        );
        assert!(denied.content.contains("private-notes"));
        assert!(denied.content.contains("handbook"));

        let sources = engine.executor.dispatch("sources", json!({})).await;
        assert!(!sources.is_error, "{}", sources.content);
        assert!(sources.content.contains("handbook (2 records"));
        assert!(
            !sources.content.contains("private-notes"),
            "undeclared source leaked through sources: {}",
            sources.content
        );
    }

    /// A-22: a served/agentic agent target gets a NON-ZERO compaction threshold by default (so its
    /// persistent-session conversation is bounded), and `settings.compact_threshold_chars` overrides
    /// it per-agent — including an explicit `0` to disable compaction. Before the fix every non-CLI
    /// construction used `AgentSpec::default()`'s `0`, so `maybe_compact` was a no-op and the
    /// transcript grew until the provider context window blew.
    #[tokio::test]
    async fn agent_spec_has_nonzero_compaction_default_and_per_agent_override() {
        let system = System::new(Workspace::new(".").unwrap());
        let base = AgentDecl {
            name: "a".into(),
            model: None,
            agent_loop: None,
            tools: vec![],
            datasources: vec![],
            description: Some("be terse".into()),
            permissions: None,
            settings: Value::Null,
        };

        // No per-agent setting → a sane non-zero default (the served agent compacts).
        let spec = agent_spec_from_decl(&base, "m", PathBuf::from("."), &system)
            .await
            .unwrap();
        assert!(
            spec.compact_threshold_chars > 0,
            "a served agent must compact by default, got {}",
            spec.compact_threshold_chars
        );

        // Per-agent override wins.
        let tuned = AgentDecl {
            settings: json!({ "compact_threshold_chars": 9999 }),
            ..base.clone()
        };
        let spec = agent_spec_from_decl(&tuned, "m", PathBuf::from("."), &system)
            .await
            .unwrap();
        assert_eq!(spec.compact_threshold_chars, 9999);

        // …including disabling compaction explicitly.
        let disabled = AgentDecl {
            settings: json!({ "compact_threshold_chars": 0 }),
            ..base.clone()
        };
        let spec = agent_spec_from_decl(&disabled, "m", PathBuf::from("."), &system)
            .await
            .unwrap();
        assert_eq!(spec.compact_threshold_chars, 0);
    }

    #[tokio::test]
    async fn agent_spec_parses_reasoning_settings() {
        let system = System::new(Workspace::new(".").unwrap());
        let decl = AgentDecl {
            name: "reasoner".into(),
            model: None,
            agent_loop: None,
            tools: vec![],
            datasources: vec![],
            description: None,
            permissions: None,
            settings: json!({ "thinking": true, "effort": "high" }),
        };
        let spec = agent_spec_from_decl(&decl, "m", PathBuf::from("."), &system)
            .await
            .unwrap();
        assert!(spec.thinking);
        assert_eq!(spec.effort, Some(Effort::High));

        let bad = AgentDecl {
            settings: json!({ "effort": "maximum-ish" }),
            ..decl
        };
        let error = agent_spec_from_decl(&bad, "m", PathBuf::from("."), &system)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("settings.effort"));
    }

    #[tokio::test]
    async fn agent_spec_injects_inline_context_blocks() {
        // A-19: `settings.context` blocks are injected into the effective system prompt as
        // <knowledge-base> sections (grounded-knowledge epic).
        let decl = AgentDecl {
            name: "a".into(),
            model: None,
            agent_loop: None,
            tools: vec![],
            datasources: vec![],
            description: Some("be terse".into()),
            permissions: None,
            settings: json!({
                "context": [
                    { "id": "hours", "title": "Opening hours", "body": "Mon–Fri 09:00–18:00 CET." }
                ]
            }),
        };
        let system = System::new(Workspace::new(".").unwrap());
        let spec = agent_spec_from_decl(&decl, "m", PathBuf::from("."), &system)
            .await
            .unwrap();
        assert_eq!(spec.context.len(), 1);
        let prompt = spec.effective_system_prompt();
        assert!(prompt.contains("be terse"), "persona kept: {prompt}");
        assert!(
            prompt.contains("<knowledge-base id=\"hours\" title=\"Opening hours\">"),
            "context injected: {prompt}"
        );
        assert!(prompt.contains("Mon–Fri 09:00–18:00 CET."));

        // A malformed context (not a list of blocks) is a clean, attributed error.
        let bad = AgentDecl {
            name: "b".into(),
            settings: json!({ "context": "nope" }),
            ..decl.clone()
        };
        assert!(
            agent_spec_from_decl(&bad, "m", PathBuf::from("."), &system)
                .await
                .is_err(),
            "a non-list context is an error"
        );
    }

    #[tokio::test]
    async fn agent_spec_appends_system_prompt_files() {
        // A declarative bot keeps its long persona in a file rather than inlining it (flux D-11):
        // `settings.system_prompt_files` paths are read (workspace-confined) and concatenated after the
        // base persona.
        let dir = std::env::temp_dir().join(format!("flux-persona-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("persona.md"), "PERSONA-MARKER: talk like a pirate").unwrap();

        let decl = AgentDecl {
            name: "a".into(),
            model: None,
            agent_loop: None,
            tools: vec![],
            datasources: vec![],
            description: Some("be terse".into()),
            permissions: None,
            settings: json!({ "system_prompt_files": ["persona.md"] }),
        };
        let system = System::new(Workspace::new(&dir).unwrap());
        let spec = agent_spec_from_decl(&decl, "m", dir.clone(), &system)
            .await
            .unwrap();
        assert!(
            spec.system_prompt.contains("be terse"),
            "the base description is kept: {}",
            spec.system_prompt
        );
        assert!(
            spec.system_prompt.contains("PERSONA-MARKER"),
            "the persona file is appended: {}",
            spec.system_prompt
        );

        // A missing file is a clean, attributed error — not a silently-empty persona.
        let bad = AgentDecl {
            name: "b".into(),
            settings: json!({ "system_prompt_files": ["nope.md"] }),
            ..decl.clone()
        };
        assert!(
            agent_spec_from_decl(&bad, "m", dir.clone(), &system)
                .await
                .is_err(),
            "an unreadable system_prompt_files path is an error"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-67: lazy agent construction must derive every workspace-sensitive capability from the
    /// explicit execution environment, never from a later process cwd. Run the cwd mutation in an
    /// isolated child test process so this process-global operation cannot race sibling tests.
    #[tokio::test]
    async fn execution_environment_retains_root_across_lazy_agent_construction() {
        const CHILD: &str = "FLUX_C67_CWD_CHILD";
        if std::env::var(CHILD).ok().as_deref() != Some("1") {
            let current_exe = std::env::current_exe().expect("current test executable");
            let cwd = std::env::current_dir().expect("parent test cwd");
            let process_system = System::new(Workspace::new(cwd).expect("parent workspace"));
            let output = process_system
                .run_with_env(
                    &[
                        current_exe.display().to_string(),
                        "execution_environment_retains_root_across_lazy_agent_construction"
                            .to_string(),
                        "--nocapture".to_string(),
                        "--test-threads=1".to_string(),
                    ],
                    &[(CHILD.to_string(), "1".to_string())],
                    std::time::Duration::from_secs(60),
                )
                .await
                .expect("run isolated cwd regression");
            assert_eq!(
                output.exit_code, 0,
                "isolated cwd regression failed\nstdout:\n{}\nstderr:\n{}",
                output.stdout, output.stderr
            );
            return;
        }

        let original = std::env::current_dir().expect("child test cwd");
        let base = std::env::temp_dir().join(format!(
            "flux-app-c67-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        let root = base.join("root");
        let other = base.join("other");
        for dir in [&root, &other] {
            std::fs::create_dir_all(dir.join(".flux/agents")).unwrap();
            std::fs::create_dir_all(dir.join(".flux/skills")).unwrap();
        }
        std::fs::write(root.join("Cargo.toml"), "[package]\nname='rooted'\n").unwrap();
        std::fs::write(other.join("package.json"), "{}").unwrap();
        std::fs::write(
            root.join("marker.txt"),
            "FROM-ORIGINAL-ROOT c67-secret-value",
        )
        .unwrap();
        std::fs::write(other.join("marker.txt"), "FROM-LATER-CWD").unwrap();
        std::fs::write(root.join("persona.md"), "PERSONA-FROM-ORIGINAL-ROOT").unwrap();
        std::fs::write(other.join("persona.md"), "PERSONA-FROM-LATER-CWD").unwrap();
        std::fs::write(
            root.join(".flux/agents/rooted.md"),
            "---\nname: rooted\ntools: [read]\n---\nROLE-FROM-ORIGINAL-ROOT",
        )
        .unwrap();
        std::fs::write(
            other.join(".flux/agents/rooted.md"),
            "---\nname: rooted\ntools: [read]\n---\nROLE-FROM-LATER-CWD",
        )
        .unwrap();
        std::fs::write(
            root.join(".flux/skills/c67-rooted.md"),
            "---\nname: c67-rooted\ndescription: rooted skill\ntriggers: [c67]\n---\nSKILL-FROM-ORIGINAL-ROOT",
        )
        .unwrap();
        std::fs::write(
            other.join(".flux/skills/c67-rooted.md"),
            "---\nname: c67-rooted\ndescription: wrong skill\ntriggers: [c67]\n---\nSKILL-FROM-LATER-CWD",
        )
        .unwrap();

        let system = Arc::new(System::new(Workspace::new(&root).unwrap()));
        let redactor = Redactor::new();
        redactor.add_secret("c67-secret-value");
        let environment = ExecutionEnvironment::new(
            system,
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            ExecutionAuthorization::local(),
        )
        .with_redactor(redactor);
        let events = Arc::new(EventStore::in_memory().unwrap());
        let app = App::try_with_execution_environment(
            Program {
                agents: vec![AgentDecl {
                    name: "assistant".into(),
                    model: None,
                    agent_loop: None,
                    tools: vec!["read".into()],
                    datasources: Vec::new(),
                    description: Some("root-stable agent".into()),
                    permissions: None,
                    settings: json!({"system_prompt_files": ["persona.md"]}),
                }],
                ..Program::default()
            },
            Some(Arc::new(ReplyProvider { reply: "ok".into() })),
            "mock",
            environment,
            None,
            events.clone(),
            HostPermissionRules::default(),
            Vec::new(),
        )
        .unwrap();

        std::env::set_current_dir(&other).unwrap();
        let engine = app.agent_engine("assistant").await.unwrap();
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        assert_eq!(engine.cwd, canonical_root);
        assert_eq!(
            engine.executor.context().system().workspace().root(),
            canonical_root
        );
        assert!(engine.system_prompt.contains("PERSONA-FROM-ORIGINAL-ROOT"));
        assert!(!engine.system_prompt.contains("PERSONA-FROM-LATER-CWD"));

        let signals = flux_runtime::detect_signals(&engine.cwd);
        let signals: Vec<&str> = signals
            .iter()
            .filter_map(|observation| observation.data["signal"].as_str())
            .collect();
        assert!(signals.contains(&"rust"), "signals: {signals:?}");
        assert!(!signals.contains(&"node"), "signals: {signals:?}");

        let roles = flux_agent::RoleRegistry::try_load_project(
            engine.executor.context().system().as_ref(),
            ".flux/agents",
        )
        .unwrap();
        assert_eq!(
            roles.get("rooted").unwrap().prompt,
            "ROLE-FROM-ORIGINAL-ROOT"
        );
        let skills = AgentSpec {
            cwd: engine.cwd.clone(),
            ..AgentSpec::new("mock")
        }
        .try_with_default_skills()
        .unwrap()
        .skills;
        let skill = skills
            .iter()
            .find(|skill| skill.name == "c67-rooted")
            .expect("rooted skill discovered");
        assert_eq!(skill.body.text(), "SKILL-FROM-ORIGINAL-ROOT");

        let read = engine
            .executor
            .dispatch("read", json!({"path": "marker.txt"}))
            .await;
        assert!(!read.is_error, "{}", read.content);
        assert!(read.content.contains("FROM-ORIGINAL-ROOT"));
        assert!(!read.content.contains("FROM-LATER-CWD"));
        assert!(read.content.contains("[redacted]"));
        assert!(Arc::ptr_eq(&engine.events, &events));

        // The compatibility `try_*` door must also fail cleanly when no current workspace can be
        // resolved. Its infallible counterpart may panic by contract; this path must not.
        let removed = base.join("removed-cwd");
        std::fs::create_dir_all(&removed).unwrap();
        std::env::set_current_dir(&removed).unwrap();
        std::fs::remove_dir(&removed).unwrap();
        let invalid = App::try_new(Program::default(), None, "mock");
        std::env::set_current_dir(&original).unwrap();
        assert!(invalid.is_err(), "invalid cwd unexpectedly built an App");

        std::fs::remove_dir_all(base).ok();
    }

    /// C-183: `flux app run`'s per-agent executors must resolve + install `[tools] disable`
    /// exactly like the interactive CLI path (C-162) — the app-run assembly seam
    /// (`App::try_with_execution_environment` → `Engine::agent_engine`) is where this story wires
    /// the resolution that was previously only reached by the CLI's `build_agent_with`.
    #[tokio::test]
    async fn app_run_agent_target_executor_installs_tools_disable() {
        let dir = std::env::temp_dir().join(format!(
            "flux-c183-agent-disable-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let environment = ExecutionEnvironment::new(
            system,
            ToolRegistry::new(),
            PermissionManager::new(),
            Arc::new(DenyApprover),
            ExecutionAuthorization::local(),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let program = Program {
            agents: vec![AgentDecl {
                name: "worker".into(),
                model: None,
                agent_loop: None,
                tools: vec!["now".into()],
                datasources: Vec::new(),
                description: Some("uses now".into()),
                permissions: None,
                settings: Value::Null,
            }],
            ..Program::default()
        };
        let app = App::try_with_execution_environment(
            program,
            Some(Arc::new(ReplyProvider { reply: "ok".into() })),
            "mock",
            environment,
            None,
            events,
            HostPermissionRules::default(),
            vec!["now".to_string(), "no-such-op".to_string()],
        )
        .expect("assemble App with [tools] disable");

        let engine = app.agent_engine("worker").await.unwrap();
        assert!(
            engine.executor.disabled_ops().contains("now"),
            "the app-run agent-target executor must carry the resolved disabled set, not just \
             the CLI's interactive executor"
        );
        let outcome = engine.executor.dispatch_outcome("now", json!({})).await;
        assert!(
            outcome.denied,
            "a config-disabled op must be refused at dispatch on the app-run agent-target path too"
        );
        assert!(
            outcome.result.content.contains("disabled by config"),
            "{}",
            outcome.result.content
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn agent_trigger_runs_a_turn_and_returns_the_reply() {
        let app = app_with_agent("hi back");
        let runs = app
            .deliver("slack", json!({ "text": "hello", "conversation": "T1" }))
            .await
            .expect("deliver");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].journey, "assistant");
        assert!(
            runs[0].result.contains("hi back"),
            "agent reply should be the model's answer, got: {:?}",
            runs[0].result
        );
    }

    /// C-33: an `agent`-bound trigger's turn carries its real usage and the engine's own canonical
    /// model spec onto the returned [`JourneyRun`] — the seam an operator-console surface (the
    /// `flux app run` CLI channel, `flux-channels::host::serve`) needs to render a cost annotation.
    /// The story's named failing-first test for the app-run/journey/agent-target surface.
    #[tokio::test]
    async fn agent_trigger_run_carries_usage_and_model_spec() {
        let src = "\
agent assistant
  description \"be terse\"
  tools []

trigger t1
  on \"slack\"
  run _
  agent assistant
";
        let provider: Arc<dyn Provider> = Arc::new(ReplyWithUsageProvider {
            reply: "hi back".to_string(),
        });
        let app = App::with_options(program(src), Some(provider), "mock", false);
        let runs = app
            .deliver("slack", json!({ "text": "hello", "conversation": "T1" }))
            .await
            .expect("deliver");
        assert_eq!(runs.len(), 1);
        let usage = runs[0]
            .usage
            .as_ref()
            .expect("an agent turn that reports usage must carry it onto the JourneyRun");
        assert_eq!(
            usage.input_tokens, 100,
            "turn input is the latest stage's context-window occupancy"
        );
        assert_eq!(
            usage.output_tokens, 80,
            "intent + exploration output is cumulative"
        );
        assert_eq!(
            runs[0].model, "mock",
            "the driving engine's own canonical model spec, not some other default"
        );
    }

    #[tokio::test]
    async fn same_conversation_reuses_one_session_distinct_ones_isolate() {
        let app = app_with_agent("ok");
        // Two mentions on the same thread accumulate in one session (multi-turn memory).
        app.deliver("slack", json!({ "text": "first", "conversation": "T1" }))
            .await
            .unwrap();
        let after_one = app.agent_session_len("assistant", "T1");
        app.deliver("slack", json!({ "text": "second", "conversation": "T1" }))
            .await
            .unwrap();
        let after_two = app.agent_session_len("assistant", "T1");
        assert!(
            after_one > 0,
            "the first turn should persist to the thread's session"
        );
        assert!(
            after_two > after_one,
            "the thread's session should grow across turns: {after_one} -> {after_two}"
        );
        // A different thread is a separate session, not the T1 one.
        app.deliver(
            "slack",
            json!({ "text": "elsewhere", "conversation": "T2" }),
        )
        .await
        .unwrap();
        assert!(app.agent_session_len("assistant", "T2") > 0);
        assert_eq!(
            app.agent_session_len("assistant", "T1"),
            after_two,
            "delivering to T2 must not touch T1's session"
        );
    }

    #[tokio::test]
    async fn trigger_without_agent_still_runs_its_journey() {
        // A plain journey trigger (no `agent`) runs the journey unchanged — the agentic path is additive.
        let src = "\
trigger t1
  on \"ping\"
  run pong

journey pong
  flow
    return \"pong!\"
";
        let app = App::with_options(program(src), None, "mock", false);
        let runs = app.deliver("ping", json!({})).await.expect("deliver");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].journey, "pong");
        assert_eq!(runs[0].result, "pong!");
    }

    /// C-33: a plain journey that dispatches no model call carries no usage — but still reports the
    /// host's default model spec (the "driving engine spec" honest stand-in for a journey, which has
    /// no per-op engine of its own) so a cost-display surface has something to show *if* usage were
    /// ever present, without inventing a fake dollar figure now.
    #[tokio::test]
    async fn journey_run_carries_no_usage_but_reports_the_host_default_model() {
        let src = "\
trigger t1
  on \"ping\"
  run pong

journey pong
  flow
    return \"pong!\"
";
        let app = App::with_options(program(src), None, "mock", false);
        let runs = app.deliver("ping", json!({})).await.expect("deliver");
        assert_eq!(runs.len(), 1);
        assert!(
            runs[0].usage.is_none(),
            "a pure-op journey drives no model call, so there's no usage to report"
        );
        assert_eq!(runs[0].model, "mock");
    }

    /// C-66: a plain authored journey has no `FlowEngine::turn_end` callback, but cognition calls
    /// still bill. The journey result must fold the operation's retained evidence into its usage
    /// exactly once, using independent-call (field-wise sum) semantics.
    #[tokio::test]
    async fn journey_run_includes_cognition_usage() {
        let src = r#"
trigger t1
  on "ping"
  run extract

journey extract
  flow
    $claims = ai.extract({ from: "Alice" })
    return $claims
"#;
        let expected = Usage {
            input_tokens: 73,
            output_tokens: 11,
            ..Default::default()
        };
        let app = App::with_options(
            program(src),
            Some(Arc::new(CognitionUsageProvider(expected.clone()))),
            "test-model",
            true,
        );

        let runs = app.deliver("ping", json!({})).await.expect("deliver");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].result, "[]");
        assert_eq!(runs[0].usage, Some(expected));
        assert_eq!(runs[0].model, "test-model");
    }

    /// C-66: when a cognition op runs inside a real `FlowEngine` turn, its independent provider
    /// call must enter the same per-call accounting path as adaptive stages. That gives the sink a
    /// turn total and gives event/cost projections one attributed `CallUsage` row.
    #[tokio::test]
    async fn flow_engine_turn_and_cost_projection_include_cognition_usage() {
        let expected = Usage {
            input_tokens: 53,
            output_tokens: 7,
            ..Default::default()
        };
        let events = Arc::new(EventStore::in_memory().unwrap());
        let engine = cognition_engine(
            Arc::new(CognitionUsageProvider(expected.clone())),
            events.clone(),
        )
        .await;
        let session = events.create_session("mock/test-model").unwrap();
        let flow = flux_lang::parse::parse(
            "flow\n  $claims = ai.extract({ from: \"Alice\" })\n  return $claims",
        )
        .unwrap();
        let mut sink = RecordingSink::default();

        engine
            .start_flow_turn(&session, &flow, &mut sink)
            .await
            .unwrap();

        assert_eq!(sink.usage, Some(expected.clone()));
        let turns = events.turns(&session).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].calls, 1, "one cognition call is attributed");
        assert_eq!(turns[0].call_usage, expected);
        assert_eq!(turns[0].usage, Some(expected.clone()));

        let costs = events
            .cost_summary(&session, &flux_core::PricingTable::default())
            .unwrap();
        assert_eq!(costs.len(), 1);
        assert_eq!(costs[0].model, "test-model");
        assert_eq!(costs[0].calls, 1);
        assert_eq!(costs[0].usage, expected);
    }

    /// The same durable accounting must survive the cognition provider failing after its usage
    /// frame. The engine keeps the failed turn/error outcome while emitting exactly one call row.
    #[tokio::test]
    async fn failed_cognition_call_reaches_turn_and_cost_projection_once() {
        let expected = Usage {
            input_tokens: 61,
            output_tokens: 5,
            ..Default::default()
        };
        let events = Arc::new(EventStore::in_memory().unwrap());
        let engine = cognition_engine(
            Arc::new(CognitionErrorProvider(expected.clone())),
            events.clone(),
        )
        .await;
        let session = events.create_session("test-model").unwrap();
        let flow = flux_lang::parse::parse(
            "flow\n  $claims = ai.extract({ from: \"Alice\" })\n  return $claims",
        )
        .unwrap();
        let mut sink = RecordingSink::default();

        engine
            .start_flow_turn(&session, &flow, &mut sink)
            .await
            .unwrap();

        assert_eq!(sink.usage, Some(expected.clone()));
        let turns = events.turns(&session).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].outcome, "error");
        assert_eq!(turns[0].calls, 1);
        assert_eq!(turns[0].call_usage, expected);
        assert_eq!(turns[0].usage, Some(expected.clone()));

        let costs = events
            .cost_summary(&session, &flux_core::PricingTable::default())
            .unwrap();
        assert_eq!(costs.len(), 1);
        assert_eq!(costs[0].calls, 1);
        assert_eq!(costs[0].usage, expected);
    }

    /// Cancelling a real engine turn drops the pending cognition future before turn finalization.
    /// The drop guard must publish the already-observed usage first, while the durable terminal
    /// outcome remains `cancelled` and the call is attributed exactly once.
    #[tokio::test]
    async fn cancelled_cognition_call_reaches_cancelled_turn_projection_once() {
        let expected = Usage {
            input_tokens: 47,
            output_tokens: 2,
            ..Default::default()
        };
        let pending = Arc::new(tokio::sync::Notify::new());
        let mut registry = ToolRegistry::new();
        flux_cognition::CognitionPack::new(
            Arc::new(PendingCognitionProvider {
                usage: expected.clone(),
                pending: pending.clone(),
            }),
            "cognition-model",
        )
        .register(&mut registry);
        flux_agent::register_agent_ops(&mut registry).unwrap();
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["ai.extract".into()], &[]),
            Arc::new(flux_runtime::AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(".").unwrap()))),
        );
        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = FlowEngine::assemble(
            Arc::new(CognitionPlanProvider {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }),
            executor,
            events.clone(),
            flow,
            "planner-model".into(),
            "plan then extract".into(),
            1024,
            5,
            Vec::new(),
            0,
            Vec::new(),
            std::env::current_dir().unwrap(),
        )
        .unwrap();
        let session = events.create_session("planner-model").unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut sink = RecordingSink::default();
        let result = {
            let turn = engine.run_turn_cancellable(&session, "extract Alice", &mut sink, &cancel);
            tokio::pin!(turn);

            tokio::select! {
                _ = pending.notified() => cancel.cancel(),
                result = &mut turn => panic!("turn ended before cognition became pending: {result:?}"),
            }
            turn.await
        };
        result.unwrap();

        assert_eq!(sink.usage, Some(expected.clone()));
        let turns = events.turns(&session).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].outcome, "cancelled");
        assert_eq!(turns[0].calls, 1);
        assert_eq!(turns[0].call_usage, expected);
        assert_eq!(turns[0].usage, Some(expected));
    }

    /// C-66: a child `FlowEngine` uses the same turn accounting hook, so cognition spend survives
    /// the `LocalSpawner` boundary in `SpawnOutcome.usage`. `TaskTool`'s typed roll-up then records
    /// this outcome on the parent without needing to inspect child evidence.
    #[tokio::test]
    async fn sub_agent_outcome_includes_child_cognition_usage() {
        let expected = Usage {
            input_tokens: 89,
            output_tokens: 13,
            ..Default::default()
        };
        let mut base = ToolRegistry::new();
        flux_cognition::CognitionPack::new(
            Arc::new(CognitionUsageProvider(expected.clone())),
            "cognition-model",
        )
        .register(&mut base);
        let mut roles = flux_agent::RoleRegistry::default();
        roles.insert(
            flux_agent::try_parse_role(
                "---\ntools: [ai.extract]\n---\nExtract the requested facts.",
                "worker",
            )
            .unwrap(),
        );
        let spawner = flux_orchestrate::LocalSpawner::new(
            Arc::new(|| {
                Ok(Box::new(CognitionPlanProvider {
                    calls: std::sync::atomic::AtomicUsize::new(0),
                }) as Box<dyn Provider>)
            }),
            Arc::new(roles),
            base,
            Arc::new(System::new(Workspace::new(".").unwrap())),
            "planner-model",
            1024,
        );

        let ctx = ToolContext::new(Arc::new(System::new(Workspace::new(".").unwrap())))
            .with_spawner(Arc::new(spawner));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            TaskTool.execute(&ctx, json!({ "role": "worker", "task": "extract Alice" })),
        )
        .await
        .expect("child completed")
        .expect("child succeeded");
        assert!(!result.is_error, "{}", result.content);

        let observations: Vec<_> = ctx
            .evidence
            .lock()
            .unwrap()
            .by_kind("subagent.usage")
            .cloned()
            .collect();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].data["model"], "planner-model");
        let usage: Usage = serde_json::from_value(observations[0].data["usage"].clone()).unwrap();
        assert_eq!(usage, expected);
    }

    #[tokio::test]
    async fn scheduled_agent_turn_receives_event_context() {
        // An event carrying no user `text` (a `startup` / schedule tick, vs. a Slack mention) must still
        // wake the agent with a concrete turn that names the firing trigger and carries the payload (e.g.
        // the tick's `at`), so a monitor can branch startup-vs-tick and read the time (flux D-11). The
        // echo provider surfaces the exact turn input as the reply.
        let src = "\
agent monitor
  description \"watch\"
  tools []

trigger tick
  on \"schedule\"
  run _
  agent monitor
";
        let provider: Arc<dyn Provider> = Arc::new(EchoProvider);
        let app = App::with_options(program(src), Some(provider), "mock", false);
        let runs = app
            .deliver("schedule", json!({ "at": "2026-06-30T12:00:00Z" }))
            .await
            .expect("deliver");
        assert_eq!(runs.len(), 1);
        let reply = &runs[0].result;
        assert!(
            reply.contains("schedule"),
            "the turn names the firing trigger/event: {reply}"
        );
        assert!(
            reply.contains("2026-06-30T12:00:00Z"),
            "the turn carries the schedule `at`: {reply}"
        );
    }
}

#[cfg(test)]
mod sandbox_posture_tests {
    use super::*;
    use flux_system::sandbox::SandboxMode;
    use std::sync::Mutex;

    /// Serializes tests that mutate the process-global `FLUX_SANDBOX` env var — two concurrent
    /// `set_var`/`remove_var` calls on the same key race across parallel test threads (mirrors the
    /// `SANDBOX_ENV_LOCK` guard in `flux-system`'s own sandbox tests).
    static SANDBOX_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Restores `FLUX_SANDBOX` to its prior value on drop — panic-safe so a failed assertion can't
    /// leak a posture into a later test in the same process.
    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(value: &str) -> Self {
            let lock = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = std::env::var_os("FLUX_SANDBOX");
            std::env::set_var("FLUX_SANDBOX", value);
            Self { _lock: lock, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(v) => std::env::set_var("FLUX_SANDBOX", v),
                None => std::env::remove_var("FLUX_SANDBOX"),
            }
        }
    }

    fn temp_workspace(tag: &str) -> Workspace {
        let dir =
            std::env::temp_dir().join(format!("flux-app-sandbox-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Workspace::new(&dir).unwrap()
    }

    /// The app's shared `System` (built by `guarded_system`, the backing for both the journey/sub-agent
    /// path and the `app run --serve` agent-target path) must carry the env-resolved sandbox posture —
    /// otherwise `FLUX_SANDBOX`/`--sandbox` would silently do nothing on those spawn paths.
    #[test]
    fn guarded_system_inherits_require_posture_from_env() {
        let _guard = EnvGuard::set("require");
        let system = guarded_system(temp_workspace("require"));
        assert_eq!(
            system.sandbox().settings().mode,
            SandboxMode::Require,
            "FLUX_SANDBOX=require must reach the app's System"
        );
        // `require` with no usable backend fails closed on `ensure_available` — the fail-safe the docs
        // promise. (On a host with a working bwrap/sandbox-exec this instead resolves an active backend
        // and is Ok; either way it is NOT the silent no-op a bare `System::new` would give.)
        let disabled_default = flux_system::System::new(temp_workspace("default"));
        assert_eq!(
            disabled_default.sandbox().settings().mode,
            SandboxMode::Off,
            "a bare System::new stays disabled — proving guarded_system is what carries the posture"
        );
    }

    /// With nothing set, `guarded_system` resolves to off/disabled so hermetic callers stay unconfined.
    #[test]
    fn guarded_system_defaults_off_when_env_unset() {
        let _guard = EnvGuard::set("off");
        let system = guarded_system(temp_workspace("off"));
        assert_eq!(system.sandbox().settings().mode, SandboxMode::Off);
    }
}
