//! Live reporting of connect-phase recovery (C-181).
//!
//! [`NativeProvider::stream`](crate::NativeProvider::stream) retries transient failures with
//! exponential backoff and force-refreshes an expired OAuth token on a 401. Those recoveries used
//! to surface only as `tracing::warn!`, and no product surface installs a subscriber — so a turn
//! that slept 30s through backoffs was indistinguishable from a turn where the model was simply
//! slow. This module is the seam that makes them visible.
//!
//! An observer is installed **lexically**, around one model call, via [`with_retry_observer`], and
//! reported to **before** the backoff sleep — the point of the whole thing is that the user learns
//! about the wait while it is still ahead of them. It is deliberately not a field on the provider
//! or the request: providers are built once per session and shared across turns, while the surface
//! sink is per-turn, so a field would need a mutable shared slot rewritten on every turn. A
//! task-local is scoped to exactly the call it belongs to, isolates concurrent turns by
//! construction, and restores the previous value on nesting.
//!
//! No observer installed is a no-op, so every embedder, test, and non-interactive path is
//! unaffected.

use std::sync::Arc;
use std::time::Duration;

/// Why a connect attempt is being recovered from.
///
/// `#[non_exhaustive]`: recovery kinds grow (a rate-limit-header-driven wait is a plausible next
/// one), and this crate is published — without the attribute, every new variant would break every
/// downstream exhaustive match, forcing a MINOR bump for an additive change. Downstream matches
/// carry a wildcard arm instead. (The same trap froze host-kit's `HandshakeInfo` at 1.0.0.)
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetryReason {
    /// The provider answered with a retryable status — 429 or 5xx (see
    /// [`is_retryable_status`](crate::is_retryable_status)).
    Status(u16),
    /// The request never completed: connect/TLS/DNS failure, or an idle-read timeout.
    Transport(String),
    /// A 401 forced one OAuth token refresh; the attempt is repeated with the freshened token.
    /// Not backed off — the delay on this event is zero.
    OauthRefresh,
    /// An alternative [`StreamTransport`](crate::StreamTransport) (e.g. the codex WebSocket)
    /// failed to connect and the call fell back to the generic HTTP path. Not a retry of the same
    /// leg and not backed off, but the same class of "something went wrong and we recovered"
    /// the user is otherwise blind to.
    TransportFallback(String),
}

impl RetryReason {
    /// A short human label for a status bar or log line.
    pub fn label(&self) -> String {
        match self {
            Self::Status(status) => format!("http {status}"),
            Self::Transport(_) => "transport".to_string(),
            Self::OauthRefresh => "auth refresh".to_string(),
            Self::TransportFallback(_) => "transport fallback".to_string(),
        }
    }

    /// Whether this event consumed one of the connect loop's bounded retry attempts. The OAuth
    /// refresh and the transport fallback each happen at most once and are budgeted separately.
    pub fn counts_against_budget(&self) -> bool {
        matches!(self, Self::Status(_) | Self::Transport(_))
    }
}

/// One connect-phase recovery, reported before the wait it announces.
///
/// `#[non_exhaustive]` so a future field (e.g. elapsed time in the connect loop) stays additive for
/// this published crate. Reads are unaffected — the fields stay `pub` — but downstream construction
/// goes through [`RetryEvent::new`], which is the documented path for an external
/// [`Provider`](crate::Provider) reporting its own recoveries via [`report_retry`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RetryEvent {
    /// The provider name (`Provider::name`).
    pub provider: String,
    /// The resolved model id the call is for.
    pub model: String,
    /// 1-based number of the attempt that just failed.
    pub attempt: u32,
    /// The connect loop's retry budget ([`DEFAULT_MAX_RETRIES`](crate::DEFAULT_MAX_RETRIES) unless
    /// overridden). Zero for reasons that do not consume the budget.
    pub max_attempts: u32,
    /// How long the caller is about to sleep before trying again. Zero when the retry is immediate.
    pub delay: Duration,
    pub reason: RetryReason,
}

impl RetryEvent {
    /// Build an event for [`report_retry`]. The constructor exists because the struct is
    /// `#[non_exhaustive]` — a literal would stop compiling downstream the first time a field is
    /// added, which is exactly the churn the attribute is there to prevent.
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        attempt: u32,
        max_attempts: u32,
        delay: Duration,
        reason: RetryReason,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            attempt,
            max_attempts,
            delay,
            reason,
        }
    }
}

/// Receives [`RetryEvent`]s for the model call it is scoped around.
///
/// Implementations must not block: they are called on the connect path, inline, immediately before
/// the backoff sleep.
pub trait RetryObserver: Send + Sync {
    fn retrying(&self, event: &RetryEvent);
}

tokio::task_local! {
    static ACTIVE_RETRY_OBSERVER: Arc<dyn RetryObserver>;
}

/// Drive `future` with `observer` installed for any provider connect it performs.
///
/// Scoped to the task that polls the future, matching `scope_runtime_turn`'s discipline. A provider
/// that moved its connect onto a detached task would fall outside the scope — none does, and the
/// connect loop is awaited inline by the model stage.
pub async fn with_retry_observer<F>(observer: Arc<dyn RetryObserver>, future: F) -> F::Output
where
    F: std::future::Future,
{
    ACTIVE_RETRY_OBSERVER.scope(observer, future).await
}

/// Report one recovery to the observer scoped around the current call, if any. Cheap and infallible
/// when none is installed.
///
/// This is the producer half of the seam, public so a [`Provider`](crate::Provider) implemented
/// outside this crate — an embedder's client, a gateway wrapper — can report its own recoveries on
/// the same channel the built-in HTTP path uses, instead of going dark.
pub fn report_retry(event: RetryEvent) {
    let _ = ACTIVE_RETRY_OBSERVER.try_with(|observer| observer.retrying(&event));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<RetryEvent>>);

    impl RetryObserver for Recorder {
        fn retrying(&self, event: &RetryEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    fn event(attempt: u32) -> RetryEvent {
        RetryEvent {
            provider: "test".into(),
            model: "m".into(),
            attempt,
            max_attempts: 6,
            delay: Duration::from_millis(500),
            reason: RetryReason::Status(429),
        }
    }

    #[tokio::test]
    async fn notify_without_an_observer_is_a_no_op() {
        // No panic, no scope — the un-instrumented path every embedder takes by default.
        report_retry(event(1));
    }

    #[tokio::test]
    async fn an_installed_observer_receives_events_from_inside_the_scope() {
        let recorder = Arc::new(Recorder::default());
        with_retry_observer(recorder.clone(), async {
            report_retry(event(1));
            report_retry(event(2));
        })
        .await;
        let seen = recorder.0.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].attempt, 1);
        assert_eq!(seen[1].attempt, 2);
    }

    #[tokio::test]
    async fn concurrent_scopes_stay_isolated() {
        let a = Arc::new(Recorder::default());
        let b = Arc::new(Recorder::default());
        let (ra, rb) = (a.clone(), b.clone());
        tokio::join!(
            with_retry_observer(ra, async { report_retry(event(1)) }),
            with_retry_observer(rb, async {
                report_retry(event(2));
                report_retry(event(3));
            }),
        );
        assert_eq!(a.0.lock().unwrap().len(), 1);
        assert_eq!(b.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn only_backed_off_reasons_consume_the_budget() {
        assert!(RetryReason::Status(503).counts_against_budget());
        assert!(RetryReason::Transport("dns".into()).counts_against_budget());
        assert!(!RetryReason::OauthRefresh.counts_against_budget());
        assert!(!RetryReason::TransportFallback("ws".into()).counts_against_budget());
    }
}
