---
id: C-25
title: "Set busy_timeout on events.db — survive cross-process SQLITE_BUSY"
pillar: Core
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "EventStore::open enables WAL but no busy_timeout/synchronous; the in-process mutex serializes one process but nothing coordinates a serve daemon + a CLI turn on the same ~/.flux/events.db — the second writer gets SQLITE_BUSY immediately, aborting the turn or losing telemetry"
---

# Set busy_timeout on events.db — survive cross-process SQLITE_BUSY

## Goal
Make the shared `events.db` safe under concurrent processes. `EventStore::open` enables WAL but sets neither
`busy_timeout` nor `synchronous` (`crates/flux-events/src/store.rs:163`). The in-process `Mutex` serializes
one process's writers, but nothing coordinates a `flux app run --serve` daemon plus a CLI turn on the same
`~/.flux/events.db` (`crates/flux-cli/src/main.rs:1069`). WAL permits a single writer; the second
`unchecked_transaction` returns `SQLITE_BUSY` immediately (no wait) — `record_message` `?`-propagates and
**aborts the turn** (`crates/flux-flow/src/engine.rs:175`), telemetry writes are swallowed and **lost**.

## Acceptance
- [ ] Failing-first test: two `EventStore` handles on the same file writing concurrently both succeed with a
      `busy_timeout` set (today the second errors `SQLITE_BUSY`).
- [ ] `EventStore::open` sets `conn.busy_timeout(~5s)` (and evaluates `synchronous=NORMAL` under WAL).
- [ ] No regression to single-process throughput.

## Progress
- 2026-07-03 DONE — `EventStore::open` sets `busy_timeout(5s)` + `synchronous=NORMAL`; writes upgraded to `BEGIN IMMEDIATE` (new `begin_write`) so the busy handler fires on the DEFERRED read→write lock upgrade. Test: `concurrent_writers_wait_on_busy_timeout_instead_of_erroring`. Full gate green.

## Notes
- Evidence: `crates/flux-events/src/store.rs:163` (open: WAL only); `crates/flux-flow/src/engine.rs:175`
  (record_message aborts the turn on error); `crates/flux-cli/src/main.rs:1069` (shared db path).
- Residual of the event-store unification. Pairs with [C-24](C-24-observation-flush-failure-watermark.md).
  Design: [library-hardening](../designs/library-hardening.md).
