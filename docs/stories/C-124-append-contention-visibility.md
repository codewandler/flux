---
id: C-124
title: Surface event-store append contention before it becomes a 5s failure
pillar: Core
status: backlog
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

## Notes
- Implementation seam: time the `BEGIN IMMEDIATE` acquisition in `begin_write`
  (`crates/flux-events/src/store/sqlite.rs:36`).
- Sibling test to extend: `concurrent_writers_wait_on_busy_timeout_instead_of_erroring`
  (`crates/flux-events/src/store/mod.rs:2487`).
