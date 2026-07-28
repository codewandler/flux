---
id: C-125
title: Multi-process event-store stress test — N writer processes × M streams, zero lost writes
pillar: Core
status: backlog
epic: event-store-concurrent-use
design: docs/designs/event-store-concurrent-use.md
note: "the cross-process claim is currently proven by a two-connection in-process test; a spawned-subprocess stress proves the real OS-level file-locking story"
---

# Multi-process event-store stress test — N writer processes × M streams, zero lost writes

## Goal
`concurrent_writers_wait_on_busy_timeout_instead_of_erroring` (`store/mod.rs:2487`) opens two
connections in one process — real deployments (serve daemon + CLI turn, C-25's scenario) contend
across *processes*, where OS file locking, WAL shared memory, and the busy handler interact for
real. Ship a spawned-subprocess stress test hammering N writer processes × M streams on one file
store, asserting contiguous `stream_seq` per stream and zero lost or duplicated writes.

## Acceptance
- [ ] A test spawns ≥ 3 child processes (test-binary re-exec or a small helper), each appending a
      known count of events across shared streams of one `events.db`; after joining, every stream's
      `stream_seq` is contiguous from 1 and the total event count equals the sum of appends.
- [ ] A variant exercises the idempotent stable-id path across processes (same `NewEvent.id` from
      two processes → exactly one stored event).
- [ ] The test is hermetic (tempdir store, no network) and stays inside the normal
      `cargo test --workspace` budget, or is `#[ignore]`-gated with the gate documented.

## Progress
- (not started)

## Notes
- Pattern precedent for PG: `concurrent_cold_boots_serialize_bootstrap_ddl` /
  `concurrent_appends_to_one_stream_are_contiguous` (`store/mod.rs:2605,2662`) — this story is the
  SQLite/process-level sibling.
- Process spawning inside flux-events tests may use `std::process::Command` directly — the
  guarded-IO rule covers tools/runtime, not test harnesses; match existing test conventions.
