//! Per-worker projection of the live sub-agent activity stream — the one surface-side fold.
//!
//! A-79 ships a correlated sub-agent activity stream ([`flux_runtime::SpawnActivitySink`], design
//! `docs/designs/live-sub-agent-activity.md`) and *is* installed in production, but every surface
//! dropped it: the TUI never decoded the `subagent.activity` observation and the CLI's sink ignored
//! it, so a long delegated run — a fleet of workers included — read as silence (C-246). This module
//! owns the single fold from that event stream to per-worker rows, so each surface renders the same
//! state instead of deriving its own. There is exactly one activity path and this is its projector.
//!
//! **Default-deny is structural here, not a policy.** The A-79 value carries tool input and
//! observation data as an *internal, host-side* contract a customer surface must default-deny.
//! Nothing in this module reads either field. What crosses into a [`WorkerRow`] is the structural
//! identity the stream exists to correlate — spawn id, role, child session, depth — plus the
//! operation *name* and a [`WorkerStatus`] from a closed set. That is strictly stronger than
//! redacting on the way out: a worker's secrets cannot leak through a field that is never read.
//! The emitter-side [`flux_secret::Redactor`] pass
//! (`flux_orchestrate`'s `redact_spawn_json`) stays the seam for the internal contract; this
//! module adds no second one.
//!
//! **Why a fold and not a log.** The operational question a fleet surface has to answer is "is that
//! worker working or hung?", which no event line can answer on its own — only the age of a worker's
//! most recent event can. [`WorkerRow::idle`] and [`WorkerRow::stalled`] are that answer, and `now`
//! is a caller-supplied parameter throughout so every call site and every test of one is
//! deterministic (the [`flux_core::humanize::fmt_age`] convention).

use std::time::{Duration, Instant};

use flux_runtime::{SpawnActivity, SpawnActivityEvent};

/// How long a worker may go without reporting before a surface calls it stalled. A worker between
/// model calls is routinely quiet for a few seconds; a minute of nothing is the operator's signal.
pub const DEFAULT_STALL_AFTER: Duration = Duration::from_secs(60);

/// How long a finished worker's row is kept so a surface can show how the wave ended.
const FINISHED_RETENTION: Duration = Duration::from_secs(30);

/// Hard bound on tracked workers. The stream is trusted but unbounded — nested delegation relays
/// grandchildren through the same reporter — so the projection is capped rather than grown.
const MAX_TRACKED: usize = 64;

/// Bound on a projected role name. Roles are file-defined (`.flux/agents/<role>.md`), not model
/// output, but a surface must not let one wrap or dominate a status line.
const MAX_ROLE_CHARS: usize = 32;

/// Bound on a projected operation name. Registry- or plugin-defined, same reasoning as the role.
const MAX_OP_CHARS: usize = 48;

/// Bound on concurrently pending calls remembered per worker, so a child that opens calls without
/// ever reporting a result cannot grow the projection.
const MAX_PENDING: usize = 16;

/// What a worker is doing, from the closed set a surface may name. Deliberately not derived from
/// tool input or observation data — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStatus {
    /// Seen, but nothing reported yet.
    Starting,
    /// Inside a planning bracket.
    Planning,
    /// Running the named operation.
    Running { op: String },
    /// Reported an outcome and has not started anything else.
    Idle,
    /// C-601: the cancel request reached this worker and it is winding down — it may still be
    /// inside an uninterruptible provider call. Non-terminal, and deliberately distinct from both
    /// `Running` (the request has not been acknowledged) and `Finished` (it is over), because
    /// "cancelling" is precisely the state an operator could not see before.
    Cancelling,
    /// Terminal, with the spawner-boundary outcome bit. No error text crosses A-79's contract.
    Finished { is_error: bool },
}

impl WorkerStatus {
    /// A fixed label from the closed set. Surfaces render this rather than inventing their own, so
    /// the CLI line and the TUI pane cannot disagree about what a worker is doing.
    pub fn label(&self) -> &'static str {
        match self {
            WorkerStatus::Starting => "starting",
            WorkerStatus::Planning => "planning",
            WorkerStatus::Running { .. } => "running",
            WorkerStatus::Idle => "idle",
            WorkerStatus::Cancelling => "cancelling",
            WorkerStatus::Finished { is_error: false } => "done",
            WorkerStatus::Finished { is_error: true } => "failed",
        }
    }

    /// The operation a `Running` worker is in, if any.
    pub fn op(&self) -> Option<&str> {
        match self {
            WorkerStatus::Running { op } => Some(op),
            _ => None,
        }
    }

    fn is_finished(&self) -> bool {
        matches!(self, WorkerStatus::Finished { .. })
    }
}

/// One worker as a surface should show it, resolved against a caller-supplied `now`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRow {
    /// A-79's process-local spawn id — the correlation key. Two concurrent storeless children of
    /// the same role share a `child_session_id` (`s_1` in every fresh store) but never a spawn id,
    /// so pairing rows on anything else misattributes their events.
    pub spawn_id: u64,
    /// The worker's file-defined role (`.flux/agents/<role>.md`) — what an operator calls it, and
    /// the only human-meaningful name a surface has. Sanitized and length-bounded on the way in,
    /// because a role name reaching surface chrome is still untrusted text.
    pub role: String,
    /// The child's own session id, carried so a surface can point an operator at the run to open
    /// for detail. Not an identity: a fresh storeless store hands every child `s_1`, which is why
    /// `spawn_id` and not this is the correlation key.
    pub child_session_id: String,
    /// Delegation nesting, `1` for a top-level agent's direct child. Nested delegation relays
    /// grandchildren through the same reporter, so this is what tells a fleet's own workers apart
    /// from the workers those workers spawned.
    pub depth: usize,
    /// What the worker is doing, from the closed label set — the answer to "working or hung?"
    /// together with [`WorkerRow::idle`].
    pub status: WorkerStatus,
    /// Operations this worker completed (results reported).
    pub ops: usize,
    /// How many of those reported an error.
    pub errors: usize,
    /// Since this worker's first reported event.
    pub elapsed: Duration,
    /// Since this worker's most recent event — the hung-versus-working signal.
    pub idle: Duration,
    /// `idle` exceeded the projection's threshold and the worker has not finished.
    pub stalled: bool,
}

#[derive(Debug)]
struct Worker {
    spawn_id: u64,
    role: String,
    child_session_id: String,
    depth: usize,
    status: WorkerStatus,
    ops: usize,
    errors: usize,
    first_seen: Instant,
    last_activity: Instant,
    /// `(call_id, op name)` for calls this worker opened and has not resolved.
    pending: Vec<(u64, String)>,
}

/// The surface-side fold over [`SpawnActivity`]. Insertion-ordered so a rendered fleet does not
/// reshuffle between frames.
#[derive(Debug)]
pub struct FleetProjection {
    workers: Vec<Worker>,
    stall_after: Duration,
    /// Workers refused because the projection was at [`MAX_TRACKED`] — reported rather than hidden.
    dropped: usize,
}

impl Default for FleetProjection {
    fn default() -> Self {
        Self::new()
    }
}

impl FleetProjection {
    /// An empty projection at the [`DEFAULT_STALL_AFTER`] threshold. A surface builds one per run
    /// and folds every [`SpawnActivity`] into it; use [`FleetProjection::with_stall_after`] to be
    /// more or less patient than the default.
    pub fn new() -> Self {
        FleetProjection {
            workers: Vec::new(),
            stall_after: DEFAULT_STALL_AFTER,
            dropped: 0,
        }
    }

    /// Override the stall threshold (tests, and a surface that wants to be more or less patient).
    pub fn with_stall_after(mut self, stall_after: Duration) -> Self {
        self.stall_after = stall_after;
        self
    }

    /// Fold one event. Returns whether a surface should redraw — `false` only for an event the
    /// projection refused (over [`MAX_TRACKED`]), so a caller can render on every accepted event
    /// without tracking state itself.
    pub fn apply(&mut self, activity: &SpawnActivity, now: Instant) -> bool {
        self.prune(now);
        let index = match self
            .workers
            .iter()
            .position(|w| w.spawn_id == activity.spawn_id)
        {
            Some(index) => index,
            None => {
                if self.workers.len() >= MAX_TRACKED {
                    self.dropped += 1;
                    return false;
                }
                self.workers.push(Worker {
                    spawn_id: activity.spawn_id,
                    role: label(&activity.role, MAX_ROLE_CHARS),
                    child_session_id: label(&activity.child_session_id, MAX_ROLE_CHARS),
                    depth: activity.depth,
                    status: WorkerStatus::Starting,
                    ops: 0,
                    errors: 0,
                    first_seen: now,
                    last_activity: now,
                    pending: Vec::new(),
                });
                self.workers.len() - 1
            }
        };
        let worker = &mut self.workers[index];
        worker.last_activity = now;
        // C-601: cancellation latches. A child that is winding down keeps reporting — a tool result
        // lands, a pending call closes — and every one of those would otherwise repaint the row as
        // ordinary work, which is exactly the "cancel was ignored" impression the state exists to
        // remove. Only the terminal leaves this state.
        let cancelling = matches!(worker.status, WorkerStatus::Cancelling);
        match &activity.event {
            SpawnActivityEvent::Planning { active } => {
                if !cancelling {
                    worker.status = if *active {
                        WorkerStatus::Planning
                    } else {
                        WorkerStatus::Idle
                    };
                }
            }
            SpawnActivityEvent::ToolCall { call_id, name, .. } => {
                // `input` is deliberately not read: it is the internal half of A-79's contract.
                let op = label(name, MAX_OP_CHARS);
                if worker.pending.len() < MAX_PENDING {
                    worker.pending.push((*call_id, op.clone()));
                }
                if !cancelling {
                    worker.status = WorkerStatus::Running { op };
                }
            }
            SpawnActivityEvent::ToolTiming { .. } => {
                // Timing only refreshes liveness; the op is already named by its `ToolCall`.
            }
            SpawnActivityEvent::ToolResult {
                call_id, is_error, ..
            } => {
                worker.pending.retain(|(id, _)| id != call_id);
                worker.ops += 1;
                if *is_error {
                    worker.errors += 1;
                }
                // An outstanding call means the worker is still inside another op; name that one
                // rather than claiming the worker went idle.
                if !cancelling {
                    worker.status = match worker.pending.last() {
                        Some((_, op)) => WorkerStatus::Running { op: op.clone() },
                        None => WorkerStatus::Idle,
                    };
                }
            }
            SpawnActivityEvent::Observation { .. } => {
                // `observation.data` is the other internal half of the contract; only the fact
                // that the worker reported *something* crosses, as refreshed liveness.
            }
            SpawnActivityEvent::Cancelling => {
                worker.status = WorkerStatus::Cancelling;
            }
            SpawnActivityEvent::Finished { is_error, .. } => {
                worker.pending.clear();
                worker.status = WorkerStatus::Finished {
                    is_error: *is_error,
                };
            }
        }
        true
    }

    /// Every tracked worker, resolved against `now`.
    pub fn rows(&self, now: Instant) -> Vec<WorkerRow> {
        self.workers
            .iter()
            .map(|worker| WorkerRow {
                spawn_id: worker.spawn_id,
                role: worker.role.clone(),
                child_session_id: worker.child_session_id.clone(),
                depth: worker.depth,
                status: worker.status.clone(),
                ops: worker.ops,
                errors: worker.errors,
                elapsed: now.saturating_duration_since(worker.first_seen),
                idle: now.saturating_duration_since(worker.last_activity),
                stalled: !worker.status.is_finished()
                    && now.saturating_duration_since(worker.last_activity) >= self.stall_after,
            })
            .collect()
    }

    /// How many tracked workers have not finished.
    pub fn live(&self) -> usize {
        self.workers
            .iter()
            .filter(|w| !w.status.is_finished())
            .count()
    }

    /// Whether anything is tracked at all — a surface shows no fleet chrome when this is true.
    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    /// Workers refused at the [`MAX_TRACKED`] bound.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Drop finished rows once their retention has elapsed. A live worker is never retired, however
    /// long it has been quiet — a stalled worker going missing from the surface is the failure this
    /// story exists to fix.
    ///
    /// **Public because retirement is time-driven, not event-driven** (C-224). [`FleetProjection::apply`]
    /// prunes on its way in, but a fleet whose last worker has finished receives no further event,
    /// so a surface that decides whether to show a fleet region at all has to be able to advance the
    /// retention clock itself. [`FleetProjection::rows`] deliberately stays `&self` — a renderer
    /// must not mutate — which is why this is a separate call rather than folded into it.
    pub fn prune(&mut self, now: Instant) {
        self.workers.retain(|worker| {
            !worker.status.is_finished()
                || now.saturating_duration_since(worker.last_activity) < FINISHED_RETENTION
        });
    }
}

/// Neutralize and bound a projected identifier. Control characters become spaces before any surface
/// sees them, so a role or operation name can never forge surface chrome with an escape sequence —
/// the same defence [`crate::projection`]'s staged-intent sanitizer applies to model-authored text.
/// Truncation counts **chars**, never bytes (the workspace's standing rule for untrusted text).
fn label(raw: &str, max_chars: usize) -> String {
    let sanitized: String = raw
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let collapsed = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > max_chars {
        collapsed.chars().take(max_chars).collect()
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn activity(spawn_id: u64, role: &str, event: SpawnActivityEvent) -> SpawnActivity {
        SpawnActivity {
            spawn_id,
            role: role.into(),
            // Deliberately the SAME session id for every child: a fresh storeless event store
            // hands every child `s_1`, which is exactly why `spawn_id` is the correlation key.
            child_session_id: "s_1".into(),
            parent_session: Some("s_parent".into()),
            depth: 1,
            event,
        }
    }

    fn call(call_id: u64, name: &str, input: serde_json::Value) -> SpawnActivityEvent {
        SpawnActivityEvent::ToolCall {
            call_id,
            name: name.into(),
            input,
        }
    }

    #[test]
    fn events_pair_to_the_right_worker_by_spawn_id() {
        // Two concurrent workers of the SAME role running the SAME op — the correlation A-79
        // exists to provide. Session id cannot separate them; spawn id must.
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        fleet.apply(&activity(1, "implementor", call(1, "read", json!({}))), t0);
        fleet.apply(&activity(2, "implementor", call(1, "grep", json!({}))), t0);
        fleet.apply(
            &activity(
                1,
                "implementor",
                SpawnActivityEvent::ToolResult {
                    call_id: 1,
                    name: "read".into(),
                    is_error: false,
                },
            ),
            t0,
        );

        let rows = fleet.rows(t0);
        assert_eq!(rows.len(), 2, "two workers tracked: {rows:?}");
        let first = rows.iter().find(|r| r.spawn_id == 1).expect("worker 1");
        let second = rows.iter().find(|r| r.spawn_id == 2).expect("worker 2");
        assert_eq!(
            first.status,
            WorkerStatus::Idle,
            "worker 1 resolved its read"
        );
        assert_eq!(first.ops, 1);
        assert_eq!(
            second.status,
            WorkerStatus::Running { op: "grep".into() },
            "worker 2's grep must not be closed by worker 1's result"
        );
        assert_eq!(second.ops, 0);
    }

    #[test]
    fn a_hung_worker_is_distinguishable_from_a_working_one() {
        // The whole operational point: same role, same op, one moved 200ms ago and one has not
        // moved in two minutes.
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        fleet.apply(&activity(1, "worker", call(1, "bash", json!({}))), t0);
        fleet.apply(&activity(2, "worker", call(1, "bash", json!({}))), t0);
        let later = t0 + Duration::from_secs(120);
        fleet.apply(&activity(2, "worker", call(2, "read", json!({}))), later);

        let rows = fleet.rows(later + Duration::from_millis(200));
        let hung = rows.iter().find(|r| r.spawn_id == 1).expect("worker 1");
        let working = rows.iter().find(|r| r.spawn_id == 2).expect("worker 2");
        assert!(hung.stalled, "120s of silence is stalled: {hung:?}");
        assert!(hung.idle >= Duration::from_secs(120));
        assert!(!working.stalled, "200ms of silence is not: {working:?}");
        assert_eq!(fleet.live(), 2, "both are still live — neither finished");
    }

    /// C-601 (failing first): when the operator cancels, a worker that is still winding down must
    /// be visible as *cancelling* — a state distinct from `Running` (which reads as "the cancel was
    /// ignored") and from the terminal it has not reached yet. Cancellation latches, so a late
    /// result from the winding-down child cannot repaint the row as ordinary work.
    #[test]
    fn a_cancelling_worker_is_distinct_from_running_and_from_its_terminal() {
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        fleet.apply(&activity(1, "researcher", call(1, "read", json!({}))), t0);
        assert_eq!(
            fleet.rows(t0)[0].status,
            WorkerStatus::Running { op: "read".into() }
        );

        // Ctrl-C. The child is mid-provider-call and has not stopped yet.
        let cancelled_at = t0 + Duration::from_secs(1);
        fleet.apply(
            &activity(1, "researcher", SpawnActivityEvent::Cancelling),
            cancelled_at,
        );
        let row = fleet.rows(cancelled_at).remove(0);
        assert_eq!(row.status, WorkerStatus::Cancelling);
        assert_eq!(row.status.label(), "cancelling");
        assert_eq!(row.status.op(), None, "a cancelling worker names no op");
        assert_eq!(fleet.live(), 1, "cancelling is not terminal");

        // A result the winding-down child still reports must not read as work resuming.
        fleet.apply(
            &activity(
                1,
                "researcher",
                SpawnActivityEvent::ToolResult {
                    call_id: 1,
                    name: "read".into(),
                    is_error: false,
                },
            ),
            cancelled_at,
        );
        assert_eq!(
            fleet.rows(cancelled_at)[0].status,
            WorkerStatus::Cancelling,
            "cancellation latches until the terminal"
        );

        // And the terminal still lands, distinct from both.
        fleet.apply(
            &activity(
                1,
                "researcher",
                SpawnActivityEvent::Finished {
                    usage: None,
                    is_error: true,
                },
            ),
            cancelled_at,
        );
        assert_eq!(
            fleet.rows(cancelled_at)[0].status,
            WorkerStatus::Finished { is_error: true }
        );
        assert_eq!(fleet.live(), 0);
    }

    #[test]
    fn a_finished_worker_is_never_stalled() {
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        fleet.apply(&activity(1, "worker", call(1, "read", json!({}))), t0);
        fleet.apply(
            &activity(
                1,
                "worker",
                SpawnActivityEvent::Finished {
                    usage: None,
                    is_error: true,
                },
            ),
            t0,
        );
        let rows = fleet.rows(t0 + Duration::from_secs(600));
        assert_eq!(rows[0].status, WorkerStatus::Finished { is_error: true });
        assert_eq!(rows[0].status.label(), "failed");
        assert!(!rows[0].stalled, "a finished worker is not hung, just done");
        assert_eq!(fleet.live(), 0);
    }

    #[test]
    fn a_live_worker_is_never_retired_however_long_it_is_quiet() {
        // Retiring a silent worker would delete the exact signal C-246 exists to show.
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        fleet.apply(&activity(1, "worker", call(1, "bash", json!({}))), t0);
        fleet.apply(
            &activity(2, "worker", SpawnActivityEvent::Planning { active: true }),
            t0 + Duration::from_secs(3_600),
        );
        let rows = fleet.rows(t0 + Duration::from_secs(3_600));
        assert!(
            rows.iter().any(|r| r.spawn_id == 1 && r.stalled),
            "the hour-silent worker is still on the surface: {rows:?}"
        );
    }

    #[test]
    fn a_finished_worker_retires_after_its_retention() {
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        fleet.apply(
            &activity(
                1,
                "worker",
                SpawnActivityEvent::Finished {
                    usage: None,
                    is_error: false,
                },
            ),
            t0,
        );
        assert_eq!(fleet.rows(t0).len(), 1);
        fleet.apply(
            &activity(2, "worker", SpawnActivityEvent::Planning { active: true }),
            t0 + FINISHED_RETENTION + Duration::from_secs(1),
        );
        let rows = fleet.rows(t0 + FINISHED_RETENTION + Duration::from_secs(1));
        assert_eq!(rows.len(), 1, "the finished row retired: {rows:?}");
        assert_eq!(rows[0].spawn_id, 2);
    }

    #[test]
    fn an_overlapping_call_keeps_naming_the_op_still_running() {
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        fleet.apply(&activity(1, "worker", call(1, "bash", json!({}))), t0);
        fleet.apply(&activity(1, "worker", call(2, "read", json!({}))), t0);
        fleet.apply(
            &activity(
                1,
                "worker",
                SpawnActivityEvent::ToolResult {
                    call_id: 2,
                    name: "read".into(),
                    is_error: false,
                },
            ),
            t0,
        );
        assert_eq!(
            fleet.rows(t0)[0].status,
            WorkerStatus::Running { op: "bash".into() },
            "the still-open bash keeps the worker `running`, not `idle`"
        );
    }

    #[test]
    fn the_projection_is_bounded() {
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        for spawn_id in 0..(MAX_TRACKED as u64 + 8) {
            fleet.apply(
                &activity(
                    spawn_id,
                    "worker",
                    SpawnActivityEvent::Planning { active: true },
                ),
                t0,
            );
        }
        assert_eq!(fleet.rows(t0).len(), MAX_TRACKED);
        assert_eq!(fleet.dropped(), 8, "refusals are counted, not hidden");
    }

    /// The corpus test the story asks for. Every one of these is fed to the projection **already
    /// past** the emitter's `Redactor` pass — i.e. as if that seam had failed open — inside the two
    /// fields A-79 documents as the internal half of its contract. The projection must not surface
    /// any of them, because it never reads those fields at all.
    #[test]
    fn no_worker_secret_can_reach_a_surface_through_the_projection() {
        // C-325: each credential is joined from two fragments at compile time, split inside the
        // vendor prefix. The corpus keeps the realistic shapes it needs; the file on disk carries
        // nothing a forge's secret scanning would block the push on.
        const CORPUS: &[&str] = &[
            concat!("sk-ant-", "api03-REALLOOKINGKEYMATERIAL"),
            concat!("ghp", "_0123456789abcdefghijklmnopqrstuvwxyz"),
            concat!("AKI", "AIOSFODNN7EXAMPLE"),
            concat!(
                "xoxb",
                "-000000000000-000000000000-ZZZZZZZZZZZZZZZZZZZZZZZZ"
            ),
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ3b3JrZXIifQ.s1gn4tur3",
            "postgres://fleet:hunter2@db.internal:5432/prod",
            "hunter2",
        ];
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        for (index, secret) in CORPUS.iter().enumerate() {
            let call_id = index as u64 + 1;
            // Secrets in a tool input: values, nested values, array members, AND keys — the same
            // four shapes the emitter-side seam scrubs.
            let mut input = json!({
                "url": format!("https://api.example.com?token={secret}"),
                "headers": { "Authorization": format!("Bearer {secret}") },
                "argv": ["curl", secret],
                "nested": { "deep": { "deeper": secret } },
            });
            input[secret.to_string()] = json!("a secret used as a JSON key");
            fleet.apply(
                &activity(1, "worker", call(call_id, "http_request", input)),
                t0,
            );
            // And in an observation payload.
            fleet.apply(
                &activity(
                    1,
                    "worker",
                    SpawnActivityEvent::Observation {
                        observation: flux_evidence::Observation::new(
                            "plugin.audit",
                            flux_evidence::Phase::ToolFollowup,
                            json!({ "credential": secret, "note": format!("used {secret}") }),
                        ),
                    },
                ),
                t0,
            );
        }

        // Everything a surface can obtain from the projection, as one haystack.
        let rows = fleet.rows(t0);
        let mut haystack = format!("{rows:?}");
        for row in &rows {
            haystack.push_str(&row.role);
            haystack.push_str(&row.child_session_id);
            haystack.push_str(row.status.label());
            haystack.push_str(row.status.op().unwrap_or(""));
        }
        for secret in CORPUS {
            assert!(
                !haystack.contains(secret),
                "`{secret}` reached the surface projection: {haystack}"
            );
        }
        // Not a vacuous pass: the structural fields the surface *is* allowed to show did arrive.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].role, "worker");
        assert_eq!(
            rows[0].status,
            WorkerStatus::Running {
                op: "http_request".into()
            }
        );
    }

    #[test]
    fn a_role_or_op_name_cannot_forge_surface_chrome() {
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        fleet.apply(
            &activity(
                1,
                "impl\u{1b}[31m\r\nfake approval y/N",
                call(1, "read\u{1b}[2K\rspoof", json!({})),
            ),
            t0,
        );
        let rows = fleet.rows(t0);
        assert!(
            !rows[0].role.contains('\u{1b}') && !rows[0].role.contains('\n'),
            "role sanitized: {:?}",
            rows[0].role
        );
        let op = rows[0].status.op().expect("running");
        assert!(
            !op.contains('\u{1b}') && !op.contains('\r'),
            "op sanitized: {op:?}"
        );
    }

    #[test]
    fn a_long_identifier_is_truncated_on_a_char_boundary() {
        let mut fleet = FleetProjection::new();
        let t0 = Instant::now();
        let wide = "é".repeat(MAX_OP_CHARS * 2);
        fleet.apply(&activity(1, &wide, call(1, &wide, json!({}))), t0);
        let rows = fleet.rows(t0);
        assert_eq!(rows[0].role.chars().count(), MAX_ROLE_CHARS);
        assert_eq!(
            rows[0].status.op().expect("running").chars().count(),
            MAX_OP_CHARS
        );
    }
}
