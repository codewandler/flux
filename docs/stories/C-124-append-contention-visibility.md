---
id: C-124
title: Surface event-store append contention before it becomes a 5s failure
pillar: Core
status: done
epic: event-store-concurrent-use
design: docs/designs/event-store-concurrent-use.md
note: "an append that waited on the SQLite busy handler > ~1s should leave a visible trace; today the only signal is the terminal busy error after the full 5s budget"
---

# Surface event-store append contention before it becomes a 5s failure

## Goal
The SQLite backend's 5s `busy_timeout` (C-25, `sqlite.rs:261`) makes contended writers wait
silently — a deployment drifting toward the wrong topology (R1 in the design doc) gets zero
warning until a writer starves past 5s and the turn aborts. Emit a counter/log line when an
append waited on the busy handler longer than ~1s, so operators see contention while it is still
harmless.

## Acceptance
- [ ] An append that blocks on the write lock beyond a threshold (~1s, constant or config) leaves
      a visible trace (log line and/or counter) naming the wait duration — proven by a
      failing-first test that holds the write lock from a second connection and asserts the trace.
- [ ] Uncontended appends emit nothing new (no per-append noise).
- [ ] No new side-channel state in the store (R7) — measurement only, no second write path.

## Progress
- (not started)

### 2026-07-28 — implementation pass

Implemented per the design doc's named seam: `begin_write` (`crates/flux-events/src/store/sqlite.rs`)
now times its own `BEGIN IMMEDIATE` acquisition — that blocking call *is* the busy-handler wait, so
no second write path or new store state was needed (R7 clean by construction).

**`crates/flux-events/src/store/sqlite.rs`:**
- Added `CONTENTION_WARN_THRESHOLD = Duration::from_secs(1)` (the "~1s, constant" option from
  Acceptance).
- `begin_write` now takes a `warn_after: Duration`, times the transaction-acquisition call with
  `Instant`, and emits `tracing::warn!(waited_ms, threshold_ms, "...")` when the wait meets or
  exceeds it. All 7 call sites (`append_with_ts`, `create_session_with_context`,
  `prune_empty_excluding`, `prune_inactive_excluding`, `prune_older_than`,
  `prune_adhoc_older_than`, `copy_session_atomic`) pass `self.contention_warn_threshold` — every
  write transaction shares one seam, not just `append`, since they all funnel through the same
  `BEGIN IMMEDIATE`.
- `SqliteEvents` gained a `contention_warn_threshold: Duration` field (always
  `CONTENTION_WARN_THRESHOLD` via `open`/`in_memory`). A `#[cfg(test)]`-only
  `open_with_contention_threshold(path, threshold)` constructor lets a test override it — this is
  the "hookable" seam requested: it keeps the acceptance's real second-connection lock-hold
  mechanism (not a synthetic delay) while letting the test use a low threshold (20ms) and a short,
  fixed lock hold (150ms) instead of sleeping past the real 1s production default. Deterministic
  (fixed sleep, not a race) and fast (whole crate suite: 0.35s).

**`crates/flux-events/src/store/mod.rs`** (`store::tests::sqlite_tests`):
- New test `append_that_waits_on_the_busy_handler_leaves_a_warn_trace`: sibling of
  `concurrent_writers_wait_on_busy_timeout_instead_of_erroring`, same "second raw connection holds
  the write lock, releases after a fixed delay" shape, opened via
  `open_with_contention_threshold(path, 20ms)` + a 150ms hold. Installs a minimal recording
  `tracing::Subscriber` (mirrors `flux_runtime::metadata`'s test-only `RecordingSubscriber` — no
  `tracing-subscriber` dep pulled in) via `tracing::subscriber::with_default`, and asserts a
  captured message mentions the wait.
  - **Failing-first proof:** temporarily short-circuited the `if waited >= warn_after` gate to
    `if false && …`, reran this test alone — failed with `expected a trace ... got: []`. Reverted
    (`if waited >= warn_after`) and reran green. No other test's behavior changed by the revert.
- New test `uncontended_append_emits_no_contention_warning`: an in-memory store (production
  threshold, no contention) appends once under the same recording subscriber and asserts the
  captured list is empty — covers Acceptance's second bullet.

**Reconciliation notes:**
- Scoped to SQLite only, matching the design doc's epic breakdown (C-124's description names only
  "the busy handler," an SQLite-specific term; Postgres's `pg_advisory_xact_lock` has a different
  blocking model — unbounded, not a 5s ceiling — and the design doc doesn't ask for a signal there).
  Built and tested the crate under `--features postgres` too; unaffected (107 tests green).
- **Known gap, not fixed here:** no product surface (`flux` binary, `flux-server`) installs a
  `tracing` subscriber anywhere in this codebase today (verified: no `tracing_subscriber`/
  `set_global_default` in the tree) — `flux_provider::retry`'s own doc-comment says the same thing
  about its pre-C-181 warnings ("no product surface installs a subscriber"). So this `tracing::warn!`
  is visible to an embedder/operator who wires up a subscriber (or exports via OpenTelemetry, C-129
  adjacent) but does not yet reach CLI stderr or the TUI. This matches the Acceptance's literal
  wording ("log line and/or counter") and the design doc's scoped deliverable; promoting it to an
  interactive/CLI-visible signal (the treatment C-180..C-182 gave retry visibility, because that was
  a per-turn UX concern) is out of scope for this story — surfaced here rather than silently decided.
- No public API changed: `EventStore::open`/`in_memory` signatures are untouched; `SqliteEvents` and
  `begin_write` are crate-private, and the new constructor is `#[cfg(test)]`-gated. Added one new
  Cargo dependency (`tracing.workspace = true`) to `codewandler-flux-events` — already a workspace
  dependency used elsewhere, no version change.

**Gate (crate-scoped, per orchestrator instruction — full workspace gate not run):**
- `cargo build -p codewandler-flux-events` — clean.
- `cargo test -p codewandler-flux-events` — 69 unit + 1 doctest, all green, 0.35s.
- `cargo test -p codewandler-flux-events --features postgres` — 107 unit + 1 doctest, all green
  (`TEST_POSTGRES_URL` unset → PG cases skip with a notice, as designed).
- `cargo clippy -p codewandler-flux-events --all-targets -- -D warnings` — clean (default and
  `--features postgres`).
- `cargo fmt -p codewandler-flux-events` — applied (two minor diffs from the new test code),
  `--check` now clean.

## Notes
- Implementation seam: time the `BEGIN IMMEDIATE` acquisition in `begin_write`
  (`crates/flux-events/src/store/sqlite.rs:36`).
- Sibling test to extend: `concurrent_writers_wait_on_busy_timeout_instead_of_erroring`
  (`crates/flux-events/src/store/mod.rs:2487`).
