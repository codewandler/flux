---
id: C-125
title: Multi-process event-store stress test — N writer processes × M streams, zero lost writes
pillar: Core
status: done
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
- 2026-07-29: Implemented. Added `crates/flux-events/tests/multiprocess_stress.rs` — an
  integration-test file whose own compiled test binary IS the worker (test-binary re-exec, no
  separate helper crate/binary). Each test detects its role via env vars: the orchestrator
  (`cargo test` invocation, no env vars) spawns `std::env::current_exe()` filtered to
  `--exact <own test name>` with `FLUX_EVENTS_C125_DB`/`_STREAMS`/`_COUNT`/`_STABLE_ID` set; the
  re-exec'd child sees the env vars, appends, and returns without spawning further.
  - `multi_process_writers_produce_contiguous_gapless_streams`: 4 worker **processes** × 25
    appends each, round-robined across 2 shared streams of ONE file `events.db`. After joining,
    asserts every stream's `stream_seq` set is exactly `0..=head` (contiguous, no gaps/dupes) and
    the total appended across all streams equals `WORKERS * PER_WORKER` (100).
  - `multi_process_idempotent_append_stores_exactly_once`: 3 worker processes race the exact same
    `NewEvent.id` into the same stream. Asserts exactly one stored event for that id and that
    `head_seq` is 1, not 3 — proving the losers re-read the winner (C-87) rather than each minting
    their own `stream_seq`, across a real process boundary.
  - Both stay well inside the normal `cargo test` budget (~0.03s wall total for the pair, plus
    process-spawn overhead); no `#[ignore]` gate needed.
- Failing-first proof: temporarily reverted `begin_write`'s `TransactionBehavior::Immediate` to
  `Deferred` (undoing C-25's fix) — `multi_process_writers_produce_contiguous_gapless_streams`
  then reliably fails (a worker process exits non-zero on `SQLITE_BUSY`, panicking the
  orchestrator's `assert!(status.success())`) across 3 repeated runs. Confirms the test actually
  exercises the real OS-level cross-process locking path (not green-by-construction). Reverted
  before landing; `git diff` against HEAD showed no residual change.
- Gate (crate-scoped): `cargo test -p codewandler-flux-events` — 77 lib + 2 (this file) + 1
  doctest, all green. `cargo clippy -p codewandler-flux-events --all-targets -- -D warnings` —
  clean. `cargo fmt -p codewandler-flux-events` — clean.
- Acceptance: all three boxes met (≥3 processes / contiguous `stream_seq` / total = sum of
  appends; idempotent stable-id variant across processes; hermetic tempdir, no network, inside
  the normal budget — no `#[ignore]` needed).

## Notes
- Pattern precedent for PG: `concurrent_cold_boots_serialize_bootstrap_ddl` /
  `concurrent_appends_to_one_stream_are_contiguous` (`store/mod.rs:2605,2662`) — this story is the
  SQLite/process-level sibling.
- Process spawning inside flux-events tests may use `std::process::Command` directly — the
  guarded-IO rule covers tools/runtime, not test harnesses; match existing test conventions.
