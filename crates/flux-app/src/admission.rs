//! Delivery admission control: the bound on how many deliveries run at once (A-129).
//!
//! A-112 made [`crate::App::deliver`] concurrent — the supervisor's actor loop stopped running
//! roots inline and started spawning each one into a `JoinSet`. That removed the last thing
//! bounding the system. The supervisor `mpsc`'s capacity *was* the backpressure, but only because
//! the loop was slow to dequeue; a loop that dequeues instantly applies none, so a webhook storm
//! could spawn roots without limit. This module puts a real bound back.
//!
//! ## What happens at the bound
//!
//! A delivery that arrives with every slot busy **waits**. It is not dropped and not rejected:
//! every delivery that reaches the supervisor queue eventually runs, and [`crate::App::deliver`]
//! still returns that delivery's runs. The two alternatives were rejected deliberately — *dropping*
//! silently loses work a webhook already acknowledged, and *rejecting* (the shape
//! `flux-server`'s per-realm A2A cap uses, `FLUX_A2A_MAX_INFLIGHT_PER_REALM`) is right for an HTTP
//! request that can be told 503 but wrong for a bus whose submitters have nowhere to put the event.
//!
//! The wait is applied **in the actor loop, before the root is spawned**. That placement is the
//! point: a saturated App stops dequeuing, its supervisor channel fills, and submitters block in
//! `send`. Backpressure therefore reaches the channel adapter that is producing the storm instead
//! of accumulating parked tasks behind a bound that only limits execution.
//!
//! ## The failure mode this chooses
//!
//! Blocking cannot lose work, but it can deadlock: if all `limit` in-flight deliveries are waiting
//! on something that only a *not yet admitted* delivery would produce, nothing drains. Re-entrant
//! `deliver` on the same App already fails fast, so the reachable shape is an external task holding
//! a delivery open while awaiting another one. [`DEFAULT_MAX_INFLIGHT_DELIVERIES`] is set well
//! above any fan-out flux itself drives so the ordinary cases — a slow sweep beside webhook intake
//! — never approach it; a program that deliberately couples deliveries must raise the limit past
//! its own fan-out width. Below saturation the bound changes nothing: unrelated channels still run
//! in parallel, which is A-112's contract.
//!
//! ## Observability
//!
//! [`DeliveryLoad`] separates the two states an operator otherwise cannot tell apart: `waiting` is
//! a delivery held by *this* bound, `in_flight` is a delivery that was admitted and is merely slow.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Deliveries allowed to run at once when nothing configures the bound.
///
/// Chosen the same way `flux-server`'s per-realm A2A cap was: comfortably above every fan-out flux
/// itself drives (the widest is A-112's 24-delivery wave), so the bound is invisible in normal
/// operation and only engages under a genuine storm.
pub const DEFAULT_MAX_INFLIGHT_DELIVERIES: usize = 64;

/// Environment override for [`DEFAULT_MAX_INFLIGHT_DELIVERIES`] — a positive integer. Named in the
/// style of `FLUX_A2A_MAX_INFLIGHT_PER_REALM`; a missing, zero or unparseable value falls back to
/// the default.
pub const MAX_INFLIGHT_DELIVERIES_ENV: &str = "FLUX_MAX_INFLIGHT_DELIVERIES";

/// The delivery bound in force for a newly built [`crate::App`], read once at construction.
/// Programmatic configuration ([`crate::App::with_max_inflight_deliveries`]) overrides this.
pub(crate) fn configured_max_inflight_deliveries() -> usize {
    limit_from_env(std::env::var(MAX_INFLIGHT_DELIVERIES_ENV).ok())
}

/// The override rule, factored out of [`configured_max_inflight_deliveries`] so it is testable
/// without mutating process-global environment under a parallel test runner.
fn limit_from_env(value: Option<String>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&limit| limit > 0)
        .unwrap_or(DEFAULT_MAX_INFLIGHT_DELIVERIES)
}

/// A snapshot of an [`App`](crate::App)'s delivery admission state (A-129).
///
/// The reason this exists rather than a single "busy" gauge: a delivery blocked by the bound and a
/// delivery that was admitted and is running slowly look identical from the outside, and the
/// operator response differs — the first says raise the limit or shed load upstream, the second
/// says look at the journey. `waiting > 0` is backpressure; `in_flight` near `limit` with
/// `waiting == 0` is a busy but healthy App.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryLoad {
    /// Deliveries admitted through the bound and currently running.
    pub in_flight: usize,
    /// Deliveries submitted to the supervisor and waiting for a slot.
    pub waiting: usize,
    /// The ceiling on `in_flight`.
    pub limit: usize,
}

impl DeliveryLoad {
    /// Whether the bound is currently holding work back, as opposed to merely being busy.
    pub fn is_backpressured(&self) -> bool {
        self.waiting > 0
    }
}

/// The bound itself: a semaphore of `limit` slots plus the two counters [`DeliveryLoad`] reports.
///
/// The semaphore is the enforcement; the counters are advisory (`Relaxed`) and exist only so a
/// waiting delivery is distinguishable from a slow one.
pub(crate) struct Admission {
    limit: usize,
    slots: Arc<Semaphore>,
    waiting: AtomicUsize,
    in_flight: AtomicUsize,
}

impl Admission {
    /// A bound of `limit` concurrent deliveries. `0` is clamped to `1`: there is deliberately no
    /// "unbounded" setting, because unbounded is the defect this exists to close.
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        let limit = limit.max(1);
        Arc::new(Self {
            limit,
            slots: Arc::new(Semaphore::new(limit)),
            waiting: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
        })
    }

    /// Record a delivery about to be handed to the supervisor queue. Called *before* the send, so a
    /// submission is never invisible in the window between enqueue and admission.
    ///
    /// The count is owned by the returned [`Submission`] until the send lands, at which point
    /// [`Submission::enqueued`] hands it to the actor loop (which clears it on admission). A guard
    /// dropped without that call subtracts the count again — see [`Submission`] for why that is a
    /// guard rather than a paired `abandon` call.
    pub(crate) fn submit(self: &Arc<Self>) -> Submission {
        self.waiting.fetch_add(1, Ordering::Relaxed);
        Submission {
            admission: Some(self.clone()),
        }
    }

    /// Wait for a slot. Resolves when the delivery may run; the returned guard holds the slot for
    /// as long as the root — cascade included — is executing.
    pub(crate) async fn admit(self: &Arc<Self>) -> DeliverySlot {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("the delivery admission semaphore is never closed");
        self.waiting.fetch_sub(1, Ordering::Relaxed);
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        DeliverySlot {
            admission: self.clone(),
            _permit: permit,
        }
    }

    pub(crate) fn load(&self) -> DeliveryLoad {
        DeliveryLoad {
            in_flight: self.in_flight.load(Ordering::Relaxed),
            waiting: self.waiting.load(Ordering::Relaxed),
            limit: self.limit,
        }
    }
}

/// A counted-but-not-yet-enqueued submission, held between [`Admission::submit`] and the send that
/// hands it to the actor loop.
///
/// This is a guard rather than a `submit`/`abandon` pair because the send it spans is an `await`:
/// `Sender::send` blocks once the queue is full, which under a storm is the normal case, and a
/// caller whose `deliver` future is dropped there (a `timeout`, a `select!` losing branch) would
/// otherwise leave its count behind forever. `waiting > 0` *is* the definition of
/// [`DeliveryLoad::is_backpressured`], so a leaked count is not a cosmetic drift — it makes the App
/// claim backpressure permanently, which is exactly the signal A-129 exists to make trustworthy.
/// Dropping on the cancellation path is the only behaviour that cannot be forgotten at a call site.
#[must_use = "an unheld submission is abandoned immediately; call `enqueued` once the send lands"]
pub(crate) struct Submission {
    /// `Some` while this guard still owns the count; `None` once the actor loop owns it.
    admission: Option<Arc<Admission>>,
}

impl Submission {
    /// The send landed: the actor loop now owns this count and will clear it on admission.
    /// Disarms the guard so dropping it is inert.
    pub(crate) fn enqueued(mut self) {
        self.admission = None;
    }
}

impl Drop for Submission {
    fn drop(&mut self) {
        if let Some(admission) = &self.admission {
            admission.waiting.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// One occupied delivery slot. Dropping it — on completion, error, or cancellation of the root's
/// task — releases the slot and admits whichever delivery has been waiting longest.
pub(crate) struct DeliverySlot {
    admission: Arc<Admission>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for DeliverySlot {
    fn drop(&mut self) {
        self.admission.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_limit_is_clamped_rather_than_meaning_unbounded() {
        assert_eq!(Admission::new(0).load().limit, 1);
    }

    #[test]
    fn the_env_override_takes_a_positive_integer_and_otherwise_defaults() {
        assert_eq!(limit_from_env(Some("7".into())), 7);
        assert_eq!(
            limit_from_env(None),
            DEFAULT_MAX_INFLIGHT_DELIVERIES,
            "an unset override leaves the documented default"
        );
        for rejected in ["0", "-1", "many", ""] {
            assert_eq!(
                limit_from_env(Some(rejected.into())),
                DEFAULT_MAX_INFLIGHT_DELIVERIES,
                "`{rejected}` is not a usable bound and must not disable it"
            );
        }
    }

    #[tokio::test]
    async fn load_separates_waiting_from_in_flight() {
        let admission = Admission::new(1);
        assert_eq!(
            admission.load(),
            DeliveryLoad {
                in_flight: 0,
                waiting: 0,
                limit: 1
            }
        );

        admission.submit().enqueued();
        admission.submit().enqueued();
        assert_eq!(admission.load().waiting, 2, "both submissions are visible");

        let first = admission.admit().await;
        let load = admission.load();
        assert_eq!(
            (load.in_flight, load.waiting),
            (1, 1),
            "one delivery runs, one is held by the bound"
        );
        assert!(load.is_backpressured());

        drop(first);
        let second = admission.admit().await;
        let load = admission.load();
        assert_eq!(
            (load.in_flight, load.waiting),
            (1, 0),
            "the released slot admitted the waiting delivery"
        );
        assert!(
            !load.is_backpressured(),
            "a busy-but-not-blocked App is not backpressured"
        );
        drop(second);
        assert_eq!(admission.load().in_flight, 0);
    }

    #[test]
    fn a_submission_that_never_reached_the_queue_stops_counting_as_waiting() {
        let admission = Admission::new(4);
        // The send failed (a stopped supervisor) or the submitting future was dropped mid-`send`
        // (a `timeout` around `deliver` while the queue was full). Either way the guard is dropped
        // without `enqueued`, and phantom backpressure must not survive it: `waiting > 0` is what
        // `is_backpressured` reports, so a leak here would pin the App to "backpressured" forever.
        drop(admission.submit());
        assert_eq!(admission.load().waiting, 0);
        assert!(!admission.load().is_backpressured());
    }

    #[test]
    fn an_enqueued_submission_keeps_its_count_for_the_actor_to_clear() {
        let admission = Admission::new(4);
        admission.submit().enqueued();
        assert_eq!(
            admission.load().waiting,
            1,
            "a submission the actor now owns stays visible until it is admitted"
        );
    }
}
