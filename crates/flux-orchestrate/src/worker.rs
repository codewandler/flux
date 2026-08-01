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
//! * **Readiness is the worker's own word.** `flux-server` prints its serving announcement to stderr
//!   once bound, and [`System::spawn_background`] pipes stderr into a capped drained buffer. So
//!   `start` waits for that line — no readiness probe, hence no egress, hence these ops declare no
//!   network access at all. The wording is not repeated here: `flux-server` is L6 and this crate is
//!   L3, so the pair would be unpinnable from either side. Both go through
//!   [`flux_core::readiness`], which is L0 and therefore legal for both (C-277).
//!
//! ## [`ExternalRuntime`] — a worker somebody else runs
//!
//! The degenerate implementation, and the reason the port has value on day one: an operator-run
//! worker becomes addressable through the same four verbs, so a coordinator Program never branches
//! on how its workers came to exist. It deliberately refuses to `stop` — it did not start the
//! process and must not pretend it can end it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_policy::{ResourceKind, ResourceRef};
use flux_runtime::{
    AgentRuntime, AuthorityRequirement, Tool, ToolContext, ToolResult, Worker, WorkerSpec,
    WorkerState, WorkerStatus,
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

/// How much of a worker's own output is kept as the `detail` a status poll reports. Enough for a
/// startup failure to explain itself, bounded so a chatty worker cannot grow this map without limit
/// (`ManagedChild` already caps what one drain returns; this caps what is *retained*).
const RETAINED_LOG: usize = 8 * 1024;

/// How deep this `flux` sits in a chain of fleet workers: absent or `0` in a coordinator a human
/// started, `n+1` in a worker started by a coordinator at depth `n`.
///
/// **A marker flux sets on its own children, not a knob.** It travels only through
/// [`ProcessRuntime`]'s explicit env override, and `build_command` *clears* the child's environment
/// before applying those overrides — so a model cannot inject or lower it, and a worker cannot mint
/// itself more depth than its parent granted. Raising it by hand only ever shrinks the budget.
const DEPTH_ENV: &str = "FLUX_FLEET_DEPTH";

/// How many fleet generations may exist below a human-started coordinator. `1` means a coordinator
/// starts workers and those workers start none.
///
/// The bound exists because a worker is spawned with `--yes` and its catalog contains `fleet.start`:
/// its approver is `AllowApprover`, so the coordinator's first start is gated and every start below
/// it would not be. That is the same unbounded-recursion hazard `LocalSpawner` bounds with
/// `max_depth` (`crate::SpawnRequest`, default 1), answered the same way and with the same default.
const DEFAULT_MAX_FLEET_DEPTH: u32 = 1;

/// How many workers one coordinator may hold at once. A second bound with a different shape: depth
/// stops a chain, this stops a fan. Sized to the loopback range a runtime can actually place workers
/// in, so exhausting it reports a budget rather than a port scan.
const DEFAULT_MAX_WORKERS: usize = 16;

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

/// What one runtime knows about its workers. One lock over both halves, because "is this id taken?"
/// must be answered against *live and starting* workers together — two locks would let two concurrent
/// starts for one item both pass the check.
#[derive(Default)]
struct WorkerTable {
    live: HashMap<String, ProcessWorker>,
    /// Ids whose spawn+readiness wait is in flight. Held only as a reservation, so the lock itself is
    /// never held across the wait.
    starting: HashSet<String>,
}

impl WorkerTable {
    fn occupied(&self, id: &str) -> bool {
        self.live.contains_key(id) || self.starting.contains(id)
    }

    fn count(&self) -> usize {
        self.live.len() + self.starting.len()
    }
}

/// A fleet worker as a **child `flux` process on this machine**, started through the guarded spawn.
pub struct ProcessRuntime {
    /// The `flux` binary a worker is started from.
    program: PathBuf,
    /// First port of this runtime's loopback range.
    base_port: u16,
    /// Cursor over the loopback port range, so N concurrent starts do not all begin at the same
    /// candidate and serialize on each other's bind failures.
    next_port: AtomicU16,
    /// Live and starting workers. A `tokio` mutex, but **never held across an await**: a `start`
    /// reserves its id, releases, then spawns and waits. Holding it across the readiness wait would
    /// serialize every start and block `fleet.stop` / `fleet.worker_status` on *other* workers behind
    /// one stalling one — a coordinator could not sweep or cancel its own wave.
    workers: tokio::sync::Mutex<WorkerTable>,
    ready_timeout: Duration,
    /// How deep this runtime's `flux` already sits in a chain of workers (see [`DEPTH_ENV`]).
    depth: u32,
    /// How many generations may exist below a human-started coordinator (see
    /// [`DEFAULT_MAX_FLEET_DEPTH`]).
    max_depth: u32,
    /// How many workers this runtime may hold at once.
    max_workers: usize,
    /// Extra environment for every worker, applied on top of `build_command`'s cleared, allow-listed
    /// base. Two things this does **not** get to carry, both enforced in [`ProcessRuntime::worker_env`]:
    /// the depth marker ([`DEPTH_ENV`]), appended after it so a host cannot widen its own budget, and
    /// anything sandbox-related (`flux_system::sandbox::SANDBOX_ENV_KEYS`), filtered out of it so a
    /// host cannot unconfine the workers it starts — the posture reaches a worker from the
    /// coordinator's resolved `Sandbox` via the guarded spawn, not from here.
    /// Nothing secret is forwarded either: a worker resolves its own provider
    /// credentials from its own configuration through the forwarded `HOME`, exactly as any other
    /// `flux` process does, and forwarding a credential from the coordinator's environment is the one
    /// thing the env-clear exists to prevent. This field is a seam so a host — and the tests below —
    /// can steer a non-default worker program without a second spawn path.
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
            base_port: DEFAULT_WORKER_BASE_PORT,
            next_port: AtomicU16::new(0),
            workers: tokio::sync::Mutex::new(WorkerTable::default()),
            ready_timeout: DEFAULT_READY_TIMEOUT,
            // Read from the host environment, not from a parameter: this is what a *parent* granted
            // this process, and nothing model-reachable may restate it.
            depth: std::env::var(DEPTH_ENV)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0),
            max_depth: DEFAULT_MAX_FLEET_DEPTH,
            max_workers: DEFAULT_MAX_WORKERS,
            env: Vec::new(),
        }
    }

    /// Override the loopback port range's start — for an operator whose default range is taken.
    pub fn with_base_port(mut self, base: u16) -> Self {
        self.base_port = base;
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

    /// Override the nesting budget and the concurrent-worker cap. Host configuration; a worker can
    /// never call this, because it does not construct its own runtime.
    pub fn with_bounds(mut self, max_depth: u32, max_workers: usize) -> Self {
        self.max_depth = max_depth;
        self.max_workers = max_workers;
        self
    }

    /// This runtime's own depth in the worker chain — `0` in a human-started coordinator.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// The next port candidate, wrapping inside the span so a long-lived coordinator that has started
    /// and stopped many workers keeps reusing the same bounded range.
    fn next_candidate(&self) -> u16 {
        let taken = self.next_port.fetch_add(1, Ordering::Relaxed);
        self.base_port.wrapping_add(taken % PORT_SPAN)
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
            //
            // Be exact about what it costs. Path-scoped write authority confines the worker's
            // *filesystem* writes to its own checkout (via the re-rooted system below), but `--yes`
            // installs an `AllowApprover`, and the worker's catalog contains `fleet.start` — so
            // **process creation** is auto-approved inside it. Left alone that is unbounded
            // recursion: a coordinator's first start is gated, and every start below it is not.
            // [`DEPTH_ENV`] is what bounds it, and the depth check in `start` is what enforces it.
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

    /// The environment one worker is started with: the host seam's `env` and the depth this worker
    /// is granted.
    ///
    /// Ordering is load-bearing — `apply_safe_env` applies these last and lets later entries win — so
    /// the depth marker is appended after everything else. A `with_startup` caller therefore cannot
    /// overwrite the budget by passing `FLUX_FLEET_DEPTH` itself.
    ///
    /// The coordinator's **sandbox posture is deliberately not here**. A worker is a full `flux`
    /// that spawns processes of its own and must confine them exactly as its parent would, but that
    /// is the guarded spawn's job: `sandbox::posture_env` renders the posture from `system`'s
    /// resolved `Sandbox` and `apply_safe_env` applies it *before* this env (C-276). This method
    /// used to hand-roll the same forwarding out of the **ambient environment** and push it into the
    /// caller-override slot, which lands after — so a coordinator pinned non-`Off` under an ambient
    /// `FLUX_SANDBOX=off` (the shape `System::with_sandbox` exists to create) handed its worker
    /// `flux-cli`'s kill switch, strictly less confined than forwarding nothing (C-282).
    ///
    /// The startup env is filtered for the same reason rather than merely documented: a call site
    /// has no legitimate reason to push a worker's posture **downward**, so the slot is closed, the
    /// way [`DEPTH_ENV`] already is. Filtered against `sandbox::SANDBOX_ENV_KEYS`, not a local copy,
    /// so it cannot drift from what the spawn actually forwards.
    ///
    /// That list covers `FLUX_SANDBOXED` as well as the posture. Since C-289 the marker is out of a
    /// caller's reach at the spawn itself — `build_command` renders it after the overrides in both
    /// directions, stamping it only when something genuinely confines the worker and removing it
    /// otherwise — so this filter is no longer what stands between a startup env and a worker that
    /// believes it is already confined. It stays because a `with_startup` caller naming the marker is
    /// describing a worker environment it cannot produce, and dropping it here keeps the two honest
    /// rather than letting the value vanish a layer down.
    fn worker_env(&self) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = self
            .env
            .iter()
            .filter(|(key, _)| !flux_system::sandbox::is_sandbox_env_key(key))
            .cloned()
            .collect();
        env.push((DEPTH_ENV.to_string(), (self.depth + 1).to_string()));
        env
    }

    /// Spawn a worker onto the first loopback port it can actually bind, and return it once it has
    /// announced that it is serving.
    ///
    /// Split out of `start` so the whole spawn-and-wait runs with **no lock held** — `start` reserves
    /// the id, calls this, then re-locks to publish. Every failure path drops `child`, whose
    /// `kill_on_drop` stops the process, so no exit from here leaks one.
    async fn place(
        &self,
        system: &System,
        id: &str,
        spec: &WorkerSpec,
    ) -> Result<(String, String, ManagedChild)> {
        let env = self.worker_env();
        let mut refused = Vec::new();
        for _ in 0..PORT_SPAN {
            let port = self.next_candidate();
            let addr = format!("127.0.0.1:{port}");
            let argv = self.argv(&addr, spec);
            let mut child = system.spawn_background(&argv, &env)?;
            match self.await_ready(&mut child).await {
                Readiness::Serving(log) => return Ok((format!("http://{addr}"), log, child)),
                // The child's own bind is the port-availability test; a conflict just means the next
                // candidate. `child` is dropped here, so nothing is left behind either way.
                Readiness::Exited { log, .. } if port_conflict(&log) => refused.push(port),
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
             {}..+{PORT_SPAN} range were all in use ({refused:?})",
            refused.len(),
            self.base_port
        )))
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
            if flux_core::readiness::announces_serving(&log) {
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
        // The nesting bound, before anything else: a worker that may not start workers must not even
        // get as far as reserving an id.
        if self.depth >= self.max_depth {
            return Err(Error::Other(format!(
                "fleet.start: refusing to start worker `{id}` — this flux is itself fleet worker \
                 generation {} of a maximum {}, and a worker runs with `--yes`, so allowing it to \
                 start workers of its own would auto-approve process creation without bound. The \
                 coordinator a human started is the only one that may open a wave",
                self.depth, self.max_depth
            )));
        }
        // A worker started under a network-isolated sandbox binds inside its own network namespace,
        // so the endpoint this op would hand back is unreachable from here — and the worker has no
        // egress to reach a provider either. Refuse: the port contract is that `start` returns an
        // *addressable* worker, and returning a live-looking endpoint nothing can reach would make
        // this op work interactively and fail silently in exactly the automation it exists for.
        //
        // The wrapping decision belongs to the coordinator's own sandbox, so it is knowable here
        // without probing: `spawn_background` passes `Confinement::Sandboxed`, and `bubblewrap_argv`
        // adds `--unshare-net` precisely when the policy closes the network.
        if system.sandbox().is_active() && !system.sandbox().settings().network {
            return Err(Error::Other(format!(
                "fleet.start: refusing to start worker `{id}` — this coordinator's sandbox is \
                 active with the network closed ({}), so the worker would bind inside its own \
                 network namespace and no endpoint could reach it. Re-run the coordinator with the \
                 sandbox network open (FLUX_SANDBOX_NET=1, or `[sandbox] network = true`), or start \
                 the worker from a coordinator that is not network-isolated",
                system.sandbox().describe()
            )));
        }
        // Scope the worker to its checkout BEFORE spawning: the re-rooted system carries both the
        // child's cwd and the sandbox's writable set, so this is the confinement, not a hint.
        //
        // `rerooted` is the *whole* guard on this path, deliberately. `Workspace::with_root`
        // canonicalizes and requires the directory to exist, and it does **not** require the new root
        // to sit under the old one — which is the only reason `fleet.isolate`'s checkout can be
        // accepted at all: `System::allocate_worktree_dir` creates it outside every workspace root on
        // purpose, under the base that system carries (`$FLUX_WORKTREE_DIR`, else
        // `$HOME/.flux/worktrees`, unless pinned — C-391). An earlier revision also ran the path
        // through `Workspace::resolve`, the write-path resolver, which admits only the primary and
        // `@named` roots — so it rejected the one input this parameter exists to take.
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

        // Reserve the id, then release the lock. Everything below awaits, and holding this across the
        // readiness wait would block every other verb on every other worker.
        {
            let mut workers = self.workers.lock().await;
            if workers.occupied(id) {
                return Err(Error::Other(format!(
                    "fleet.start: a worker for `{id}` is already running or starting — stop it with \
                     fleet.stop before starting another, or dispatch to the one that exists"
                )));
            }
            if workers.count() >= self.max_workers {
                return Err(Error::Other(format!(
                    "fleet.start: refusing to start worker `{id}` — this coordinator already holds \
                     {} of a maximum {} workers; stop one with fleet.stop first",
                    workers.count(),
                    self.max_workers
                )));
            }
            workers.starting.insert(id.to_string());
        }
        let placed = self.place(system, id, &spec).await;
        // Release the reservation on **every** path, then publish on success. Written as one
        // re-lock over an already-computed outcome so no `?` can escape between the two.
        let mut workers = self.workers.lock().await;
        workers.starting.remove(id);
        match placed {
            Ok((endpoint, log, child)) => {
                workers.live.insert(
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
                Ok(Worker {
                    id: id.to_string(),
                    endpoint,
                    context_id: spec.context_id,
                })
            }
            Err(e) => Err(e),
        }
    }

    async fn stop(&self, _system: &System, id: &str) -> Result<()> {
        let mut workers = self.workers.lock().await;
        // Removing the handle kills the child on drop, so the stop is the removal — there is no
        // window in which the map claims a worker this runtime has already signalled.
        match workers.live.remove(id) {
            Some(mut worker) => {
                worker.child.kill();
                Ok(())
            }
            // A worker still inside its readiness wait is deliberately NOT stoppable: its handle
            // belongs to the in-flight `place` call, which kills it on any failure. Saying so beats
            // reporting "no such worker" for something the caller just asked to start.
            None if workers.starting.contains(id) => Err(Error::Other(format!(
                "fleet.stop: worker `{id}` is still starting — wait for fleet.start to return, \
                 which either publishes the worker or stops it"
            ))),
            None => Err(Error::Other(format!(
                "fleet.stop: no worker `{id}` was started by this coordinator"
            ))),
        }
    }

    async fn status(&self, _system: &System, id: &str) -> Result<WorkerStatus> {
        let mut workers = self.workers.lock().await;
        if !workers.live.contains_key(id) && workers.starting.contains(id) {
            return Ok(WorkerStatus {
                id: id.to_string(),
                state: WorkerState::Starting,
                // No endpoint yet: which port the worker took is not settled until it binds, and
                // offering a guess is exactly the not-dispatchable-looking thing `Starting` is for.
                endpoint: None,
                context_id: None,
                exit_code: None,
                detail: "the worker is still inside its readiness wait".to_string(),
            });
        }
        let worker = workers
            .live
            .get_mut(id)
            .ok_or_else(|| Error::Other(format!("fleet.worker_status: no worker `{id}`")))?;
        // Drain before the liveness test, and retain: a worker that just died explains itself in
        // output that `read_output` yields exactly once.
        let (out, err) = worker.child.read_output();
        push_capped(&mut worker.log, &out);
        push_capped(&mut worker.log, &err);
        if flux_core::readiness::announces_serving(&worker.log) {
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
        // Only a *published* worker has an endpoint. One still inside its readiness wait has not
        // settled which port it took, so there is nothing truthful to return yet.
        workers
            .live
            .get(id)
            .map(|w| w.endpoint.clone())
            .ok_or_else(|| {
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

/// The checkout a start would give a worker write authority over, as its own grantable subject. A
/// different resource family from the worker id, which is why `authority_requirements` discriminates
/// rather than iterating.
fn worktree_subject(worktree: Option<&str>) -> Vec<String> {
    worktree
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
        .map(|dir| vec![dir.to_string()])
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
                          dispatch to. Pass the `worktree` fleet.isolate returned to confine the \
                          worker to that checkout; the returned `context_id` resumes the same worker \
                          session on a later fleet.dispatch. Stop it with fleet.stop. The endpoint \
                          is on loopback, which fleet.dispatch refuses unless the operator started \
                          flux with --allow-private-net."
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

    /// Two independently grantable subjects, kept apart by resource family exactly as
    /// `fleet.dispatch` keeps a worker origin apart from a board item (A-130): the **worker** this
    /// start creates, and the **checkout** it is given write authority over. A start with no
    /// `worktree` reports only the first, because then it grants no tree.
    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let args = serde_json::from_value::<StartInput>(params.clone()).ok();
        let mut subjects = worker_subject(args.as_ref().map(|a| a.item.as_str()));
        subjects.extend(worktree_subject(
            args.as_ref().and_then(|a| a.worktree.as_deref()),
        ));
        subjects
    }

    /// Derived here rather than from the declaration, for the reason A-130 records on
    /// `fleet.dispatch`: the declaration path applies every declared access kind to every subject, so
    /// it would demand `process.exec` on a *directory*. Each subject earns only the family that fits
    /// it — `process.exec` on the worker, `workspace.write` on the checkout the worker may write.
    fn authority_requirements(
        &self,
        params: &Value,
        _subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        let args = serde_json::from_value::<StartInput>(params.clone()).ok();
        let worker = worker_subject(args.as_ref().map(|a| a.item.as_str()));
        // An unnameable worker yields no subject — but never an empty requirement list, which would
        // mean this op demands nothing at all. `Executor::gate` walks the requirements for its policy
        // floor, so the fallback is the conservative wildcard, exactly as `fleet.dispatch` argues.
        let mut requirements = match worker.first() {
            Some(subject) => vec![AuthorityRequirement::new(
                "process.exec",
                ResourceRef::named(ResourceKind::Process, subject),
            )],
            None => vec![AuthorityRequirement::new(
                "process.exec",
                ResourceRef::any(ResourceKind::Process),
            )],
        };
        if let Some(dir) = args
            .as_ref()
            .and_then(|a| a.worktree.as_deref())
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            requirements.push(AuthorityRequirement::workspace_write(dir));
        }
        Ok(requirements)
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
        // Deliberately NOT run through `Workspace::resolve`. That is the write-path resolver, which
        // admits only the primary root and `@named` roots — and `fleet.isolate`'s checkout lives
        // outside every workspace root by design (`allocate_worktree_dir`), so resolving here refused
        // the exact input this parameter exists to accept. The guard is `System::rerooted` inside the
        // runtime: it canonicalizes, requires the directory to exist, and is the same seam a
        // context-local worktree transition uses. What keeps it from being a blank cheque is the
        // authority contract above — the checkout is a named `workspace.write` subject an operator
        // approves — plus an absolute path, so nothing is interpreted relative to a cwd the caller
        // cannot see.
        let worktree = match args
            .worktree
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
        {
            Some(dir) => {
                let path = std::path::Path::new(dir);
                if !path.is_absolute() {
                    return Err(Error::Other(format!(
                        "fleet.start: `worktree` must be an absolute path — `{dir}` is relative, and \
                         a worker's checkout is not resolved against this session's cwd. Pass the \
                         `worktree` fleet.isolate returned verbatim"
                    )));
                }
                Some(path.to_path_buf())
            }
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
                    // Said in the result, not only in the description: the endpoint this op just
                    // handed back is on loopback, and `fleet.dispatch` guards every caller-supplied
                    // URL through the SSRF guard, whose default refuses private addresses. Without
                    // this line the next call fails with a guard message that names no remedy.
                    "dispatch_note": "This endpoint is on loopback. fleet.dispatch resolves worker \
                                      URLs through the SSRF guard, which refuses private addresses \
                                      unless flux was started with --allow-private-net.",
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

    /// The committed stand-in worker (`tests/fixtures/stand-in-worker.sh`) — see its own header for
    /// why it is a fixture rather than a file these tests write, and for the marker protocol.
    ///
    /// It speaks the real worker argv and announces itself exactly as `flux_server::serve` does, so
    /// the lifecycle is proven against a **real guarded spawn** and a real OS process without booting
    /// a full `flux`.
    /// C-277: readiness is matched through the shared contract, not a literal copied from a crate
    /// this one may not depend on.
    ///
    /// `flux-server` is L6 and this crate is L3, so the pair was unpinnable from either side: a
    /// rewording of the server's line does not fail loudly here, it degrades `fleet.start` to its
    /// full 60-second readiness timeout and then reports a worker that never announced itself —
    /// which reads as a slow or hung worker rather than the broken contract it is.
    #[test]
    fn readiness_is_matched_through_the_shared_contract() {
        let source = include_str!("worker.rs");
        // Split so this assertion's own text is not the match it is looking for.
        let literal = ["listening on ", "http://"].concat();
        assert!(
            !source.contains(&literal),
            "this crate spells `flux-server`'s readiness wording itself, and the layering rule \
             means no test can check the two agree — match it with \
             `flux_core::readiness::announces_serving` instead."
        );
    }

    /// C-277: the stand-in worker is the third copy of the wording, and the dangerous one.
    ///
    /// Every `ProcessRuntime` lifecycle test below proves itself against this fixture rather than a
    /// real `flux`. If the server's announcement were reworded and the fixture were not, the fixture
    /// would still agree with the matcher and the whole suite would stay green while `fleet.start`
    /// timed out against every real worker — a guard tested against its own assumptions. Pinning the
    /// fixture to the shared contract is what turns a rewording into a failing test: change
    /// `flux_core::readiness::SERVING_MARKER` and this fails until the fixture is moved with it.
    #[test]
    fn the_stand_in_worker_announces_exactly_what_the_real_server_announces() {
        let fixture = std::fs::read_to_string(stand_in_worker()).unwrap();
        // The fixture interpolates the address it parsed out of its own argv.
        let expected = flux_core::readiness::serving_announcement("$addr");
        assert!(
            fixture.contains(&expected),
            "the stand-in worker no longer announces what `flux_server::serve` announces \
             ({expected:?}); every lifecycle test below would keep passing against a contract no \
             real worker honours"
        );
    }

    fn stand_in_worker() -> PathBuf {
        let path = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/stand-in-worker.sh"
        ));
        assert!(
            path.is_file(),
            "the committed stand-in worker is missing: {}",
            path.display()
        );
        path
    }

    /// Ask the stand-in worker in `checkout` to never announce — the shape of a worker that starts but
    /// never binds.
    fn never_announces(checkout: &std::path::Path) {
        std::fs::write(checkout.join("no-announce"), "").expect("the no-announce marker");
    }

    /// Ask the stand-in worker in `checkout` to exit with `code` right after announcing — the shape of
    /// a worker that crashes on its own.
    fn exits_with(checkout: &std::path::Path, code: i32) {
        std::fs::write(checkout.join("exit-code"), code.to_string()).expect("the exit-code marker");
    }

    /// A guarded system over a throwaway root, with the sandbox **disabled** — the spawn path under
    /// test is `build_command` itself, and `Sandbox::disabled()` is what every hermetic test site
    /// uses so a box without bubblewrap and a box with one behave the same here. The sandbox-posture
    /// tests below build their own `System` instead, precisely because this one cannot see that path.
    fn test_system(root: &std::path::Path) -> System {
        System::new(Workspace::new(root).expect("workspace over the test root"))
    }

    /// A throwaway directory **on the real disk**, under the workspace's own gitignored `target/`.
    ///
    /// Deliberately not `std::env::temp_dir()`: `/tmp` here is a 32G tmpfs shared with every other
    /// build on the box, and filling it wedges unrelated processes. `target/` is where build output
    /// already goes.
    fn test_root(tag: &str) -> PathBuf {
        let dir = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/c243-tests"
        ))
        .join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("test root");
        dir
    }

    /// Serializes the tests that mutate the process-global sandbox environment, and restores every
    /// key it touched on drop — panic-safe, so a failed assertion cannot leak a posture into a
    /// later test in the same process. Mirrors `flux_system::sandbox::EnvGuard`, which is
    /// `pub(crate)` there and so cannot be reused across the crate boundary.
    struct SandboxEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    static SANDBOX_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl SandboxEnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            // A poisoned lock still serializes correctly: the guard restores what it saved, so a
            // panicking test leaves the environment clean even though the mutex is marked.
            let lock = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = keys.iter().map(|&k| (k, std::env::var_os(k))).collect();
            Self { _lock: lock, saved }
        }
    }

    impl Drop for SandboxEnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// The keys `flux_system::sandbox::posture_env` may emit, collected out of a spawned `env`
    /// dump — so a test asserts the **whole** forwarded posture rather than the one entry it
    /// expected to move. Same shape (and, deliberately, its own copy of the key list rather than a
    /// read of the production constant) as C-276's helper in `flux-system`.
    fn forwarded_posture(stdout: &str) -> Vec<&str> {
        stdout
            .lines()
            .filter(|line| {
                line.starts_with("FLUX_SANDBOX=")
                    || line.starts_with("FLUX_SANDBOX_NET=")
                    || line.starts_with("FLUX_SANDBOX_WRITABLE=")
                    || line.starts_with("FLUX_BWRAP_BIN=")
                    || line.starts_with("FLUX_SANDBOX_EXEC_BIN=")
            })
            .collect()
    }

    /// Fast startup budget + no extra env, so a failing test fails in seconds rather than a minute.
    fn runtime_for(program: PathBuf) -> ProcessRuntime {
        ProcessRuntime::with_program(program).with_startup(Duration::from_secs(10), Vec::new())
    }

    /// A spec for one worker, with the fields most tests do not care about defaulted.
    fn spec_for(name: &str) -> WorkerSpec {
        WorkerSpec {
            name: name.into(),
            context_id: "ctx".into(),
            ..Default::default()
        }
    }

    /// C-243 Acceptance 1: the round trip. Start a worker, see it live at the endpoint the runtime
    /// chose, stop it, and see that the id is gone. Every step goes through the real ops over the
    /// real guarded spawn.
    #[tokio::test]
    async fn a_worker_round_trips_start_status_stop() {
        let root = test_root("roundtrip");
        let program = stand_in_worker();
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
        let root = test_root("killed");
        let program = stand_in_worker();
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
            .live
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
        let root = test_root("exits");
        // No `worktree`, so the worker's cwd is the workspace root — that is where its markers live.
        exits_with(&root, 17);
        let runtime = runtime_for(stand_in_worker());
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
            flux_core::readiness::announces_serving(&status.detail),
            "the worker's own output must survive the drain: {status:?}"
        );
        // Repeated polls must keep agreeing — `try_wait` reports a code once, so a runtime that did
        // not retain it would report `dead` with an unknown code on the second look.
        let again = runtime.status(&system, "C-243").await.unwrap();
        assert_eq!(again.exit_code, Some(17), "{again:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Acceptance 4, and the regression for the defect review finding B1 caught: the worker is scoped
    /// to the checkout it was given, **and that checkout lives outside the workspace root**.
    ///
    /// The arrangement here is the one `fleet.isolate` actually produces, which is the whole point.
    /// `System::allocate_worktree_dir` creates its parent under the base that system carries
    /// (`$FLUX_WORKTREE_DIR`, else `$HOME/.flux/worktrees`, unless pinned — C-391) —
    /// "outside any workspace root **on purpose**", per its own doc — and
    /// C-241 hands back `<that>/checkout`. An earlier revision of this op ran the path through
    /// `Workspace::resolve`, the write-path resolver, which admits only the primary and `@named`
    /// roots; it therefore rejected every real `fleet.isolate` output with "escapes the workspace
    /// root", and the original version of this test missed it by putting the checkout *inside* the
    /// test root — an arrangement the composition never produces.
    #[tokio::test]
    async fn a_worker_is_confined_to_a_checkout_outside_the_workspace_root() {
        let base = test_root("worktree");
        let root = base.join("repo");
        // A sibling of the workspace root, not a descendant — exactly `allocate_worktree_dir`'s
        // relationship to the caller's root.
        let checkout = base.join("flux-worktree-stand-in").join("checkout");
        std::fs::create_dir_all(&root).expect("the workspace root");
        std::fs::create_dir_all(&checkout).expect("the isolated checkout");
        assert!(
            !checkout.starts_with(&root),
            "this test is only meaningful if the checkout is outside the workspace root"
        );
        let runtime = runtime_for(stand_in_worker());

        // Through the **op**, not the runtime: the op is where the rejected resolve used to live.
        let ctx = ToolContext::new(Arc::new(test_system(&root)));
        let started = FleetStartTool::new(Arc::new(runtime))
            .execute(
                &ctx,
                json!({ "item": "C-243", "worktree": checkout.display().to_string() }),
            )
            .await
            .expect("fleet.start dispatches");
        assert!(
            !started.is_error,
            "a checkout outside the workspace root is exactly what fleet.isolate returns, so \
             fleet.start must accept it: {}",
            started.content
        );

        // The endpoint it handed back is real, and it tells the caller about the SSRF guard it is
        // about to meet — the refusal was correct but undiscoverable before.
        let started: Value = serde_json::from_str(&started.content).expect("start returns JSON");
        assert!(started["endpoint"]
            .as_str()
            .expect("an endpoint")
            .starts_with("http://127.0.0.1:"));
        assert!(
            started["dispatch_note"]
                .as_str()
                .expect("a dispatch note")
                .contains("--allow-private-net"),
            "the loopback endpoint must name the grant fleet.dispatch will demand: {started}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// The same confinement, asserted where the child's cwd is observable: the runtime handle stays
    /// in the test, so a status poll can read the `cwd=` line the stand-in worker prints.
    #[tokio::test]
    async fn a_workers_cwd_is_the_checkout_it_was_given() {
        let base = test_root("cwd");
        let root = base.join("repo");
        let checkout = base.join("flux-worktree-stand-in").join("checkout");
        std::fs::create_dir_all(&root).expect("the workspace root");
        std::fs::create_dir_all(&checkout).expect("the isolated checkout");
        let runtime = runtime_for(stand_in_worker());
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
        std::fs::remove_dir_all(&base).ok();
    }

    /// Dropping the write-path resolver is not a blank cheque. A **relative** `worktree` is refused
    /// outright — there is no cwd a caller could reason about for a worker's checkout — and a
    /// non-existent one is refused by `Workspace::with_root`, which is now the guard.
    #[tokio::test]
    async fn a_worktree_must_be_an_existing_absolute_directory() {
        let root = test_root("worktree-guard");
        let ctx = ToolContext::new(Arc::new(test_system(&root)));
        let op = FleetStartTool::new(Arc::new(runtime_for(stand_in_worker())));

        let relative = op
            .execute(&ctx, json!({ "item": "rel", "worktree": "../../etc" }))
            .await
            .expect_err("a relative worktree is refused");
        assert!(
            relative.to_string().contains("must be an absolute path"),
            "{relative}"
        );

        let missing = op
            .execute(
                &ctx,
                json!({ "item": "missing", "worktree": root.join("no-such-checkout").display().to_string() }),
            )
            .await
            .expect("fleet.start dispatches");
        assert!(missing.is_error, "{}", missing.content);
        assert!(
            missing.content.contains("cannot be scoped to"),
            "the refusal must name the checkout: {}",
            missing.content
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Two workers must not collide — the point of the whole story. Each gets its own loopback port,
    /// and a second start for the *same* item is refused rather than silently replacing the first
    /// (which would orphan a running process no id refers to).
    #[tokio::test]
    async fn concurrent_workers_get_distinct_endpoints_and_an_id_cannot_be_reused() {
        let root = test_root("distinct");
        let program = stand_in_worker();
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

    /// Review finding B2, the posture half: a coordinator whose sandbox is **active with the network
    /// closed** must refuse to start a worker rather than hand back an endpoint nothing can reach.
    ///
    /// Under the C-262 unattended profile (`flux app run --serve`, `flux run --yes`, …)
    /// `apply_sandbox_env` sets `FLUX_SANDBOX=require` and `FLUX_SANDBOX_NET=0`; `spawn_background`
    /// passes `Confinement::Sandboxed`, and `bubblewrap_argv` then adds `--unshare-net`. The worker
    /// binds `127.0.0.1:<port>` inside its **own** network namespace, so the announcement arrives and
    /// the endpoint is unreachable — works interactively (sandbox off, unwrapped), silently broken in
    /// exactly the automation this op exists for.
    ///
    /// This test asserts the right thing in **both** gate lanes rather than only where a backend
    /// exists, which is the gap that let the defect through in the first place: `System::new` is
    /// `Sandbox::disabled()`, so a test built on it can never see this path. With a usable backend the
    /// posture resolves active and the start must refuse; without one it resolves inactive, nothing is
    /// wrapped, and the start must succeed.
    #[tokio::test]
    async fn a_network_isolated_coordinator_refuses_to_start_an_unreachable_worker() {
        use flux_system::sandbox::{Sandbox, SandboxMode, SandboxSettings};

        let root = test_root("netns");
        let program = stand_in_worker();
        let runtime = runtime_for(program);

        // `On` rather than `Require` on purpose: `Require` without a backend refuses at
        // `ensure_available`, which would prove something else entirely.
        let closed = Sandbox::resolve(SandboxSettings {
            mode: SandboxMode::On,
            network: false,
            extra_writable: Vec::new(),
        });
        let active = closed.is_active();
        // Printed so `--nocapture` says which lane actually ran: a test that silently takes the
        // no-backend branch everywhere would prove nothing, which is how B2 survived in the first
        // place. CI runs both lanes (C-266's backend job, and the ordinary no-backend one).
        eprintln!(
            "C-243 netns lane: sandbox active = {active} ({})",
            closed.describe()
        );
        let system = System::new(Workspace::new(&root).expect("workspace")).with_sandbox(closed);

        let started = runtime.start(&system, spec_for("C-243")).await;
        if active {
            let refusal = started.expect_err(
                "a network-isolated coordinator must refuse rather than return an \
                             endpoint nothing can reach",
            );
            let refusal = refusal.to_string();
            assert!(
                refusal.contains("network namespace") && refusal.contains("FLUX_SANDBOX_NET=1"),
                "the refusal must explain the netns and name the remedy, got {refusal}"
            );
        } else {
            // No backend on this box: nothing is wrapped, so there is no namespace to be trapped in
            // and the start is legitimate. Asserting the *refusal* here would pass for the wrong
            // reason on CI and hide a regression on a developer box.
            started.expect("without an active backend nothing is wrapped, so the start is fine");
        }

        // The other half of the same posture question: with the network OPEN, an active sandbox is
        // fine — the child shares the host's loopback — so the start must succeed either way.
        let open = Sandbox::resolve(SandboxSettings {
            mode: SandboxMode::On,
            network: true,
            extra_writable: Vec::new(),
        });
        let system = System::new(Workspace::new(&root).expect("workspace")).with_sandbox(open);
        runtime
            .start(&system, spec_for("C-244"))
            .await
            .expect("an open-network sandbox leaves the worker on the host's loopback");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Review finding B2, the inheritance half: a worker must receive its parent's sandbox **posture**
    /// so it confines its own descendants — and must NOT receive `FLUX_SANDBOXED`, which only
    /// `build_command` may set and which means "you are already wrapped, do not nest".
    ///
    /// Since C-276 the posture travels with the guarded spawn itself (`sandbox::posture_env`,
    /// rendered from the coordinator's resolved `Sandbox`), so `worker_env` no longer forwards any
    /// of it — the assertion is now that it forwards **none** of it, marker included. C-282 removed
    /// the hand-rolled copy; C-289 made "only `build_command` may set it" true of every spawn rather
    /// than only a wrapped one. The two tests below are what hold that.
    #[test]
    fn a_worker_env_carries_no_sandbox_posture_and_never_the_confined_marker() {
        let runtime = ProcessRuntime::with_program("/nonexistent/worker");
        let env = runtime.worker_env();

        assert!(
            !env.iter().any(|(k, _)| k == "FLUX_SANDBOXED"),
            "FLUX_SANDBOXED is build_command's to decide, from what actually confines the worker; \
             forwarding it would describe a worker environment this seam cannot produce: {env:?}"
        );
        assert!(
            !env.iter().any(|(k, _)| k.starts_with("FLUX_SANDBOX")
                || k == "FLUX_BWRAP_BIN"
                || k == "FLUX_SANDBOX_EXEC_BIN"),
            "the posture is the guarded spawn's to forward, from the resolved Sandbox — a copy \
             here lands in the caller-override slot and replaces it: {env:?}"
        );
    }

    /// C-282: a worker must receive the posture its coordinator **resolved**, and `worker_env` must
    /// not be able to replace it with a different one.
    ///
    /// The guarded spawn already forwards the resolved posture (C-276's `sandbox::posture_env`),
    /// but it does so *before* caller overrides — so whatever `worker_env` puts in the env slot
    /// lands after it and wins. The forwarder removed here read the **ambient** environment, which
    /// `System::with_sandbox` exists precisely to diverge from: a coordinator pinned `On` under an
    /// ambient `FLUX_SANDBOX=off` handed its worker `flux-cli`'s kill switch, leaving it strictly
    /// less confined than forwarding nothing would have.
    ///
    /// Asserted as a **differential** against a spawn with no caller env at all: the expectation is
    /// the real `posture_env` output on this host rather than a second copy of its rules, so the
    /// test cannot agree with a wrong implementation of the decision it is checking. The whole
    /// forwarded posture is compared, not the one key expected to move.
    #[tokio::test]
    async fn a_worker_receives_the_coordinators_resolved_posture_not_the_ambient_one() {
        use flux_system::sandbox::{Sandbox, SandboxMode, SandboxSettings};

        let root = test_root("posture-floor");
        let runtime = runtime_for(stand_in_worker());

        let _env = SandboxEnvGuard::new(&["FLUX_SANDBOX", "FLUX_SANDBOX_NET"]);
        // `On` rather than `Require`: `Require` without a backend refuses at `ensure_available`,
        // which would prove something else entirely. Resolved before the ambient env is poisoned,
        // so this is a genuinely *pinned* posture — the `with_sandbox` shape.
        let pinned = Sandbox::resolve(SandboxSettings {
            mode: SandboxMode::On,
            network: true,
            extra_writable: Vec::new(),
        });
        let system = System::new(Workspace::new(&root).expect("workspace")).with_sandbox(pinned);
        // The ambient environment now contradicts the pin, the way an embedder's host env can.
        std::env::set_var("FLUX_SANDBOX", "off");
        std::env::set_var("FLUX_SANDBOX_NET", "0");

        let floor = system
            .run_with_env(&["env".to_string()], &[], Duration::from_secs(60))
            .await
            .expect("the baseline spawn");
        let floor = forwarded_posture(&floor.stdout).join("\n");
        assert!(
            floor.contains("FLUX_SANDBOX=on"),
            "the baseline must carry the pinned posture or this test proves nothing:\n{floor}"
        );

        let worker_env = runtime.worker_env();
        let worker = system
            .run_with_env(&["env".to_string()], &worker_env, Duration::from_secs(60))
            .await
            .expect("the worker-env spawn");
        assert_eq!(
            forwarded_posture(&worker.stdout).join("\n"),
            floor,
            "a worker's env must not move the posture the guarded spawn already forwarded: \
             pushing the ambient `off` into the caller-override slot hands the worker flux-cli's \
             kill switch, which beats its own `[sandbox] require` and C-262's unattended \
             fail-closed profile.\nworker env: {worker_env:?}\nfull child env:\n{}",
            worker.stdout
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The other half of C-282: `with_startup`'s `env` is a caller override too, and the guarded
    /// spawn applies those *after* the posture — so an embedder passing `FLUX_SANDBOX=off` through
    /// that seam would silently unconfine every worker it starts.
    ///
    /// There is no legitimate reason for a call site to push a coordinator's posture **downward**:
    /// a worker is a full `flux` that spawns processes of its own and must confine them exactly as
    /// its parent would. So the slot is closed rather than merely documented — the same treatment
    /// [`DEPTH_ENV`] already gets, for the same reason.
    ///
    /// `FLUX_SANDBOXED` is in the input on purpose. When this test was written it was the only place
    /// a caller could be stopped from forging the marker — `build_command` overwrote it after the
    /// overrides *only* for a genuinely wrapped spawn, so a coordinator with an inactive sandbox (the
    /// default posture) defended nothing. C-289 closed that at the spawn, in both directions; what
    /// this test still pins is that the startup seam never carries the key that far. The assertion
    /// below covers every `FLUX_SANDBOX*` spelling, and the filter (`sandbox::SANDBOX_ENV_KEYS`)
    /// covers exactly the same set, so neither over-promises.
    #[test]
    fn a_startup_env_may_not_push_a_sandbox_posture_or_forge_the_marker() {
        let runtime = ProcessRuntime::with_program("/nonexistent/worker").with_startup(
            Duration::from_secs(1),
            vec![
                ("FLUX_SANDBOX".to_string(), "off".to_string()),
                ("FLUX_SANDBOXED".to_string(), "1".to_string()),
                (
                    "FLUX_BWRAP_BIN".to_string(),
                    "/nonexistent/other-bwrap".to_string(),
                ),
                ("WORKER_LABEL".to_string(), "kept".to_string()),
            ],
        );
        let env = runtime.worker_env();

        assert!(
            !env.iter()
                .any(|(k, _)| k.starts_with("FLUX_SANDBOX") || k == "FLUX_BWRAP_BIN"),
            "a startup env must not be able to name the worker's sandbox posture, nor claim a \
             confinement that never happened: {env:?}"
        );
        assert!(
            env.contains(&("WORKER_LABEL".to_string(), "kept".to_string())),
            "only the posture keys are dropped — carrying the rest is the seam's whole point: \
             {env:?}"
        );
    }

    /// Review finding B3: a worker is spawned with `--yes`, and its own catalog contains
    /// `fleet.start` with an `AllowApprover` behind it — so the coordinator's first start is gated by
    /// `process.exec` approval and every start below it would not be. The depth marker bounds it, the
    /// same way `LocalSpawner` bounds sub-agent recursion with `max_depth`.
    #[tokio::test]
    async fn a_worker_may_not_start_workers_of_its_own() {
        let root = test_root("depth");
        let program = stand_in_worker();
        let system = test_system(&root);

        // What a worker's own runtime looks like: depth 1 under the default budget of 1.
        let nested = runtime_for(program.clone()).with_bounds(1, DEFAULT_MAX_WORKERS);
        let nested = ProcessRuntime { depth: 1, ..nested };
        let refusal = nested
            .start(&system, spec_for("C-244"))
            .await
            .expect_err("generation 1 of 1 must not open a wave of its own");
        let refusal = refusal.to_string();
        assert!(
            refusal.contains("generation 1 of a maximum 1") && refusal.contains("--yes"),
            "the refusal must name the budget and why it exists, got {refusal}"
        );

        // The marker a worker is started with is what makes that true, and it is one more than the
        // parent's — a coordinator at depth 0 grants 1, never 0.
        let coordinator = runtime_for(program);
        assert_eq!(coordinator.depth(), 0, "a test process is not a worker");
        let env = coordinator.worker_env();
        assert_eq!(
            env.iter()
                .filter(|(k, _)| k == DEPTH_ENV)
                .map(|(_, v)| v.as_str())
                .next_back(),
            Some("1"),
            "a worker must be told the generation it occupies: {env:?}"
        );
        // And the budget cannot be widened from inside: the depth marker is appended last, so a host
        // seam passing its own value loses to the runtime's.
        let spoofed = ProcessRuntime::with_program("/nonexistent/worker")
            .with_startup(Duration::from_secs(1), vec![(DEPTH_ENV.into(), "0".into())]);
        assert_eq!(
            spoofed
                .worker_env()
                .iter()
                .filter(|(k, _)| k == DEPTH_ENV)
                .map(|(_, v)| v.as_str())
                .next_back(),
            Some("1"),
            "the runtime's depth must win over any value handed to it"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The concurrent-worker cap: depth stops a chain, this stops a fan.
    #[tokio::test]
    async fn a_coordinator_cannot_hold_more_workers_than_its_cap() {
        let root = test_root("fan");
        let program = stand_in_worker();
        let runtime = runtime_for(program).with_bounds(DEFAULT_MAX_FLEET_DEPTH, 2);
        let system = test_system(&root);

        runtime.start(&system, spec_for("a")).await.expect("first");
        runtime.start(&system, spec_for("b")).await.expect("second");
        let refusal = runtime
            .start(&system, spec_for("c"))
            .await
            .expect_err("the third exceeds the cap");
        assert!(
            refusal.to_string().contains("2 of a maximum 2"),
            "{refusal}"
        );
        // Stopping one frees the budget, so the cap is a live count and not a high-water mark.
        runtime.stop(&system, "a").await.expect("stop frees a slot");
        runtime
            .start(&system, spec_for("c"))
            .await
            .expect("the freed slot is usable");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Review finding, the lock: a start that stalls in its readiness wait must not block the other
    /// verbs. Before this, `start` held the workers mutex across the whole wait — up to 60s — so a
    /// coordinator could neither sweep nor cancel its wave while one worker was slow to bind.
    ///
    /// One runtime, therefore one lock, therefore a real test: the stand-in worker announces only when
    /// its checkout lacks a `no-announce` marker, so the same program gives a live worker and a
    /// stalling one.
    #[tokio::test]
    async fn a_stalling_start_does_not_block_the_other_verbs() {
        let base = test_root("lock");
        let root = base.join("repo");
        let announcing = base.join("announcing");
        let silent = base.join("silent");
        std::fs::create_dir_all(&root).expect("workspace root");
        std::fs::create_dir_all(&announcing).expect("announcing checkout");
        std::fs::create_dir_all(&silent).expect("silent checkout");
        // One program, two behaviours chosen by the checkout — which is what lets a single runtime
        // (and therefore a single lock) hold both a stalling start and a live worker.
        never_announces(&silent);

        let program = stand_in_worker();
        let runtime = Arc::new(
            ProcessRuntime::with_program(program).with_startup(Duration::from_secs(30), Vec::new()),
        );
        let system = Arc::new(test_system(&root));

        // A live worker first, so there is something to sweep.
        runtime
            .start(
                &system,
                WorkerSpec {
                    name: "live".into(),
                    worktree: Some(announcing.clone()),
                    context_id: "ctx".into(),
                    model: None,
                },
            )
            .await
            .expect("the marked checkout announces");

        // Now a start that will sit in its readiness wait for the full 30s budget.
        let stalling = {
            let (runtime, system) = (runtime.clone(), system.clone());
            tokio::spawn(async move {
                runtime
                    .start(
                        &system,
                        WorkerSpec {
                            name: "stalling".into(),
                            worktree: Some(silent.clone()),
                            context_id: "ctx".into(),
                            model: None,
                        },
                    )
                    .await
            })
        };
        // Let it get past the reservation and into the wait.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // The whole point: these must answer now, not in 30s.
        let swept = tokio::time::timeout(Duration::from_secs(2), runtime.status(&system, "live"))
            .await
            .expect("a status poll must not queue behind a stalling start")
            .expect("the live worker resolves");
        assert_eq!(swept.state, WorkerState::Live);

        // The stalling worker is visible as `starting` — not missing, and not dispatchable.
        let starting =
            tokio::time::timeout(Duration::from_secs(2), runtime.status(&system, "stalling"))
                .await
                .expect("a status poll for the starting worker must not block either")
                .expect("a reserved id resolves");
        assert_eq!(starting.state, WorkerState::Starting);
        assert!(starting.endpoint.is_none(), "{starting:?}");

        tokio::time::timeout(Duration::from_secs(2), runtime.stop(&system, "live"))
            .await
            .expect("a stop must not queue behind a stalling start")
            .expect("the live worker stops");

        stalling.abort();
        std::fs::remove_dir_all(&base).ok();
    }

    /// `with_base_port` is public API on an exported type, and it did nothing: `next_candidate`
    /// hardcoded the default base, so a configured range was silently ignored.
    #[test]
    fn a_configured_base_port_is_the_one_that_gets_offered() {
        let runtime = ProcessRuntime::with_program("/nonexistent/worker").with_base_port(9100);
        let offered: Vec<u16> = (0..4).map(|_| runtime.next_candidate()).collect();
        assert_eq!(offered, vec![9100, 9101, 9102, 9103], "{offered:?}");

        let default = ProcessRuntime::with_program("/nonexistent/worker");
        assert_eq!(default.next_candidate(), DEFAULT_WORKER_BASE_PORT);
        // And the range wraps inside its span rather than walking off into arbitrary ports.
        let far = ProcessRuntime::with_program("/nonexistent/worker").with_base_port(9100);
        for _ in 0..PORT_SPAN {
            far.next_candidate();
        }
        assert_eq!(far.next_candidate(), 9100, "the range must wrap in-span");
    }

    /// `ExternalRuntime` makes an operator-run worker addressable through the same port — and is
    /// honest about what it does not own: it refuses to stop a process it never started.
    #[tokio::test]
    async fn an_external_worker_is_addressable_but_not_stoppable() {
        let root = test_root("external");
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
