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
//! * **[`max_evidence_payload_bytes`](ResourceLimits::max_evidence_payload_bytes)** — how many bytes
//!   of observation payload the shared evidence log may retain (C-298).
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
//! The transcript is deliberately **not** bounded here: it is owned by the session store and already
//! has an explicit, host-set compaction threshold (`ClientBuilder::with_compaction`) and context
//! budget (`ClientBuilder::context_budget`).
//!
//! # Why the evidence log is bounded by *payload* and not by *entries* (C-298)
//!
//! C-290 declined to bound the evidence log at all, because it drives reactions, `metrics()` and the
//! audit trail, and dropping the oldest observations to fit a ceiling would be a silent truncation of
//! an audit record rather than a cache eviction. That reasoning was right and still holds — three
//! readers outside `flux-evidence` depend on the log's *shape*, not just its contents:
//!
//! * `flux-flow`'s durable event-store flush (C-14) slices the unflushed tail by **absolute index**
//!   into `EvidenceLog::all()`, so compacting the front would silently stop persisting observations.
//! * `flux-tools`' `metrics` op reports **cumulative** `by_kind` counts (`tool_call`, `tool_error`,
//!   `turn.iteration`) — dropping entries makes a progress signal a model branches on go backwards.
//! * `flux-flow` snapshots those same counts as per-turn `turn.iteration` / `subagent.usage`
//!   baselines and diffs against them, so a count that can *shrink* is a correctness hazard next to
//!   turn termination.
//!
//! So C-298 separates the two things the `Vec` was doing rather than adding an eviction knob. The
//! unbounded part is the arbitrary-size `data` payload — a `tool_call`'s permission subjects, a
//! flow's `observe(…)` argument — and that is what
//! [`max_evidence_payload_bytes`](ResourceLimits::max_evidence_payload_bytes) bounds, by eliding the
//! oldest payloads behind a self-describing marker. Every observation stays, in order, with its
//! `kind` and `phase`, so all three readers above are untouched, and an elided payload is legible as
//! elided rather than looking like one that was never there.
//!
//! What this does **not** do is bound the log's entry count, and no honest ceiling here could: an
//! entry ceiling means dropping entries, which is exactly what the three readers forbid. A long-lived
//! runtime therefore still retains a fixed-size header per observation. That residual is a fraction
//! of what it retained before — the payload was the dominant and the only *unbounded* term — and
//! saying so plainly is better than a knob that claims more than it delivers.

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
/// **Scope of that sharing (C-299).** Clones share the budget *within one agent* — including the
/// fresh executor a surface mints per run, which is what keeps `FlowClient::build_executor` from
/// escaping the ceiling. A **sub-agent** is different: it gets an
/// [`independent_copy`](Self::independent_copy), so the *concurrency* ceiling is **per agent**, not
/// one budget for the whole process. That is a deliberate, safety-driven choice, not an oversight —
/// see [`independent_copy`](Self::independent_copy) for the deadlock that sharing the execution
/// semaphore across the `task` boundary produces.
///
/// **What bounds the tree, then (C-444).** A per-agent ceiling that composes into an unbounded total
/// is only half a ceiling: `max_concurrent_tool_calls = N` with k live children permits N×(k+1)
/// simultaneous calls, and nothing bounded k. So a second, **tree-wide** ceiling rides alongside it —
/// [`max_live_agents`](Self::max_live_agents), enforced by a census that *is* shared across
/// [`independent_copy`](Self::independent_copy). Bounding k is what turns N×(k+1) into a finite
/// number. The two ceilings are deliberately different shapes because they have different failure
/// modes: the execution semaphore *queues* (and would deadlock if shared), while the agent census
/// *refuses immediately* (and so cannot deadlock, no matter how deep the tree).
///
/// Everything is off by default — an unconfigured runtime behaves exactly as it did before C-290.
/// [`autonomous`](Self::autonomous) is the preset an auto-approving SDK embedder gets by default
/// (C-444), because an unattended posture with no ceiling was the actual finding.
#[derive(Debug, Clone, Default)]
pub struct ResourceLimits {
    max_concurrent_tool_calls: Option<usize>,
    queue_timeout: Option<Duration>,
    max_retained_result_bytes: Option<usize>,
    max_evidence_payload_bytes: Option<usize>,
    max_live_agents: Option<usize>,
    /// Present iff a concurrency ceiling is configured. Shared across clones — that sharing is the
    /// whole point (see the type doc).
    slots: Option<Arc<Semaphore>>,
    /// Present iff an agent ceiling is configured. Shared across clones **and** across
    /// [`independent_copy`](Self::independent_copy) — unlike `slots`, because this is the ceiling
    /// that bounds the whole delegated tree rather than one agent in it.
    agents: Option<Arc<AgentCensus>>,
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

    /// Cap how many agents may be **live at once across the whole delegated tree** — the root plus
    /// every transitively spawned sub-agent (C-444).
    ///
    /// This is the ceiling that makes
    /// [`max_concurrent_tool_calls`](Self::with_max_concurrent_tool_calls) bound a *tree* rather than
    /// an agent. Because a child gets its own execution budget (see
    /// [`independent_copy`](Self::independent_copy) for why it must), N per agent with an unbounded
    /// agent count is an unbounded total; with this set, the tree's simultaneous tool calls are
    /// bounded by `N × max_live_agents`, which is finite and stated.
    ///
    /// `0` is meaningless (the root itself is an agent), so it is read as `1` — which means "no
    /// delegation": the root runs and every `task` is refused.
    ///
    /// **This ceiling refuses; it never queues.** A spawn that would exceed it fails immediately with
    /// an actionable message rather than waiting for a sibling to finish. That is deliberate and it is
    /// what makes the census safe to share across the delegation boundary that the execution
    /// semaphore cannot cross: a waiting spawn could be waiting on an ancestor that is waiting on it,
    /// which is precisely the deadlock [`independent_copy`](Self::independent_copy) documents. A
    /// refusal has no such shape — the model reads it and proceeds with fewer children.
    pub fn with_max_live_agents(mut self, n: usize) -> Self {
        let n = n.max(1);
        self.max_live_agents = Some(n);
        self.agents = Some(Arc::new(AgentCensus::new(n)));
        self
    }

    /// Cap the bytes of observation `data` payload the shared evidence log retains (C-298).
    ///
    /// Unlike [`with_max_retained_result_bytes`](Self::with_max_retained_result_bytes) this is not a
    /// cache bound and so is **not** correctness-neutral: reaching it elides the *oldest* payloads.
    /// What makes that admissible rather than a silent truncation of an audit record is that no
    /// observation is dropped — count, order, `kind` and `phase` are preserved, so every reader that
    /// addresses the log by absolute index or counts it by kind is unaffected — and each elided
    /// payload is replaced by a self-describing marker naming this knob. See
    /// [`EvidenceLog::set_max_payload_bytes`](flux_evidence::EvidenceLog::set_max_payload_bytes) for
    /// what is and is not bounded, and where an elided payload can still be read in full.
    pub fn with_max_evidence_payload_bytes(mut self, bytes: usize) -> Self {
        self.max_evidence_payload_bytes = Some(bytes);
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
        if let Some(bytes) = limits.max_evidence_payload_bytes {
            resolved = resolved.with_max_evidence_payload_bytes(bytes);
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

    /// The configured evidence-payload ceiling, if any (C-298).
    pub fn max_evidence_payload_bytes(&self) -> Option<usize> {
        self.max_evidence_payload_bytes
    }

    /// The configured tree-wide live-agent ceiling, if any (C-444).
    pub fn max_live_agents(&self) -> Option<usize> {
        self.max_live_agents
    }

    /// Whether any ceiling at all is configured.
    pub fn is_unbounded(&self) -> bool {
        self.max_concurrent_tool_calls.is_none()
            && self.max_retained_result_bytes.is_none()
            && self.max_evidence_payload_bytes.is_none()
            && self.max_live_agents.is_none()
    }

    /// The ceilings an **autonomous** posture carries (C-444): the shape an SDK embedder gets by
    /// default when it chooses auto-approval, and the reason that choice is not a hole.
    ///
    /// Running without per-effect approval is a valid posture, not safety switched off (C-463). What
    /// makes it valid is that the constraint budget moves from human latency to policy, isolation and
    /// **budgets** — so the budgets have to exist. These are that: a bounded number of simultaneous
    /// tool calls per agent, a bounded number of live agents across the tree (so the two compose to a
    /// finite total), and bounded retention.
    ///
    /// The numbers are deliberately generous rather than tight. They are a *ceiling on runaway*, not a
    /// throttle on legitimate work: an exploratory research agent should never notice them, while a
    /// delegated tree that has started multiplying hits something finite. A host with a real workload
    /// in mind should state its own — this is the floor for one that has not.
    pub fn autonomous() -> Self {
        Self::new()
            // Enough for a wide `parallel` block; far short of unbounded fan-out.
            .with_max_concurrent_tool_calls(16)
            // With the above: at most 16 × 8 = 128 simultaneous tool calls across the whole tree.
            .with_max_live_agents(8)
            // 64 MiB of op-cache retention; eviction is correctness-neutral (a miss re-runs the op).
            .with_max_retained_result_bytes(64 * 1024 * 1024)
            // 32 MiB of evidence payload; elision is legible as elision (C-298) and keeps every
            // observation's count, order, kind and phase intact.
            .with_max_evidence_payload_bytes(32 * 1024 * 1024)
    }

    /// Claim a place in the tree-wide agent census for one sub-agent, or refuse (C-444). The returned
    /// guard frees the place when the child's turn ends.
    ///
    /// `Ok(None)` when no agent ceiling is configured — the unbounded default.
    pub fn admit_agent(&self) -> std::result::Result<Option<AgentSlot>, AgentCensusRefusal> {
        match self.agents.as_ref() {
            None => Ok(None),
            Some(census) => census.clone().admit().map(Some),
        }
    }

    /// The same ceilings, with a **fresh concurrency budget** — the shape a sub-agent inherits
    /// (C-299).
    ///
    /// Every configured value is carried over, but the semaphore is new, so the child's tool calls
    /// are bounded by `max_concurrent_tool_calls` *independently of the parent's*. The byte ceilings
    /// are copied for the same reason they are per-executor anyway: each agent owns its own op cache
    /// and evidence log.
    ///
    /// # Why not one shared budget
    ///
    /// A single semaphore across parent and children is the stronger guarantee — it would bound total
    /// process concurrency instead of per-agent concurrency — and it **deadlocks**.
    ///
    /// On the conversational path the outermost agent-loop op holds the permit: `execute_batch` is a
    /// registered tool dispatched through [`Executor::dispatch`](crate::Executor::dispatch), so it
    /// takes a slot and keeps it for the whole batch — including the `task` call inside it, and that
    /// child's entire turn. (`task` itself takes no *additional* permit: it runs on the same Tokio
    /// task, so the identity-keyed [`HELD_SLOTS`] exemption above gives it an inert slot. Exactly one
    /// permit is held, not two — but one is enough.) The child, by contrast, is reached through
    /// `SpawnTaskSupervisor::spawn`, and `HELD_SLOTS` is a task-local that does not cross
    /// `tokio::spawn`, so the child cannot inherit that exemption: it queues behind the very call
    /// waiting for it. At a ceiling of 1 nothing runs; in general every delegated child stalls until
    /// the queue timeout refuses it.
    ///
    /// Marking the delegating op as non-occupying does not close this — and for a sharper reason than
    /// "the op set is open-ended": the permit is not held by `task` at all, it is held by
    /// `execute_batch`. Exempting `task` changes nothing. One would have to exempt every op that can
    /// transitively await a sub-agent (`execute_batch`, `explore`, `ai_segment`, `flow_run`, any
    /// authored model stage), which is the whole nested-program family and is open-ended: any future
    /// op that runs an authored flow can contain a `task`. That invariant is unenforceable, and a
    /// regression would surface only under saturation *and* delegation. A shared budget therefore
    /// needs a structural mechanism (ancestry-keyed permits, or releasing a slot across any nested
    /// dispatch), which is a design and not a wiring change.
    ///
    /// The honest consequence, which every doc site states: `max_concurrent_tool_calls = N` bounds
    /// **each agent** at N, so k live sub-agents may run up to N×(k+1) tool calls at once.
    ///
    /// ⚠ **C-444 bounds k rather than reopening this.** The reasoning above is unchanged and the
    /// execution semaphore is still per agent — but a per-agent ceiling whose total is unbounded was
    /// itself the finding, so [`max_live_agents`](Self::with_max_live_agents) bounds how many agents
    /// can be live at once, and *that* census is **shared** by this copy rather than duplicated. It
    /// can be shared precisely because it refuses instead of queueing, so it introduces none of the
    /// waiting this method exists to avoid. With both set the tree total is `N × max_live_agents` —
    /// finite, and stated.
    pub fn independent_copy(&self) -> Self {
        let mut copy = Self {
            max_concurrent_tool_calls: self.max_concurrent_tool_calls,
            queue_timeout: self.queue_timeout,
            max_retained_result_bytes: self.max_retained_result_bytes,
            max_evidence_payload_bytes: self.max_evidence_payload_bytes,
            max_live_agents: self.max_live_agents,
            slots: None,
            // Shared, NOT copied: this is the ceiling on the whole delegated tree, so a child that
            // got its own census would defeat the very multiplication it exists to bound.
            agents: self.agents.clone(),
        };
        if let Some(n) = self.max_concurrent_tool_calls {
            // A fresh semaphore of the same size: same ceiling, separate budget.
            copy.slots = Some(Arc::new(Semaphore::new(n)));
        }
        copy
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

/// The tree-wide live-agent census behind [`ResourceLimits::with_max_live_agents`] (C-444).
///
/// Shared across the `task` boundary — including through
/// [`ResourceLimits::independent_copy`] — because bounding the *tree* is the whole point. A plain
/// counter rather than a [`Semaphore`] for a load-bearing reason: it must never make a caller wait.
/// The execution semaphore cannot be shared across delegation because an ancestor holds a permit for
/// its child's whole turn, so a waiting child waits on itself; a census that refuses on the spot has
/// no such cycle to enter.
#[derive(Debug)]
pub(crate) struct AgentCensus {
    /// The ceiling, counting the root agent. Children admitted = `limit - 1` at most.
    limit: usize,
    /// Agents currently live, root included — hence the census starts at 1.
    live: std::sync::atomic::AtomicUsize,
}

impl AgentCensus {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            // The root agent occupies the first place: `max_live_agents = 1` therefore means "no
            // delegation", not "one child".
            live: std::sync::atomic::AtomicUsize::new(1),
        }
    }

    /// Admit one agent, or refuse. A compare-and-swap loop rather than a bare `fetch_add`, so a
    /// refused spawn never transiently overshoots the ceiling it is being refused by (two concurrent
    /// `task` calls at the boundary would otherwise both add, then both roll back — and a census
    /// reader in between would see the ceiling breached).
    fn admit(self: Arc<Self>) -> std::result::Result<AgentSlot, AgentCensusRefusal> {
        let mut live = self.live.load(std::sync::atomic::Ordering::SeqCst);
        loop {
            if live >= self.limit {
                return Err(AgentCensusRefusal { limit: self.limit });
            }
            match self.live.compare_exchange_weak(
                live,
                live + 1,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(AgentSlot { census: self }),
                Err(observed) => live = observed,
            }
        }
    }
}

/// One live sub-agent's place in the tree-wide census (C-444). Dropping it frees the place, so a
/// child that finishes — or panics — never leaks its slot.
#[derive(Debug)]
pub struct AgentSlot {
    census: Arc<AgentCensus>,
}

impl Drop for AgentSlot {
    fn drop(&mut self) {
        self.census
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A sub-agent spawn refused because the tree-wide live-agent ceiling was already met (C-444).
///
/// Transient in the same sense as [`ConcurrencyRefusal`]: the same delegation may succeed once a
/// sibling finishes. It is *not* an authorization denial — nothing was forbidden, the tree is simply
/// at its budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCensusRefusal {
    /// The tree-wide ceiling that bound this spawn, counting the root agent.
    pub limit: usize,
}

impl AgentCensusRefusal {
    /// The refusal the caller sees: what bound it, and what to do about it.
    pub fn message(&self) -> String {
        format!(
            "sub-agent refused: the runtime's tree-wide ceiling of {} live agent(s) — the root plus \
             every delegated child — is already met. Wait for a delegated child to finish, delegate \
             less at once, or raise `max_live_agents`.",
            self.limit
        )
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
        assert_eq!(limits.max_evidence_payload_bytes(), None);
        assert_eq!(
            limits.tool_call_queue_timeout(),
            DEFAULT_TOOL_CALL_QUEUE_TIMEOUT
        );
    }

    /// C-298's ceiling counts as a configured ceiling on its own — `is_unbounded` must not report a
    /// runtime as unbounded just because the other two knobs are unset.
    #[test]
    fn an_evidence_ceiling_alone_makes_a_runtime_bounded() {
        let limits = ResourceLimits::new().with_max_evidence_payload_bytes(64 * 1024);
        assert!(!limits.is_unbounded());
        assert_eq!(limits.max_evidence_payload_bytes(), Some(64 * 1024));
        assert_eq!(limits.max_concurrent_tool_calls(), None);
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
            max_evidence_payload_bytes: Some(262_144),
            ..Default::default()
        });
        assert_eq!(limits.max_concurrent_tool_calls(), Some(4));
        assert_eq!(
            limits.tool_call_queue_timeout(),
            Duration::from_millis(2_500)
        );
        assert_eq!(limits.max_retained_result_bytes(), Some(1_048_576));
        assert_eq!(limits.max_evidence_payload_bytes(), Some(262_144));

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

    /// C-299: the shape a sub-agent inherits. `independent_copy` keeps every configured value but
    /// mints a **fresh** semaphore, so a child never queues behind its parent — that is what makes
    /// descending the ceiling deadlock-free, given that the agent-loop op driving the delegation
    /// (`execute_batch`) holds a permit for the child's whole turn.
    #[tokio::test]
    async fn an_independent_copy_keeps_the_ceiling_but_not_the_budget() {
        let parent = ResourceLimits::new()
            .with_max_concurrent_tool_calls(1)
            .with_tool_call_queue_timeout(Duration::from_millis(20))
            .with_max_retained_result_bytes(4_096)
            .with_max_evidence_payload_bytes(2_048);
        let child = parent.independent_copy();

        // Same ceilings, value for value.
        assert_eq!(child.max_concurrent_tool_calls(), Some(1));
        assert_eq!(child.tool_call_queue_timeout(), Duration::from_millis(20));
        assert_eq!(child.max_retained_result_bytes(), Some(4_096));
        assert_eq!(child.max_evidence_payload_bytes(), Some(2_048));

        // Different budget: the parent saturated must not refuse the child.
        let _parent_held = parent.acquire_execution_slot().await.expect("parent slot");
        assert!(
            parent.acquire_execution_slot().await.is_err(),
            "the parent's own ceiling must still bind for itself"
        );
        assert!(
            child.acquire_execution_slot().await.is_ok(),
            "a child must not queue behind the parent that is waiting for it — that is the deadlock"
        );
    }

    /// And an independent copy of an unbounded parent stays unbounded rather than inventing a
    /// ceiling of one.
    #[tokio::test]
    async fn an_independent_copy_of_an_unbounded_runtime_is_unbounded() {
        let child = ResourceLimits::new().independent_copy();
        assert!(child.is_unbounded());
        assert_eq!(child.max_concurrent_tool_calls(), None);
        assert!(child.acquire_execution_slot().await.is_ok());
        assert!(child.acquire_execution_slot().await.is_ok());
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

    // -- C-444: the tree-wide agent census -------------------------------------------------------

    /// **The finding, stated as arithmetic.** A per-agent concurrency ceiling with no bound on the
    /// agent count has an unbounded total; bounding the agent count makes the total finite. This is
    /// what `max_live_agents` buys, and it is the half `independent_copy` deliberately cannot.
    #[test]
    fn the_agent_ceiling_is_what_makes_the_tree_total_finite() {
        let unbounded_tree = ResourceLimits::new().with_max_concurrent_tool_calls(4);
        assert_eq!(
            unbounded_tree.max_live_agents(),
            None,
            "a concurrency ceiling alone leaves the agent count — and so the tree total — unbounded"
        );

        let bounded_tree = ResourceLimits::new()
            .with_max_concurrent_tool_calls(4)
            .with_max_live_agents(3);
        assert_eq!(bounded_tree.max_live_agents(), Some(3));
        // 4 per agent × 3 live agents = 12 simultaneous tool calls, tree-wide. Finite, and stated.
        assert_eq!(bounded_tree.max_concurrent_tool_calls(), Some(4));
    }

    /// The census is **shared** across `independent_copy`, unlike the execution semaphore. That is
    /// the whole mechanism: a child that got its own census would restore the multiplication.
    #[test]
    fn a_delegated_child_shares_the_agent_census_but_not_the_execution_budget() {
        let parent = ResourceLimits::new()
            .with_max_concurrent_tool_calls(2)
            .with_max_live_agents(2);
        let child = parent.independent_copy();

        assert_eq!(child.max_live_agents(), Some(2), "the numbers descend");
        // Separate execution budgets (the C-299 deadlock constraint) …
        let parent_slots = parent.slots.as_ref().expect("parent semaphore");
        let child_slots = child.slots.as_ref().expect("child semaphore");
        assert!(
            !Arc::ptr_eq(parent_slots, child_slots),
            "the execution semaphore must stay per agent — sharing it across the `task` boundary \
             deadlocks (see `independent_copy`)"
        );
        // … but ONE census, so the tree cannot multiply past the ceiling.
        let parent_census = parent.agents.as_ref().expect("parent census");
        let child_census = child.agents.as_ref().expect("child census");
        assert!(
            Arc::ptr_eq(parent_census, child_census),
            "the agent census must be SHARED across delegation — a per-child census would defeat \
             the tree-wide bound it exists to enforce"
        );
    }

    /// The ceiling counts the root, admits up to `limit - 1` children, and **refuses** rather than
    /// queueing. Refusing is what makes a shared census deadlock-free.
    #[test]
    fn the_census_admits_up_to_the_ceiling_then_refuses() {
        let limits = ResourceLimits::new().with_max_live_agents(3);
        // The root occupies one place, so two children fit.
        let first = limits.admit_agent().expect("first child admitted");
        let second = limits.admit_agent().expect("second child admitted");
        let refused = limits
            .admit_agent()
            .expect_err("the third child exceeds a ceiling of 3 counting the root");
        assert_eq!(refused.limit, 3);
        assert!(
            refused.message().contains("max_live_agents"),
            "the refusal must name the knob to raise, got: {}",
            refused.message()
        );

        // A place frees when a child's turn ends, so the same delegation succeeds later: transient,
        // not an authorization denial.
        drop(second);
        let third = limits.admit_agent().expect("a freed place is reusable");
        drop((first, third));
    }

    /// `max_live_agents = 1` means "no delegation": the root itself is an agent.
    #[test]
    fn a_ceiling_of_one_admits_no_children_at_all() {
        let limits = ResourceLimits::new().with_max_live_agents(1);
        assert!(
            limits.admit_agent().is_err(),
            "a ceiling of 1 is the root alone — every `task` must be refused"
        );
        // `0` is meaningless as a ceiling and reads as 1 rather than refusing the root's existence.
        assert_eq!(
            ResourceLimits::new()
                .with_max_live_agents(0)
                .max_live_agents(),
            Some(1)
        );
    }

    /// The unbounded default is untouched: configuring nothing admits every agent, as before C-444.
    #[test]
    fn no_agent_ceiling_admits_every_agent() {
        let limits = ResourceLimits::new();
        for _ in 0..64 {
            assert!(
                limits.admit_agent().expect("unbounded admits").is_none(),
                "an unconfigured census must hand back no slot at all, not a bounded one"
            );
        }
        assert!(limits.is_unbounded());
    }

    /// The autonomous preset is genuinely bounded on every axis — including the tree — because
    /// "unattended *and* unbounded" is the configuration C-444 exists to remove.
    #[test]
    fn the_autonomous_preset_bounds_the_tree_as_well_as_the_agent() {
        let limits = ResourceLimits::autonomous();
        assert!(!limits.is_unbounded());
        assert!(limits.max_concurrent_tool_calls().is_some());
        assert!(
            limits.max_live_agents().is_some(),
            "an autonomous posture must bound the agent count too — a per-agent ceiling alone \
             multiplies across a delegated tree, which is finding F4"
        );
        assert!(limits.max_retained_result_bytes().is_some());
        assert!(limits.max_evidence_payload_bytes().is_some());
    }
}
