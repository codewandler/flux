---
id: C-87
title: Event-store & shared-engine growth/correctness (prune, idempotency, projections, eviction)
pillar: Core
status: done
priority: 15
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "Growth/correctness (Medium) — durable facts pruned, idempotency race, unbounded projections, per-turn evidence clone"
---

# Event-store & shared-engine growth/correctness

## Goal
A cluster of unbounded-growth and correctness gaps that bite most on a long-lived, shared (A2A) engine:
`prune_empty` deletes sessions with no `Message` but real durable facts (run-trace, observations,
`CallUsage`, `Custom`); the caller-id idempotency check is a check-then-insert race that returns an
append error under multiple writers instead of a no-op; projections (`conversation`/`turns`/
`cost_summary`) materialize + JSON-decode the entire stream unbounded; `Executor::evidence()` deep-clones
the never-trimmed evidence log ~4×/turn under a mutex; and `sticky_groups` / composite `State.sessions`
are per-session maps that are never evicted.

## Acceptance
- [x] `prune_empty` gates on `last_seq <= 0`, not `msg_count == 0` (sqlite + postgres); test
      `prune_empty_keeps_sessions_with_durable_nonmessage_facts` (a session with an observation but no
      message survives).
- [x] Caller-id append catches the `UNIQUE(id)` violation and returns the existing event (no-op idempotent
      retry) instead of erroring — both backends.
- [x] `conversation()` (`kind IN ('message','compacted')`) and `observations()` (`kind = 'observation'`)
      use `load_by_kind` (served by `idx_events_stream_kind`); test
      `projections_read_only_their_kinds_from_a_mixed_stream`.
- [x] Evidence via a non-cloning `evidence_kind_count` accessor; composite/session maps evicted via
      `clear_session` (tests `clear_turn_bounds_session_caches_to_active`, `clear_session_drops_the_entry`).

## Progress
- **2026-07-15 — DONE (full workspace gate green).** Store layer: `prune_empty` last_seq gate,
  UNIQUE-violation idempotent no-op, and kind-filtered `conversation`/`observations`/`run_trace`
  projections. Shared-engine layer (flux-flow): non-cloning `evidence_kind_count` and per-session
  cache eviction via `clear_session`. The two new store tests were wired into the sqlite/pg backend
  test-runners this pass. Verified by those tests + the full suite.

## Notes
- `crates/flux-events/src/store/sqlite.rs:478` (+`postgres.rs:492`), `postgres.rs:578` (+`sqlite.rs:593`),
  `crates/flux-events/src/store/mod.rs:556`; `crates/flux-flow/src/engine.rs:395,133`, `composites.rs:65`.
- Design: [harness-hardening](../designs/harness-hardening.md).
