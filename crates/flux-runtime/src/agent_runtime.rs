//! [`AgentRuntime`] — the port that answers **where a fleet worker runs** (C-243).
//!
//! A2A gave the fleet a way to *talk* to a worker (`fleet.dispatch` / `fleet.status` /
//! `fleet.cancel`, `crate::DispatchLedger`), but nothing made a worker *exist*: `flux` never spawned
//! `flux`, and `FlowEngine`'s turn gate serves one concurrent turn per worker, so a coordinator's
//! "wave" was always a wave of one. This port is the seam that fixes it, and it is deliberately the
//! narrowest thing that can: four verbs over an opaque worker id.
//!
//! ## Why the port is here and this shape
//!
//! `ProcessRuntime` (a child `flux` on this machine) is the first implementation; `DockerRuntime`
//! (A-124) and `KubernetesRuntime` (A-125) are meant to land against this same trait without
//! touching the ops above it. So nothing runtime-specific may appear in the signatures — no image
//! name, no namespace, no port, no container id. A worker is named by the id its runtime minted and
//! reached at the endpoint its runtime reports; everything else is that runtime's private business.
//!
//! Every method takes the guarded [`System`], because **every** implementation of this port creates
//! OS processes (`flux app run --serve`, `docker run`, `kubectl`) and all of them must do it through
//! flux's single `build_command` choke point. Passing the guarded system in rather than capturing one
//! at construction also means a worker inherits the workspace that is active *at start time* — which
//! is what lets a coordinator scope a worker to the isolated checkout it just made for one item
//! rather than to the coordinator's own root.

use std::path::PathBuf;

use async_trait::async_trait;
use flux_core::Result;
use flux_system::System;

/// What one worker is asked to be — the runtime-independent half of a start request.
#[derive(Debug, Clone, Default)]
pub struct WorkerSpec {
    /// Logical name of the work this worker serves — in the fleet coordinator, the board item id.
    /// Also the worker's id, so a coordinator that knows the item can always find the worker again
    /// without a second registry to fall out of sync (the same argument as `DispatchLedger`).
    pub name: String,
    /// Checkout the worker is confined to — what `fleet.isolate` (C-241) hands back. `None` runs the
    /// worker in the coordinator's own workspace, which is only ever right for a worker that does
    /// not write.
    pub worktree: Option<PathBuf>,
    /// A2A `contextId` the worker's session is bound to. A later `fleet.dispatch` quoting the same
    /// value resumes the same session on that worker (`flux_server`'s `find_or_mint_session`), which
    /// is what makes a rework round a continuation rather than a fresh, contextless run.
    pub context_id: String,
    /// Model spec for the worker. `None` leaves the worker to resolve its own configured default.
    pub model: Option<String>,
}

/// A worker that has been started and is addressable.
#[derive(Debug, Clone)]
pub struct Worker {
    /// Opaque id this runtime knows the worker by — the handle for `stop`/`status`/`endpoint`.
    pub id: String,
    /// A2A endpoint the worker answers on, ready to be passed to `fleet.dispatch` as `worker`.
    pub endpoint: String,
    /// The `contextId` the worker's session is bound to (echoed from the spec).
    pub context_id: String,
}

/// Liveness of a worker, as its runtime observes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    /// Started, but has not yet reported that it is serving. Not dispatchable.
    Starting,
    /// Serving, and safe to dispatch to.
    Live,
    /// Gone. A dispatch to it will fail; the coordinator must restart or reassign the item.
    Dead,
}

impl WorkerState {
    /// Stable wire spelling, so an op's JSON and a Program's `match` agree.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Live => "live",
            Self::Dead => "dead",
        }
    }
}

/// One worker's observed state.
///
/// `Dead` is a first-class answer rather than an error: a coordinator sweeping its wave needs to
/// learn that a worker died, and an `Err` would be indistinguishable from "the poll itself failed".
#[derive(Debug, Clone)]
pub struct WorkerStatus {
    /// The worker id this status is about.
    pub id: String,
    /// Liveness.
    pub state: WorkerState,
    /// Where the worker answers, while it still does.
    pub endpoint: Option<String>,
    /// The `contextId` this worker's session is bound to — reported so a coordinator that lost its
    /// own bookkeeping can still resume the right conversation from the worker id alone.
    pub context_id: Option<String>,
    /// Exit code, once the worker has exited and the runtime observed a code (a signalled worker
    /// exits with none).
    pub exit_code: Option<i32>,
    /// Human-readable reason/diagnostics — for `ProcessRuntime`, the tail of the worker's own
    /// stderr, which is the only place a startup failure explains itself.
    pub detail: String,
}

/// Where a fleet worker runs. See the module docs for why the signatures look like this.
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// Stable name of this runtime, reported by the ops so an operator can see which backend
    /// answered (`process`, later `docker` / `kubernetes`).
    fn kind(&self) -> &'static str;

    /// Start a worker for `spec` and return it only once it is addressable — a `start` that returned
    /// a not-yet-serving endpoint would hand the caller a worker whose first dispatch fails for a
    /// reason indistinguishable from a dead one.
    async fn start(&self, system: &System, spec: WorkerSpec) -> Result<Worker>;

    /// Stop the worker `id` names. Idempotent from the caller's side: stopping an already-dead
    /// worker succeeds, because the caller's intent ("this worker must not be running") holds.
    /// An **unknown** id is an error — that is a coordinator bug, not a satisfied intent.
    async fn stop(&self, system: &System, id: &str) -> Result<()>;

    /// Observe the worker `id` names. Errors only if the id is unknown.
    async fn status(&self, system: &System, id: &str) -> Result<WorkerStatus>;

    /// The endpoint the worker `id` names answers on. Errors if the id is unknown.
    async fn endpoint(&self, system: &System, id: &str) -> Result<String>;
}
