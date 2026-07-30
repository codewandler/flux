//! Host-set ceilings on what a runtime **uses**, as distinct from what it **spends** (C-290).
//!
//! `AgentSpec::max_iterations`, `max_model_calls`, `max_tokens`, `ConsultConfig::max_calls` and
//! `Limits::turn_token_budget` all bound *spend*. Nothing bounded *use*: the only concurrency
//! control in the tree was `ServerConfig::max_inflight_per_principal`, which is server-side and
//! per-principal, so an embedding host could not bound a runtime it constructed in-process.
//!
//! [`ResourceLimits`] closes that gap with two ceilings, both enforced inside the safety envelope
//! ([`Executor::dispatch`](crate::Executor::dispatch)) — the one funnel every tool call already
//! traverses, which is what makes them apply to in-process embedding and not only to `flux-server`:
//!
//! * **[`max_concurrent_tool_calls`](ResourceLimits::max_concurrent_tool_calls)** — how many tool
//!   calls may be inside `Tool::execute` simultaneously.
//! * **[`max_retained_result_bytes`](ResourceLimits::max_retained_result_bytes)** — how many bytes
//!   of tool results the executor may retain in its deterministic op cache.
//!
//! # Why there is no `max_memory_bytes`
//!
//! A process-wide RSS ceiling is not something a library can honestly enforce. A Rust library
//! cannot observe or refuse an allocation made by its caller, by the provider SDK, by a plugin
//! subprocess, or by the allocator's own arenas; and it cannot unwind a `Vec` growth that already
//! succeeded. A knob that reported "you are protected to 512 MiB" while doing none of that would be
//! worse than no knob, so this module does not ship one. What it ships instead is a bound on the
//! one structure the runtime itself retains and grows without a byte limit — the op cache, which
//! was bounded only by an entry count (512), so 512 large file reads retained an unbounded number
//! of bytes. Evicting from it is correctness-neutral: a miss re-runs the op.
//!
//! The other candidates named in C-290 are deliberately **not** bounded here, because bounding them
//! would be a silent truncation of an audit or conversational record rather than a cache eviction:
//! the evidence log (`flux-evidence`) drives reactions, metrics and the audit trail, and the
//! transcript is owned by the session store and already has an explicit, host-set
//! compaction threshold (`ClientBuilder::with_compaction`) and context budget
//! (`ClientBuilder::context_budget`).

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

/// How long a tool call waits for a concurrency slot before it is refused, when the host set a
/// [`max_concurrent_tool_calls`](ResourceLimits::max_concurrent_tool_calls) but no explicit
/// [`tool_call_queue_timeout`](ResourceLimits::tool_call_queue_timeout): 30 seconds.
///
/// **What this does and does not guarantee.** There is no *sentinel* for "wait forever": no value
/// of the timeout is interpreted as unbounded, and this default binds whenever the host does not
/// set one. An unbounded queue is indistinguishable from a hang at the call site, which is why the
/// unset case has a finite answer.
///
/// It is **not** a ceiling on the wait. The timeout is an arbitrary [`Duration`] (and through
/// `[limits] tool_call_queue_timeout_ms` an arbitrary `u64` of milliseconds), and nothing clamps
/// it: `u64::MAX` milliseconds is ~584,942,417 years, and `tokio::time::timeout` honors an absurd
/// duration rather than capping it. An operator who writes that has chosen a hang, deliberately and
/// visibly. Nothing here overrides that choice, because any maximum this module could pick would be
/// a guess that breaks a legitimate long-queue deployment — a batch host may genuinely prefer
/// queueing for an hour over being refused.
pub const DEFAULT_TOOL_CALL_QUEUE_TIMEOUT: Duration = Duration::from_secs(30);

/// The op cache's entry-count bound, unchanged from before the byte ceiling existed. Both bounds
/// apply; whichever is reached first resets the cache.
pub(crate) const MAX_RETAINED_RESULTS: usize = 512;

tokio::task_local! {
    /// Identities (semaphore addresses) of the concurrency ceilings whose slots the currently
    /// executing tool call already holds.
    ///
    /// A tool may dispatch through the same executor while it runs (an adapter op assembling a
    /// nested runtime, a composite). That nested call is *part of* an execution the outer slot is
    /// already counting, so it must not queue behind itself — at a ceiling of 1 that is a deadlock.
    /// Keying on the semaphore's identity rather than a bare flag keeps the exemption exact: a
    /// nested call against a *different* ceiling (a separately configured runtime) still acquires
    /// its own slot.
    ///
    /// Tokio task-locals scope to the future being polled, not to the task, so sibling branches of
    /// a `parallel` block — which `join_all`s on one task — never see each other's entries.
    static HELD_SLOTS: Vec<usize>;
}

/// Ceilings a host sets when it constructs a runtime. Cheap to [`Clone`]: the concurrency ceiling
/// is a shared handle, so every executor derived from one configured environment counts against the
/// **same** budget rather than getting a private copy of it.
///
/// Everything is off by default — an unconfigured runtime behaves exactly as it did before C-290.
#[derive(Debug, Clone, Default)]
pub struct ResourceLimits {
    max_concurrent_tool_calls: Option<usize>,
    queue_timeout: Option<Duration>,
    max_retained_result_bytes: Option<usize>,
    /// Present iff a concurrency ceiling is configured. Shared across clones — that sharing is the
    /// whole point (see the type doc).
    slots: Option<Arc<Semaphore>>,
}

impl ResourceLimits {
    /// Unbounded — the default.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cap the number of tool calls that may be inside `Tool::execute` at once, across every
    /// executor derived from the environment this is installed on.
    ///
    /// `0` is meaningless as a ceiling (it would refuse every call), so it is read as `1`. A call
    /// that arrives at a saturated runtime waits up to
    /// [`tool_call_queue_timeout`](Self::tool_call_queue_timeout) and is then refused with an
    /// actionable message. It never truncates; and it never waits *unboundedly by default*, though
    /// a host that sets an absurd timeout gets an absurd wait — see
    /// [`DEFAULT_TOOL_CALL_QUEUE_TIMEOUT`].
    pub fn with_max_concurrent_tool_calls(mut self, n: usize) -> Self {
        let n = n.max(1);
        self.max_concurrent_tool_calls = Some(n);
        self.slots = Some(Arc::new(Semaphore::new(n)));
        self
    }

    /// How long a tool call waits for a slot before being refused. Defaults to
    /// [`DEFAULT_TOOL_CALL_QUEUE_TIMEOUT`]; `Duration::ZERO` refuses immediately rather than
    /// queueing at all. Ignored when no concurrency ceiling is set.
    ///
    /// Not clamped. A very large value is honored as written, so it is the one way to turn the
    /// refusal back into an effectively unbounded wait — deliberately, never by default and never
    /// via a sentinel. See [`DEFAULT_TOOL_CALL_QUEUE_TIMEOUT`] for why no maximum is imposed.
    pub fn with_tool_call_queue_timeout(mut self, timeout: Duration) -> Self {
        self.queue_timeout = Some(timeout);
        self
    }

    /// Cap the bytes of tool results the executor retains in its deterministic op cache. Reaching
    /// the ceiling evicts; a result larger than the whole ceiling is never retained. Eviction is
    /// correctness-neutral — a cache miss re-runs the op — so this bound is never observable as a
    /// truncated result.
    pub fn with_max_retained_result_bytes(mut self, bytes: usize) -> Self {
        self.max_retained_result_bytes = Some(bytes);
        self
    }

    /// Build the runtime ceilings from a file-configured `[limits]` table.
    pub fn from_config(limits: &flux_config::Limits) -> Self {
        let mut resolved = Self::new();
        if let Some(n) = limits.max_concurrent_tool_calls {
            resolved = resolved.with_max_concurrent_tool_calls(n);
        }
        if let Some(ms) = limits.tool_call_queue_timeout_ms {
            resolved = resolved.with_tool_call_queue_timeout(Duration::from_millis(ms));
        }
        if let Some(bytes) = limits.max_retained_result_bytes {
            resolved = resolved.with_max_retained_result_bytes(bytes);
        }
        resolved
    }

    /// The configured simultaneous-tool-call ceiling, if any.
    pub fn max_concurrent_tool_calls(&self) -> Option<usize> {
        self.max_concurrent_tool_calls
    }

    /// The wait a queued tool call tolerates before refusal.
    pub fn tool_call_queue_timeout(&self) -> Duration {
        self.queue_timeout
            .unwrap_or(DEFAULT_TOOL_CALL_QUEUE_TIMEOUT)
    }

    /// The configured retained-result byte ceiling, if any.
    pub fn max_retained_result_bytes(&self) -> Option<usize> {
        self.max_retained_result_bytes
    }

    /// Whether any ceiling at all is configured.
    pub fn is_unbounded(&self) -> bool {
        self.max_concurrent_tool_calls.is_none() && self.max_retained_result_bytes.is_none()
    }

    /// Take a concurrency slot for one tool execution, or refuse.
    ///
    /// Returns an inert slot when no ceiling is configured, and also when this task already holds a
    /// slot on this same ceiling (see [`HELD_SLOTS`]).
    pub(crate) async fn acquire_execution_slot(
        &self,
    ) -> std::result::Result<ExecutionSlot, ConcurrencyRefusal> {
        let Some(slots) = self.slots.as_ref() else {
            return Ok(ExecutionSlot::inert());
        };
        let id = Arc::as_ptr(slots) as usize;
        if HELD_SLOTS
            .try_with(|held| held.contains(&id))
            .unwrap_or(false)
        {
            return Ok(ExecutionSlot::inert());
        }
        let limit = self.max_concurrent_tool_calls.unwrap_or(1);
        match slots.clone().try_acquire_owned() {
            Ok(permit) => return Ok(ExecutionSlot::held(permit, id)),
            // Never closed: nothing calls `Semaphore::close`. Treat it as "no ceiling" rather than
            // failing a call for a condition that cannot happen.
            Err(TryAcquireError::Closed) => return Ok(ExecutionSlot::inert()),
            Err(TryAcquireError::NoPermits) => {}
        }
        let started = Instant::now();
        let timeout = self.tool_call_queue_timeout();
        match tokio::time::timeout(timeout, slots.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Ok(ExecutionSlot::held(permit, id)),
            Ok(Err(_closed)) => Ok(ExecutionSlot::inert()),
            Err(_elapsed) => Err(ConcurrencyRefusal {
                limit,
                waited: started.elapsed(),
            }),
        }
    }
}

/// One tool execution's claim on the concurrency ceiling. Dropping it frees the slot.
pub(crate) struct ExecutionSlot {
    /// `None` when no ceiling is configured, or when the call is re-entrant on a slot this task
    /// already holds.
    _permit: Option<OwnedSemaphorePermit>,
    /// The ceiling's identity, present iff a permit was actually taken.
    id: Option<usize>,
}

impl ExecutionSlot {
    fn inert() -> Self {
        Self {
            _permit: None,
            id: None,
        }
    }

    fn held(permit: OwnedSemaphorePermit, id: usize) -> Self {
        Self {
            _permit: Some(permit),
            id: Some(id),
        }
    }

    /// Run `future` marked as holding this slot, so a nested dispatch through the same ceiling does
    /// not queue behind the execution it is part of.
    pub(crate) async fn hold<F: Future>(&self, future: F) -> F::Output {
        let Some(id) = self.id else {
            return future.await;
        };
        let mut held = HELD_SLOTS.try_with(Clone::clone).unwrap_or_default();
        held.push(id);
        HELD_SLOTS.scope(held, future).await
    }
}

/// A tool call refused because the runtime's concurrency ceiling stayed saturated for the whole
/// queue timeout. Transient by construction: the same call may succeed once a slot frees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcurrencyRefusal {
    /// The ceiling that bound this call.
    pub limit: usize,
    /// How long the call waited for a slot before giving up.
    pub waited: Duration,
}

impl ConcurrencyRefusal {
    /// The refusal the caller sees: what bound it, for how long, and what to do about it.
    pub fn message(&self, op: &str) -> String {
        format!(
            "`{op}` refused: the runtime's concurrency limit of {} simultaneous tool call(s) stayed saturated for {} ms. \
             Retry once a call completes, run fewer operations at once, or raise `max_concurrent_tool_calls`.",
            self.limit,
            self.waited.as_millis()
        )
    }
}

/// The bytes one retained result occupies — both faces the executor keeps.
pub(crate) fn retained_bytes(result: &crate::ToolResult) -> usize {
    result.content.len() + result.view.as_deref().map_or(0, str::len)
}

/// The executor's deterministic op-result cache, bounded by entry count **and** by retained bytes.
#[derive(Default)]
pub(crate) struct OpCache {
    entries: std::collections::HashMap<u64, crate::ToolResult>,
    bytes: usize,
}

impl OpCache {
    pub(crate) fn get(&self, key: &u64) -> Option<crate::ToolResult> {
        self.entries.get(key).cloned()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Bytes currently retained.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Retain `result` under `key`, honoring both bounds. A full reset never affects correctness,
    /// only reuse — which is why the crude "clear on overflow" policy the entry bound already used
    /// is the right shape for the byte bound too.
    pub(crate) fn insert(&mut self, key: u64, result: crate::ToolResult, budget: Option<usize>) {
        let size = retained_bytes(&result);
        if let Some(budget) = budget {
            // A single result bigger than the entire ceiling can never be retained without
            // breaching it — never cache it, rather than clearing the cache to make room and
            // breaching anyway.
            if size > budget {
                return;
            }
            if self.bytes + size > budget {
                self.clear();
            }
        }
        if self.entries.len() >= MAX_RETAINED_RESULTS {
            self.clear();
        }
        if let Some(previous) = self.entries.insert(key, result) {
            self.bytes -= retained_bytes(&previous);
        }
        self.bytes += size;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolResult;

    #[test]
    fn unconfigured_limits_bound_nothing() {
        let limits = ResourceLimits::new();
        assert!(limits.is_unbounded());
        assert_eq!(limits.max_concurrent_tool_calls(), None);
        assert_eq!(limits.max_retained_result_bytes(), None);
        assert_eq!(
            limits.tool_call_queue_timeout(),
            DEFAULT_TOOL_CALL_QUEUE_TIMEOUT
        );
    }

    /// A ceiling of zero would refuse everything; it is read as one instead.
    #[test]
    fn a_zero_concurrency_ceiling_is_read_as_one() {
        let limits = ResourceLimits::new().with_max_concurrent_tool_calls(0);
        assert_eq!(limits.max_concurrent_tool_calls(), Some(1));
    }

    /// The `[limits]` table maps straight onto the runtime ceilings, and an absent key leaves the
    /// corresponding ceiling off rather than inventing one.
    #[test]
    fn the_config_table_maps_onto_the_runtime_ceilings() {
        let limits = ResourceLimits::from_config(&flux_config::Limits {
            max_concurrent_tool_calls: Some(4),
            tool_call_queue_timeout_ms: Some(2_500),
            max_retained_result_bytes: Some(1_048_576),
            ..Default::default()
        });
        assert_eq!(limits.max_concurrent_tool_calls(), Some(4));
        assert_eq!(
            limits.tool_call_queue_timeout(),
            Duration::from_millis(2_500)
        );
        assert_eq!(limits.max_retained_result_bytes(), Some(1_048_576));

        let empty = ResourceLimits::from_config(&flux_config::Limits::default());
        assert!(empty.is_unbounded());
        assert_eq!(
            empty.tool_call_queue_timeout(),
            DEFAULT_TOOL_CALL_QUEUE_TIMEOUT
        );
    }

    /// The queue timeout is deliberately **not** clamped, so this pins the documented behavior: a
    /// host that asks for an absurd wait gets one, and turning that into a clamp is a deliberate
    /// change rather than an accident. No sentinel means "wait forever" — the only way there is to
    /// write an absurd number, and the 30s default binds when nothing is written.
    #[test]
    fn an_absurd_queue_timeout_is_honored_rather_than_clamped() {
        let limits = ResourceLimits::from_config(&flux_config::Limits {
            max_concurrent_tool_calls: Some(1),
            tool_call_queue_timeout_ms: Some(u64::MAX),
            ..Default::default()
        });
        assert_eq!(
            limits.tool_call_queue_timeout(),
            Duration::from_millis(u64::MAX),
            "nothing caps the configured wait — the docs must keep saying so"
        );
        assert!(
            limits.tool_call_queue_timeout() > Duration::from_secs(60 * 60 * 24 * 365),
            "and it is long enough to be a hang in practice"
        );
    }

    /// Clones share the ceiling — that is what makes it a runtime-wide budget rather than a
    /// per-executor one.
    #[tokio::test]
    async fn clones_share_one_concurrency_budget() {
        let limits = ResourceLimits::new().with_max_concurrent_tool_calls(1);
        let clone = limits.clone();
        let held = limits.acquire_execution_slot().await.expect("first slot");
        let refused = clone
            .clone()
            .with_tool_call_queue_timeout(Duration::from_millis(20))
            .acquire_execution_slot()
            .await;
        assert!(
            refused.is_err(),
            "a clone must count against the same budget"
        );
        drop(held);
        assert!(clone.acquire_execution_slot().await.is_ok());
    }

    /// A nested dispatch inside a running tool must not queue behind the execution it belongs to.
    #[tokio::test]
    async fn a_re_entrant_call_does_not_deadlock_on_its_own_slot() {
        let limits = ResourceLimits::new()
            .with_max_concurrent_tool_calls(1)
            .with_tool_call_queue_timeout(Duration::from_millis(50));
        let outer = limits.acquire_execution_slot().await.expect("outer slot");
        let nested = outer
            .hold(async { limits.acquire_execution_slot().await })
            .await;
        assert!(
            nested.is_ok(),
            "a nested call on a held ceiling must be exempt, not deadlocked"
        );
    }

    /// A *different* ceiling is not exempted by an outer slot.
    #[tokio::test]
    async fn a_nested_call_on_a_different_ceiling_still_queues() {
        let outer_limits = ResourceLimits::new().with_max_concurrent_tool_calls(1);
        let inner_limits = ResourceLimits::new()
            .with_max_concurrent_tool_calls(1)
            .with_tool_call_queue_timeout(Duration::from_millis(20));
        let inner_held = inner_limits
            .acquire_execution_slot()
            .await
            .expect("inner slot");
        let outer = outer_limits.acquire_execution_slot().await.expect("outer");
        let nested = outer
            .hold(async { inner_limits.acquire_execution_slot().await })
            .await;
        assert!(
            nested.is_err(),
            "holding one ceiling must not exempt a call from another"
        );
        drop(inner_held);
    }

    #[test]
    fn a_refusal_names_the_limit_and_the_knob() {
        let message = ConcurrencyRefusal {
            limit: 4,
            waited: Duration::from_millis(250),
        }
        .message("read");
        assert!(message.contains("concurrency limit of 4"));
        assert!(message.contains("max_concurrent_tool_calls"));
        assert!(message.contains("250 ms"));
    }

    #[test]
    fn the_byte_ceiling_evicts_instead_of_growing() {
        let mut cache = OpCache::default();
        for key in 0..10u64 {
            cache.insert(key, ToolResult::ok("x".repeat(1_000)), Some(4_096));
            assert!(cache.bytes() <= 4_096, "retained {} bytes", cache.bytes());
        }
        assert!(cache.bytes() > 0);
    }

    #[test]
    fn a_result_larger_than_the_whole_ceiling_is_never_retained() {
        let mut cache = OpCache::default();
        cache.insert(1, ToolResult::ok("x".repeat(100)), Some(4_096));
        cache.insert(2, ToolResult::ok("x".repeat(5_000)), Some(4_096));
        assert_eq!(cache.bytes(), 100, "the oversized result must not be kept");
        assert!(cache.get(&2).is_none());
        assert!(cache.get(&1).is_some(), "and it must not evict what fits");
    }

    #[test]
    fn re_inserting_a_key_does_not_double_count_it() {
        let mut cache = OpCache::default();
        cache.insert(1, ToolResult::ok("x".repeat(100)), Some(4_096));
        cache.insert(1, ToolResult::ok("x".repeat(200)), Some(4_096));
        assert_eq!(cache.bytes(), 200);
    }

    #[test]
    fn an_unbounded_cache_still_honors_the_entry_bound() {
        let mut cache = OpCache::default();
        for key in 0..=(MAX_RETAINED_RESULTS as u64) {
            cache.insert(key, ToolResult::ok("x"), None);
        }
        assert_eq!(cache.bytes(), 1, "the entry bound reset the cache");
    }
}
