---
id: C-26
title: "Give await/resume continuations real turn telemetry (not turn_id = -1)"
pillar: Core
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "resume_suspended never calls begin_turn/end_turn — it finishes with a hardcoded turn_id=-1, so observations flush unscoped and no TurnSummary/CallUsage is emitted; every A-11 reply-driven continuation (incl. its sub-agent spend) is invisible to turns()/efficiency/cost_summary"
---

# Give await/resume continuations real turn telemetry (not turn_id = -1)

## Goal
Make a resumed (reply-parked) continuation a first-class turn in the evidence projections. `resume_suspended`
never calls `begin_turn`/`end_turn`; every branch finishes with a hardcoded `turn_id = -1`
(`crates/flux-flow/src/engine.rs:719`,`:735`,`:745`). `finish_turn(-1)` → `flush_observations(session, -1)`
records observations **unscoped** (a turn is tagged only when `turn_id >= 0`,
`crates/flux-events/src/store.rs:633`), and no `TurnStarted`/`TurnEnded`/`CallUsage` is ever emitted. So an
A-11 reply-parking resume that runs real work — including `task` sub-agent spend — produces no `TurnSummary`
and can't be reassembled per-turn via `load_turn`.

## Acceptance
- [ ] Failing-first test: a suspended flow resumed on a correlated reply produces a `TurnSummary` (with any
      spend as `CallUsage`) and its observations are retrievable scoped to that turn (today none exist / they
      are unscoped under `-1`).
- [ ] `resume_suspended` wraps its work in a real `begin_turn`/`end_turn` with a proper turn id.
- [ ] Design note: turn identity for a continuation (new turn id vs. continuation of the suspended one).

## Progress
- 2026-07-03 DONE — `resume_suspended` wraps its work in a real `begin_turn`/`end_turn` (a new turn id) across all three exit paths, recording sub-agent `CallUsage` via `record_resume_usage`. Test: `resumed_flow_produces_turn_telemetry`. Full gate green.

## Notes
- Evidence: `crates/flux-flow/src/engine.rs:719`,`:735`,`:745`; `crates/flux-events/src/store.rs:633`.
- Residual of [A-11](A-11-journey-reply-parking.md) / [C-14](C-14-durable-evidence-trail.md).
  Design: [library-hardening](../designs/library-hardening.md).
