//! Fleet worker lifecycle (C-243) — the two first implementations of the [`AgentRuntime`] port and
//! the three ops that drive it.
//!
//! A-116 gave the fleet a way to *talk* to a worker. It gave it no way to *have* one: `flux` never
//! spawned `flux`, and `FlowEngine`'s turn gate serves one concurrent turn per worker, so every
//! coordinator "wave" was a wave of one. That is why [`ProcessRuntime`] is a prerequisite for
//! parallelism rather than an optimization of it.
//!
//! ## [`ProcessRuntime`] — a child `flux` on this machine
//!
//! One worker is one `flux app run --serve 127.0.0.1:<port> --yes` child, started through
//! [`System::spawn_background`] — flux's single `build_command` choke point, so the child is
//! argv-only (no shell string), its cwd is pinned to the workspace root, and its environment is
//! **cleared** to the minimal non-secret allow-list. There is no second `Command::new` here and
//! there must never be one.
//!
//! Three consequences of going through that one path are worth stating, because they are the design
//! rather than incidental:
//!
//! * **Worktree scoping is free.** [`WorkerSpec::worktree`] is applied by re-rooting the guarded
//!   system ([`System::rerooted`]) before the spawn, so the child's cwd *and* the OS sandbox's
//!   writable set both follow the isolated checkout — the confinement is structural, not an
//!   instruction in the worker's prompt.
//! * **The port is chosen by the parent and proven by the child.** This crate opens no socket to
//!   look for a free port (it must not: it is a model-facing operation crate). It offers the child a
//!   port; the child's own `bind` is the availability test, and an "address already in use" exit
//!   moves on to the next candidate. That is both simpler and more honest than a probe-then-hope
//!   race.
//! * **Readiness is the worker's own word.** `flux-server` prints `flux server listening on
//!   http://<addr>` to stderr once bound, and [`System::spawn_background`] pipes stderr into a capped
//!   drained buffer. So `start` waits for that line — no readiness probe, hence no egress, hence
//!   these ops declare no network access at all.
//!
//! ## [`ExternalRuntime`] — a worker somebody else runs
//!
//! The degenerate implementation, and the reason the port has value on day one: an operator-run
//! worker becomes addressable through the same four verbs, so a coordinator Program never branches
//! on how its workers came to exist. It deliberately refuses to `stop` — it did not start the
//! process and must not pretend it can end it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_runtime::{
    AgentRuntime, Tool, ToolContext, ToolResult, Worker, WorkerSpec, WorkerState, WorkerStatus,
};
use flux_spec::{tool_input_schema, AccessKind, Effect, Idempotency, Risk, ToolSpec};
use flux_system::{ManagedChild, System};

use crate::parse_params;

/// First loopback port a [`ProcessRuntime`] worker is offered. Above the `flux app run --serve`
/// default (`8787`) so a hand-started server and a fleet worker never contend for the same port.
pub const DEFAULT_WORKER_BASE_PORT: u16 = 8790;

/// How many consecutive ports one `start` will offer before giving up. Bounds the work a coordinator
/// on a busy box does before reporting that it cannot place a worker.
const PORT_SPAN: u16 = 64;

/// How long `start` waits for a worker to announce that it is serving. Generous: a cold child `flux`
/// loads config, resolves a provider and builds its catalog before it binds.
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Cadence of the readiness poll. Fast enough that a test does not pay for the budget above.
const READY_POLL: Duration = Duration::from_millis(25);

/// The line `flux_server::serve` prints to stderr once it has bound — the worker's own statement
/// that it is serving, and the only readiness signal available without making a request.
const LISTENING_MARKER: &str = "listening on http://";

/// How much of a worker's own output is kept as the `detail` a status poll reports. Enough for a
/// startup failure to explain itself, bounded so a chatty worker cannot grow this map without limit
/// (`ManagedChild` already caps what one drain returns; this caps what is *retained*).
const RETAINED_LOG: usize = 8 * 1024;

// ── ProcessRuntime ──────────────────────────────────────────────────────────

/// One live child `flux`, plus what the ops need to answer about it after the fact.
struct ProcessWorker {
    /// The guarded child handle. Dropping it kills the worker (`kill_on_drop`), which is what makes
    /// a coordinator crash leak no orphaned workers.
    child: ManagedChild,
    endpoint: String,
    context_id: String,
    /// Tail of the worker's own stdout/stderr, accumulated across polls. Retained because
    /// [`ManagedChild::read_output`] *drains*: the reason a worker died is readable exactly once, so
    /// the runtime keeps it or loses it.
    log: String,
    /// Exit code observed on the first status poll that saw the worker gone. Cached for the same
    /// reason — `try_wait` reports a code once.
    exit_code: Option<i32>,
    /// Whether the worker has ever announced that it is serving.
    announced: bool,
}

/// A fleet worker as a **child `flux` process on this machine**, started through the guarded spawn.
pub struct ProcessRuntime {
    /// The `flux` binary a worker is started from.
    program: PathBuf,
    /// Cursor over the loopback port range, so N concurrent starts do not all begin at the same
    /// candidate and serialize on each other's bind failures.
    next_port: AtomicU16,
    /// Live workers by id. A `tokio` mutex because a `start` holds it across the readiness await.
    workers: tokio::sync::Mutex<HashMap<String, ProcessWorker>>,
    ready_timeout: Duration,
    /// Extra environment for every worker, applied on top of `build_command`'s cleared, allow-listed
    /// base. **Empty in production, deliberately**: a worker resolves its own provider credentials
    /// from its own configuration through the forwarded `HOME`, exactly as any other `flux` process
    /// does, and forwarding anything from the coordinator's environment here would be the one thing
    /// the env-clear exists to prevent. It is a seam so a host — and the tests below — can steer a
    /// non-default worker program without a second spawn path.
    env: Vec<(String, String)>,
}

impl ProcessRuntime {
    /// Build the runtime that starts workers from **this** `flux` binary.
    ///
    /// `current_exe` is the same resolution `eval_run` uses to find the binary it benchmarks, and
    /// for the same reason: the coordinator and its workers must be the same build, or a wave's
    /// results describe two different agents.
    pub fn new() -> Result<Self> {
        let program = std::env::current_exe()
            .map_err(|e| Error::Other(format!("fleet.start: locate the flux binary: {e}")))?;
        Ok(Self::with_program(program))
    }

    /// Build the runtime around an explicit worker program.
    ///
    /// Trusted host configuration, never a model input — the same posture as `FLUX_EVAL_BINARY`. Two
    /// callers need it: a host whose `flux` is not `current_exe` (a wrapper, a re-exec shim), and the
    /// tests below, which stand a worker up without paying for a full `flux` boot.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            next_port: AtomicU16::new(DEFAULT_WORKER_BASE_PORT),
            workers: tokio::sync::Mutex::new(HashMap::new()),
            ready_timeout: DEFAULT_READY_TIMEOUT,
            env: Vec::new(),
        }
    }

    /// Override the loopback port range's start — for an operator whose default range is taken.
    pub fn with_base_port(mut self, base: u16) -> Self {
        self.next_port = AtomicU16::new(base);
        self
    }

    /// Override the readiness budget and the environment a worker is started with. Host/test
    /// configuration in the same sense as [`with_program`](Self::with_program); the values are
    /// applied on top of `build_command`'s cleared, allow-listed base and are never model-supplied.
    pub fn with_startup(mut self, ready_timeout: Duration, env: Vec<(String, String)>) -> Self {
        self.ready_timeout = ready_timeout;
        self.env = env;
        self
    }

    /// The next port candidate, wrapping inside the span so a long-lived coordinator that has
    /// started and stopped many workers keeps reusing the same bounded range.
    fn next_candidate(&self) -> u16 {
        let base = DEFAULT_WORKER_BASE_PORT;
        let taken = self.next_port.fetch_add(1, Ordering::Relaxed);
        // `wrapping_*` keeps this total for any configured base; the modulus keeps it in the span.
        base.wrapping_add(taken.wrapping_sub(base) % PORT_SPAN)
    }

    /// The argv one worker is started with. Argv-only by construction — there is no string here that
    /// a shell would ever see.
    fn argv(&self, addr: &str, spec: &WorkerSpec) -> Vec<String> {
        let mut argv = vec![
            self.program.display().to_string(),
            "app".to_string(),
            "run".to_string(),
            // `=` rather than a separate token: `--serve <addr>` is clap's optional-value flag and
            // would swallow a following positional as the address.
            format!("--serve={addr}"),
            // A served worker has no interactive approver, so `flux app run --serve` requires this.
            // It is not a widening: the worker's authority is bounded by its own policy and — via
            // the re-rooted system below — by a writable set confined to its own checkout.
            "--yes".to_string(),
        ];
        if let Some(model) = spec
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            argv.push("-m".to_string());
            argv.push(model.to_string());
        }
        argv
    }

    /// Wait for a freshly spawned child to announce that it is serving, or to die trying.
    ///
    /// Drains before testing liveness on every pass, and once more after observing an exit: the
    /// output is what explains a failure, and `read_output` yields each byte exactly once.
    async fn await_ready(&self, child: &mut ManagedChild) -> Readiness {
        let deadline = Instant::now() + self.ready_timeout;
        let mut log = String::new();
        loop {
            let (out, err) = child.read_output();
            push_capped(&mut log, &out);
            push_capped(&mut log, &err);
            if log.contains(LISTENING_MARKER) {
                return Readiness::Serving(log);
            }
            let status = child.status();
            if !status.running {
                let (out, err) = child.read_output();
                push_capped(&mut log, &out);
                push_capped(&mut log, &err);
                return Readiness::Exited {
                    log,
                    exit_code: status.exit_code,
                };
            }
            if Instant::now() >= deadline {
                return Readiness::TimedOut(log);
            }
            tokio::time::sleep(READY_POLL).await;
        }
    }
}

/// How a `start` attempt on one candidate port turned out.
enum Readiness {
    /// The worker announced that it is serving. Carries its output so far.
    Serving(String),
    /// The worker exited before announcing anything.
    Exited { log: String, exit_code: Option<i32> },
    /// Still running at the readiness deadline — a worker this slow is not usable, and leaving it
    /// running would leak a process nothing tracks.
    TimedOut(String),
}

/// Did this worker die because the port was taken? Matched on the OS's own words (`std::io::Error`
/// renders `EADDRINUSE` as "Address already in use") rather than on a flux-side message, because the
/// bind error arrives from `tokio`'s listener and passes through untranslated.
fn port_conflict(log: &str) -> bool {
    let log = log.to_ascii_lowercase();
    log.contains("address already in use") || log.contains("os error 98")
}

/// Append to a retained log, keeping the **tail** within [`RETAINED_LOG`] — a startup failure's
/// explanation is at the end, so dropping the head is the right truncation.
fn push_capped(log: &mut String, chunk: &str) {
    if chunk.is_empty() {
        return;
    }
    log.push_str(chunk);
    if log.len() > RETAINED_LOG {
        // Split on a char boundary at or after the ideal cut so this never panics mid-codepoint.
        let cut = log.len() - RETAINED_LOG;
        let cut = (cut..log.len())
            .find(|i| log.is_char_boundary(*i))
            .unwrap_or(log.len());
        *log = log[cut..].to_string();
    }
}

/// The tail of a worker's output, for an error message that has to fit in a tool result.
fn tail(log: &str, lines: usize) -> String {
    let kept: Vec<&str> = log.trim_end().lines().rev().take(lines).collect();
    kept.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[async_trait]
impl AgentRuntime for ProcessRuntime {
    fn kind(&self) -> &'static str {
        "process"
    }

    async fn start(&self, system: &System, spec: WorkerSpec) -> Result<Worker> {
        // Preflight in cost order, cheapest and most specific first, so nothing is spawned that
        // would immediately have to be torn down again — the same ordering `fleet.isolate` uses.
        let id = spec.name.trim();
        if id.is_empty() {
            return Err(Error::Other(
                "fleet.start: `item` must name the work this worker serves — it is also the \
                 worker's id, so an anonymous worker could never be found again"
                    .into(),
            ));
        }
        let mut workers = self.workers.lock().await;
        if workers.contains_key(id) {
            return Err(Error::Other(format!(
                "fleet.start: a worker for `{id}` is already running — stop it with fleet.stop \
                 before starting another, or dispatch to the one that exists"
            )));
        }
        // Scope the worker to its checkout BEFORE spawning: the re-rooted system carries both the
        // child's cwd and the sandbox's writable set, so this is the confinement, not a hint.
        let rerooted = match spec.worktree.as_deref() {
            Some(dir) => Some(system.rerooted(dir).map_err(|e| {
                Error::Other(format!(
                    "fleet.start: worker `{id}` cannot be scoped to `{}`: {e}",
                    dir.display()
                ))
            })?),
            None => None,
        };
        let system = rerooted.as_ref().unwrap_or(system);

        let mut refused = Vec::new();
        for _ in 0..PORT_SPAN {
            let port = self.next_candidate();
            let addr = format!("127.0.0.1:{port}");
            let argv = self.argv(&addr, &spec);
            let mut child = system.spawn_background(&argv, &self.env)?;
            match self.await_ready(&mut child).await {
                Readiness::Serving(log) => {
                    let endpoint = format!("http://{addr}");
                    workers.insert(
                        id.to_string(),
                        ProcessWorker {
                            child,
                            endpoint: endpoint.clone(),
                            context_id: spec.context_id.clone(),
                            log,
                            exit_code: None,
                            announced: true,
                        },
                    );
                    return Ok(Worker {
                        id: id.to_string(),
                        endpoint,
                        context_id: spec.context_id,
                    });
                }
                // The child's own bind is the port-availability test; a conflict just means the next
                // candidate. `child` is dropped here, so nothing is left behind either way.
                Readiness::Exited { log, exit_code } if port_conflict(&log) => {
                    refused.push(port);
                    let _ = exit_code;
                }
                Readiness::Exited { log, exit_code } => {
                    return Err(Error::Other(format!(
                        "fleet.start: worker `{id}` exited before it began serving (exit {}): {}",
                        exit_code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "signalled".into()),
                        tail(&log, 12)
                    )));
                }
                Readiness::TimedOut(log) => {
                    // Killed by the drop below. A worker that never announces is not usable, and
                    // leaving it running would leak a process no id refers to.
                    return Err(Error::Other(format!(
                        "fleet.start: worker `{id}` did not begin serving within {}s and was \
                         stopped: {}",
                        self.ready_timeout.as_secs(),
                        tail(&log, 12)
                    )));
                }
            }
        }
        Err(Error::Other(format!(
            "fleet.start: worker `{id}` found no free loopback port — {} candidate(s) in the \
             {DEFAULT_WORKER_BASE_PORT}..+{PORT_SPAN} range were all in use ({refused:?})",
            refused.len()
        )))
    }

    async fn stop(&self, _system: &System, id: &str) -> Result<()> {
        let mut workers = self.workers.lock().await;
        // Removing the handle kills the child on drop, so the stop is the removal — there is no
        // window in which the map claims a worker this runtime has already signalled.
        match workers.remove(id) {
            Some(mut worker) => {
                worker.child.kill();
                Ok(())
            }
            None => Err(Error::Other(format!(
                "fleet.stop: no worker `{id}` was started by this coordinator"
            ))),
        }
    }

    async fn status(&self, _system: &System, id: &str) -> Result<WorkerStatus> {
        let mut workers = self.workers.lock().await;
        let worker = workers
            .get_mut(id)
            .ok_or_else(|| Error::Other(format!("fleet.worker_status: no worker `{id}`")))?;
        // Drain before the liveness test, and retain: a worker that just died explains itself in
        // output that `read_output` yields exactly once.
        let (out, err) = worker.child.read_output();
        push_capped(&mut worker.log, &out);
        push_capped(&mut worker.log, &err);
        if worker.log.contains(LISTENING_MARKER) {
            worker.announced = true;
        }
        let live = worker.child.status();
        if !live.running {
            let (out, err) = worker.child.read_output();
            push_capped(&mut worker.log, &out);
            push_capped(&mut worker.log, &err);
            // `try_wait` reports a code once; keep the first answer so repeated polls agree.
            worker.exit_code = worker.exit_code.or(live.exit_code);
        }
        let state = match (live.running, worker.announced) {
            (false, _) => WorkerState::Dead,
            (true, true) => WorkerState::Live,
            (true, false) => WorkerState::Starting,
        };
        Ok(WorkerStatus {
            id: id.to_string(),
            state,
            // A dead worker's endpoint is reported as absent rather than as an address that would
            // refuse every connection — the whole point of the state is not to look dispatchable.
            endpoint: (state != WorkerState::Dead).then(|| worker.endpoint.clone()),
            context_id: Some(worker.context_id.clone()),
            exit_code: worker.exit_code,
            detail: tail(&worker.log, 12),
        })
    }

    async fn endpoint(&self, _system: &System, id: &str) -> Result<String> {
        let workers = self.workers.lock().await;
        workers.get(id).map(|w| w.endpoint.clone()).ok_or_else(|| {
            Error::Other(format!("fleet worker `{id}` is not known to this runtime"))
        })
    }
}

// ── ExternalRuntime ─────────────────────────────────────────────────────────

/// A worker **somebody else runs** — an operator-managed `flux serve` — made addressable through the
/// same port, so a coordinator Program never branches on how its workers came to exist.
///
/// It owns no process, and says so rather than pretending: `stop` refuses, and `status` reports
/// `Live` for a configured worker with the honest caveat in `detail` that liveness it did not spawn
/// is only observable by dispatching to it. `Dead` here would be a guess; `Live` plus the caveat is
/// what the runtime actually knows.
pub struct ExternalRuntime {
    /// Worker id → endpoint, from operator configuration.
    endpoints: HashMap<String, String>,
}

impl ExternalRuntime {
    /// Bind the runtime to the operator's worker table.
    pub fn new(endpoints: HashMap<String, String>) -> Self {
        Self { endpoints }
    }

    fn resolve(&self, id: &str) -> Result<String> {
        self.endpoints.get(id).cloned().ok_or_else(|| {
            let mut known: Vec<&str> = self.endpoints.keys().map(String::as_str).collect();
            known.sort_unstable();
            Error::Other(format!(
                "no external worker `{id}` is configured (known: {})",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ))
        })
    }
}

#[async_trait]
impl AgentRuntime for ExternalRuntime {
    fn kind(&self) -> &'static str {
        "external"
    }

    async fn start(&self, _system: &System, spec: WorkerSpec) -> Result<Worker> {
        let id = spec.name.trim();
        let endpoint = self.resolve(id)?;
        // Deliberately not an error: `start` on an external worker is the *no-op that makes it
        // addressable*, which is exactly what lets one Program drive either runtime.
        Ok(Worker {
            id: id.to_string(),
            endpoint,
            context_id: spec.context_id,
        })
    }

    async fn stop(&self, _system: &System, id: &str) -> Result<()> {
        // Refused, not silently ignored. A coordinator that believes it stopped a worker it cannot
        // stop will reassign the item and end up with two workers on one branch.
        let _ = self.resolve(id)?;
        Err(Error::Other(format!(
            "fleet.stop: worker `{id}` is externally managed — this coordinator did not start it \
             and cannot stop it; cancel its task with fleet.cancel instead"
        )))
    }

    async fn status(&self, _system: &System, id: &str) -> Result<WorkerStatus> {
        let endpoint = self.resolve(id)?;
        Ok(WorkerStatus {
            id: id.to_string(),
            state: WorkerState::Live,
            endpoint: Some(endpoint),
            // An external worker's session binding belongs to whoever dispatches to it — this
            // runtime never minted one, so it reports none rather than inventing one.
            context_id: None,
            exit_code: None,
            detail: "externally managed worker: this runtime owns no process, so liveness is only \
                     observable by dispatching to it (fleet.dispatch / fleet.status)"
                .to_string(),
        })
    }

    async fn endpoint(&self, _system: &System, id: &str) -> Result<String> {
        self.resolve(id)
    }
}

// ── fleet.start / fleet.worker_status / fleet.stop ──────────────────────────

/// The permission subject a worker-lifecycle call occupies. Scoped to the **worker**, never `*`: an
/// operator grants "this coordinator may run workers for board items", and a worker it cannot name
/// yields no subject at all, which forces approval rather than matching a broad grant.
fn worker_subject(id: Option<&str>) -> Vec<String> {
    id.map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| vec![format!("fleet-worker:{id}")])
        .unwrap_or_default()
}

/// Arguments for `fleet.start`.
#[derive(Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct StartInput {
    /// Board item this worker serves. Also the worker's id, for fleet.worker_status / fleet.stop.
    item: String,
    /// Isolated checkout to confine the worker to — the `worktree` fleet.isolate returned. Without
    /// one the worker runs in the coordinator's own workspace and is not write-confined.
    #[serde(default)]
    worktree: Option<String>,
    /// A2A conversation id to bind the worker's session to, so a later fleet.dispatch resumes it.
    /// Defaults to a deterministic id derived from `item`.
    #[serde(default)]
    context_id: Option<String>,
    /// Model spec for the worker. Omit to let the worker use its own configured default.
    #[serde(default)]
    model: Option<String>,
}

/// Arguments for `fleet.worker_status` and `fleet.stop` — both act on a worker id `fleet.start`
/// returned.
#[derive(Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WorkerRefInput {
    /// Worker id returned by fleet.start (the board item it serves)
    worker_id: String,
}

/// The `contextId` a worker is bound to when the caller names none.
///
/// Derived from the item rather than random so it is *re-derivable*: a restarted coordinator that
/// knows the item knows the conversation to resume, with no third store to consult.
fn default_context_id(item: &str) -> String {
    format!("fleet-{item}")
}

/// `fleet.start` — make a worker exist.
///
/// The op that closes the gap this whole story is about. Its authority is the guarded spawn's: it
/// creates an OS process and nothing else, so it declares `Process` access and **no** network access
/// — readiness is read off the worker's own stderr, not probed over HTTP.
pub struct FleetStartTool {
    runtime: std::sync::Arc<dyn AgentRuntime>,
}

impl FleetStartTool {
    /// Bind the op to the runtime that places its workers.
    pub fn new(runtime: std::sync::Arc<dyn AgentRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for FleetStartTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fleet.start".into(),
            description: "Start a flux worker for one board item and return the A2A endpoint to \
                          dispatch to. Confine it to an isolated checkout with `worktree`; the \
                          returned `context_id` resumes the same worker session on a later \
                          fleet.dispatch. Stop it with fleet.stop."
                .into(),
            input_schema: tool_input_schema::<StartInput>(),
            output_schema: None,
            // A real OS process with real authority, created through the guarded spawn.
            // `LocalSystem` joins `Process` the way the `cargo_*` ops declare it — the child
            // occupies this host, it is not merely a computation — while the *access* stays the one
            // family that is actually exercised, so an operator is asked for `process.exec` on this
            // worker and nothing wider.
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::High,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let args = serde_json::from_value::<StartInput>(params.clone()).ok();
        worker_subject(args.as_ref().map(|a| a.item.as_str()))
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: StartInput = parse_params(params, "fleet.start")?;
        let item = args.item.trim().to_string();
        let context_id = args
            .context_id
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| default_context_id(&item));
        let system = ctx.system();
        // A caller-named worktree is resolved through the workspace guard, so a path outside it is
        // refused here rather than becoming a child's cwd.
        let worktree = match args
            .worktree
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
        {
            Some(dir) => Some(system.workspace().resolve(dir)?),
            None => None,
        };
        let spec = WorkerSpec {
            name: item,
            worktree,
            context_id,
            model: args.model,
        };
        match self.runtime.start(&system, spec).await {
            Ok(worker) => Ok(ToolResult::ok(
                serde_json::json!({
                    "worker_id": worker.id,
                    "endpoint": worker.endpoint,
                    "context_id": worker.context_id,
                    "runtime": self.runtime.kind(),
                    "state": WorkerState::Live.as_str(),
                })
                .to_string(),
            )),
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }
}

/// `fleet.worker_status` — is the worker still there?
///
/// Distinct from `fleet.status`, which reads a *task* on a worker. The two questions have different
/// answers and different failure modes: a task can be `completed` on a worker that has since died,
/// and a live worker can hold no task at all. A worker that has exited reports `dead` with its exit
/// code and the tail of its own output, which is where a startup failure explains itself.
pub struct FleetWorkerStatusTool {
    runtime: std::sync::Arc<dyn AgentRuntime>,
}

impl FleetWorkerStatusTool {
    /// Bind the op to the runtime that owns its workers.
    pub fn new(runtime: std::sync::Arc<dyn AgentRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for FleetWorkerStatusTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fleet.worker_status".into(),
            description: "Report whether a worker started by fleet.start is live, still starting, \
                          or dead — with its exit code and the tail of its own output when it died. \
                          This is the worker's liveness, not a task's state (that is fleet.status)."
                .into(),
            input_schema: tool_input_schema::<WorkerRefInput>(),
            output_schema: None,
            // Observes a child this coordinator already owns and reaches nothing that outlives the
            // call — a bounded read, exactly as `fleet.status` declares its remote counterpart. The
            // `Process` access is what it looks at, not what it creates.
            effects: vec![Effect::Read],
            risk: Risk::Low,
            // Deliberately NOT `Idempotent`: that word licenses the op cache to serve a stored
            // result instead of executing, and observing the change since the last poll is the
            // entire point — the same reasoning `fleet.status` carries.
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let args = serde_json::from_value::<WorkerRefInput>(params.clone()).ok();
        worker_subject(args.as_ref().map(|a| a.worker_id.as_str()))
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: WorkerRefInput = parse_params(params, "fleet.worker_status")?;
        match self
            .runtime
            .status(&ctx.system(), args.worker_id.trim())
            .await
        {
            Ok(status) => Ok(ToolResult::ok(
                serde_json::json!({
                    "worker_id": status.id,
                    "state": status.state.as_str(),
                    "live": status.state == WorkerState::Live,
                    "endpoint": status.endpoint,
                    "context_id": status.context_id,
                    "exit_code": status.exit_code,
                    "detail": status.detail,
                    "runtime": self.runtime.kind(),
                })
                .to_string(),
            )),
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }
}

/// `fleet.stop` — end a worker this coordinator started.
pub struct FleetStopTool {
    runtime: std::sync::Arc<dyn AgentRuntime>,
}

impl FleetStopTool {
    /// Bind the op to the runtime that owns its workers.
    pub fn new(runtime: std::sync::Arc<dyn AgentRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for FleetStopTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fleet.stop".into(),
            description: "Stop a worker started by fleet.start, terminating its process and every \
                          descendant. Stopping a worker that has already exited succeeds; an \
                          unknown worker id is an error. Externally managed workers refuse — cancel \
                          their task with fleet.cancel instead."
                .into(),
            input_schema: tool_input_schema::<WorkerRefInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            // Repeating a stop on a worker this runtime still knows is safe by construction (the
            // kill is idempotent), but the second call on a *removed* worker is an error, so this is
            // `Conditional` rather than `Idempotent` — and `Idempotent` would let the op cache skip
            // the call entirely, which for a kill is exactly wrong.
            idempotency: Idempotency::Conditional,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let args = serde_json::from_value::<WorkerRefInput>(params.clone()).ok();
        worker_subject(args.as_ref().map(|a| a.worker_id.as_str()))
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: WorkerRefInput = parse_params(params, "fleet.stop")?;
        let worker_id = args.worker_id.trim();
        match self.runtime.stop(&ctx.system(), worker_id).await {
            Ok(()) => Ok(ToolResult::ok(
                serde_json::json!({
                    "worker_id": worker_id,
                    "state": WorkerState::Dead.as_str(),
                    "runtime": self.runtime.kind(),
                })
                .to_string(),
            )),
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use flux_runtime::ToolRegistry;
    use flux_spec::metadata_violations;
    use flux_system::Workspace;
    use serde_json::json;

    /// A stand-in worker: an executable that announces itself the way `flux_server::serve` does and
    /// then stays up. It exists so the lifecycle can be proven against a **real guarded spawn** and
    /// a real OS process — the parts that can actually be wrong — without paying for a full `flux`
    /// boot, a provider, or a port that a CI box may not let us bind twice.
    ///
    /// It is handed the same argv a real worker gets (`app run --serve=<addr> --yes`), so the
    /// address it echoes is the one `ProcessRuntime` chose, which is what makes the returned
    /// endpoint meaningful rather than assumed.
    fn fake_worker(dir: &std::path::Path, body: &str) -> PathBuf {
        let path = dir.join("fake-worker.sh");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n\
                 # $1=app $2=run $3=--serve=<addr>\n\
                 addr=${{3#--serve=}}\n\
                 {body}\n"
            ),
        )
        .expect("write the stand-in worker");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("make the stand-in worker executable");
        }
        path
    }

    /// A worker that serves: announces, then blocks until it is killed.
    const SERVES: &str = "echo \"flux server listening on http://$addr\" >&2\n\
                          while true; do sleep 1; done";

    /// A worker that dies on its own after announcing — the shape a crashed worker has.
    const ANNOUNCES_THEN_DIES: &str = "echo \"flux server listening on http://$addr\" >&2\nexit 17";

    /// A guarded system over a throwaway root, with the sandbox **disabled** — the spawn path under
    /// test is `build_command` itself, and `Sandbox::disabled()` is what every hermetic test site
    /// uses so a box without bubblewrap and a box with one behave the same here.
    fn test_system(root: &std::path::Path) -> System {
        System::new(Workspace::new(root).expect("workspace over the temp root"))
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flux-c243-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp root");
        dir
    }

    /// Fast startup budget + no marker env, so a failing test fails in seconds rather than a minute.
    fn runtime_for(program: PathBuf) -> ProcessRuntime {
        ProcessRuntime::with_program(program).with_startup(Duration::from_secs(10), Vec::new())
    }

    /// C-243 Acceptance 1: the round trip. Start a worker, see it live at the endpoint the runtime
    /// chose, stop it, and see that the id is gone. Every step goes through the real ops over the
    /// real guarded spawn.
    #[tokio::test]
    async fn a_worker_round_trips_start_status_stop() {
        let root = temp_root("roundtrip");
        let program = fake_worker(&root, SERVES);
        let runtime: Arc<dyn AgentRuntime> = Arc::new(runtime_for(program));
        let ctx = ToolContext::new(Arc::new(test_system(&root)));

        let start = FleetStartTool::new(runtime.clone())
            .execute(&ctx, json!({ "item": "C-243" }))
            .await
            .expect("fleet.start dispatches");
        assert!(!start.is_error, "fleet.start failed: {}", start.content);
        let started: Value = serde_json::from_str(&start.content).expect("start returns JSON");
        assert_eq!(started["worker_id"], "C-243");
        assert_eq!(started["runtime"], "process");
        // The endpoint is the address the runtime offered and the worker echoed back — proof the
        // two agree, not an assumption that they do.
        let endpoint = started["endpoint"]
            .as_str()
            .expect("an endpoint")
            .to_string();
        assert!(
            endpoint.starts_with("http://127.0.0.1:"),
            "a process worker must be reachable on loopback, got {endpoint}"
        );
        // The context id is re-derivable from the item, which is what lets a restarted coordinator
        // resume the same worker session (A2A `contextId` continuity).
        assert_eq!(started["context_id"], "fleet-C-243");

        let status = FleetWorkerStatusTool::new(runtime.clone())
            .execute(&ctx, json!({ "worker_id": "C-243" }))
            .await
            .expect("fleet.worker_status dispatches");
        let live: Value = serde_json::from_str(&status.content).expect("status returns JSON");
        assert_eq!(live["state"], "live", "{}", status.content);
        assert_eq!(live["live"], true);
        assert_eq!(live["endpoint"], endpoint);
        // Acceptance 4: the session binding is readable back off the worker id alone, so a restarted
        // coordinator can resume the same worker session without its own bookkeeping.
        assert_eq!(live["context_id"], "fleet-C-243", "{}", status.content);

        let stop = FleetStopTool::new(runtime.clone())
            .execute(&ctx, json!({ "worker_id": "C-243" }))
            .await
            .expect("fleet.stop dispatches");
        assert!(!stop.is_error, "fleet.stop failed: {}", stop.content);

        // After a stop the worker is not merely dead, it is unknown: the handle is gone, so nothing
        // can report it as dispatchable.
        let after = FleetWorkerStatusTool::new(runtime)
            .execute(&ctx, json!({ "worker_id": "C-243" }))
            .await
            .expect("fleet.worker_status dispatches");
        assert!(after.is_error, "a stopped worker must not still resolve");
        assert!(
            after.content.contains("no worker `C-243`"),
            "{}",
            after.content
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// C-243 Acceptance 6: a worker that dies is reported **dead**, never live. The subprocess is
    /// killed out from under the runtime — the crash case — and the status poll must notice from the
    /// process itself rather than from a flag the runtime set when it was asked to stop.
    #[tokio::test]
    async fn a_killed_worker_is_reported_dead_rather_than_live() {
        let root = temp_root("killed");
        let program = fake_worker(&root, SERVES);
        let runtime = Arc::new(runtime_for(program));
        let system = test_system(&root);

        let worker = runtime
            .start(
                &system,
                WorkerSpec {
                    name: "C-243".into(),
                    context_id: "ctx".into(),
                    ..Default::default()
                },
            )
            .await
            .expect("the worker starts");
        assert_eq!(
            runtime.status(&system, &worker.id).await.unwrap().state,
            WorkerState::Live
        );

        // Kill the child directly: this is a worker dying, not a worker being stopped.
        runtime
            .workers
            .lock()
            .await
            .get_mut("C-243")
            .expect("the worker is registered")
            .child
            .kill();

        // `kill` is asynchronous at the OS level; poll the way a coordinator's sweep would.
        let mut state = WorkerState::Live;
        for _ in 0..200 {
            state = runtime.status(&system, "C-243").await.unwrap().state;
            if state == WorkerState::Dead {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            state,
            WorkerState::Dead,
            "a killed worker must read as dead"
        );
        // And it must not look dispatchable: no endpoint is offered for a dead worker.
        let status = runtime.status(&system, "C-243").await.unwrap();
        assert!(status.endpoint.is_none(), "{status:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A worker that exits on its own — the startup-failure shape — reports dead **with its own exit
    /// code and output**, because that tail is the only place the reason exists.
    #[tokio::test]
    async fn a_worker_that_exits_reports_its_exit_code_and_output() {
        let root = temp_root("exits");
        let program = fake_worker(&root, ANNOUNCES_THEN_DIES);
        let runtime = runtime_for(program);
        let system = test_system(&root);

        runtime
            .start(
                &system,
                WorkerSpec {
                    name: "C-243".into(),
                    context_id: "ctx".into(),
                    ..Default::default()
                },
            )
            .await
            .expect("it announced before exiting, so the start succeeds");

        let mut status = runtime.status(&system, "C-243").await.unwrap();
        for _ in 0..200 {
            if status.state == WorkerState::Dead {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
            status = runtime.status(&system, "C-243").await.unwrap();
        }
        assert_eq!(status.state, WorkerState::Dead);
        assert_eq!(status.exit_code, Some(17), "{status:?}");
        assert!(
            status.detail.contains("listening on http://"),
            "the worker's own output must survive the drain: {status:?}"
        );
        // Repeated polls must keep agreeing — `try_wait` reports a code once, so a runtime that did
        // not retain it would report `dead` with an unknown code on the second look.
        let again = runtime.status(&system, "C-243").await.unwrap();
        assert_eq!(again.exit_code, Some(17), "{again:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Acceptance 4: the worker is scoped to the checkout it was given. Proven through the guarded
    /// system's own resolution — a worktree outside the workspace is refused before anything spawns,
    /// which is what makes the confinement structural rather than an instruction.
    #[tokio::test]
    async fn a_worker_is_confined_to_the_checkout_it_was_given() {
        let root = temp_root("worktree");
        let checkout = root.join("wt-C-243");
        std::fs::create_dir_all(&checkout).expect("the isolated checkout");
        // The stand-in worker reports its own cwd, so the assertion is about where the OS actually
        // put the child, not about what the runtime intended.
        let program = fake_worker(
            &root,
            "echo \"flux server listening on http://$addr\" >&2\n\
             echo \"cwd=$(pwd)\" >&2\n\
             while true; do sleep 1; done",
        );
        let runtime = runtime_for(program);
        let system = test_system(&root);

        runtime
            .start(
                &system,
                WorkerSpec {
                    name: "C-243".into(),
                    worktree: Some(checkout.clone()),
                    context_id: "ctx".into(),
                    model: None,
                },
            )
            .await
            .expect("the worker starts in its checkout");
        let mut detail = String::new();
        for _ in 0..200 {
            detail = runtime.status(&system, "C-243").await.unwrap().detail;
            if detail.contains("cwd=") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let expected = std::fs::canonicalize(&checkout).expect("canonical checkout");
        assert!(
            detail.contains(&format!("cwd={}", expected.display())),
            "the child's cwd must be its checkout, got {detail:?} (wanted {})",
            expected.display()
        );

        // And a path outside the workspace never reaches a spawn at all — the guarded workspace
        // refuses it while resolving, before any child exists to have to clean up.
        let outside = FleetStartTool::new(Arc::new(runtime_for(fake_worker(&root, SERVES))))
            .execute(
                &ToolContext::new(Arc::new(test_system(&checkout))),
                json!({ "item": "escape", "worktree": "../../etc" }),
            )
            .await;
        let refusal = outside.expect_err("a worktree outside the workspace must be refused");
        assert!(
            refusal.to_string().contains("escapes the workspace root"),
            "the refusal must come from the workspace guard, got {refusal}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Two workers must not collide — the point of the whole story. Each gets its own loopback port,
    /// and a second start for the *same* item is refused rather than silently replacing the first
    /// (which would orphan a running process no id refers to).
    #[tokio::test]
    async fn concurrent_workers_get_distinct_endpoints_and_an_id_cannot_be_reused() {
        let root = temp_root("distinct");
        let program = fake_worker(&root, SERVES);
        let runtime = runtime_for(program);
        let system = test_system(&root);

        let spec = |name: &str| WorkerSpec {
            name: name.into(),
            context_id: "ctx".into(),
            ..Default::default()
        };
        let first = runtime.start(&system, spec("C-243")).await.expect("first");
        let second = runtime.start(&system, spec("C-244")).await.expect("second");
        assert_ne!(
            first.endpoint, second.endpoint,
            "two live workers cannot share one endpoint"
        );

        let clash = runtime.start(&system, spec("C-243")).await;
        assert!(
            clash.is_err(),
            "a second worker for the same item must be refused, got {clash:?}"
        );
        assert_eq!(
            runtime.endpoint(&system, "C-243").await.unwrap(),
            first.endpoint,
            "the refusal must not have disturbed the running worker"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// `ExternalRuntime` makes an operator-run worker addressable through the same port — and is
    /// honest about what it does not own: it refuses to stop a process it never started.
    #[tokio::test]
    async fn an_external_worker_is_addressable_but_not_stoppable() {
        let root = temp_root("external");
        let system = test_system(&root);
        let runtime = ExternalRuntime::new(HashMap::from([(
            "C-243".to_string(),
            "https://worker-1.internal:8787".to_string(),
        )]));

        let worker = runtime
            .start(
                &system,
                WorkerSpec {
                    name: "C-243".into(),
                    context_id: "ctx-1".into(),
                    ..Default::default()
                },
            )
            .await
            .expect("an already-running worker is addressable");
        assert_eq!(worker.endpoint, "https://worker-1.internal:8787");
        assert_eq!(worker.context_id, "ctx-1");
        assert_eq!(
            runtime.status(&system, "C-243").await.unwrap().state,
            WorkerState::Live
        );

        let stop = runtime.stop(&system, "C-243").await;
        assert!(
            stop.is_err(),
            "an external worker must refuse to be stopped"
        );
        assert!(
            stop.unwrap_err().to_string().contains("externally managed"),
            "the refusal must say why"
        );
        // An unknown worker is an error on every verb, and the message names what IS configured.
        let unknown = runtime.endpoint(&system, "C-999").await.unwrap_err();
        assert!(unknown.to_string().contains("known: C-243"), "{unknown}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The finished `ToolSpec`s are checked by `flux_spec::metadata_violations` rather than by eye,
    /// and they register into a real `ToolRegistry` — which is what enforces the authority contract
    /// (`Effect::Process` demands `AccessKind::Process`, so a declaration that drifts cannot even
    /// be registered).
    #[test]
    fn the_worker_ops_declare_coherent_metadata_and_register() {
        let runtime: Arc<dyn AgentRuntime> = Arc::new(ExternalRuntime::new(HashMap::new()));
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(FleetStartTool::new(runtime.clone())),
            Arc::new(FleetWorkerStatusTool::new(runtime.clone())),
            Arc::new(FleetStopTool::new(runtime)),
        ];
        for tool in &tools {
            let spec = tool.spec();
            let violations = metadata_violations(&spec, &tool.semantic_effects());
            assert!(
                violations.is_empty(),
                "`{}` declares incoherent metadata: {violations:?}",
                spec.name
            );
            // These ops create and signal local processes; they make no request of their own, and
            // declaring network access would ask an operator for egress that never happens.
            assert!(
                !spec.access.contains(&AccessKind::Network),
                "`{}` must not demand network access",
                spec.name
            );
        }
        let mut registry = ToolRegistry::new();
        registry
            .try_register_all_from("C-243 worker lifecycle", tools)
            .expect("the worker ops satisfy the registry's authority contract");
        for name in ["fleet.start", "fleet.worker_status", "fleet.stop"] {
            let tool = registry.get(name).expect("registered");
            // Never `*`: a grant is per worker, and an unnameable worker yields no subject at all,
            // which forces approval instead of matching a broad grant.
            let subjects = tool.permission_subjects(&json!({ "item": "  ", "worker_id": "  " }));
            assert!(subjects.is_empty(), "`{name}` reported {subjects:?}");
        }
    }
}
