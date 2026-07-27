---
id: D-181
title: Scope the resurrect ledger and crash tail to the interrupted turn, not the session
pillar: Agent
status: ready
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
priority: 1
note: "review finding (2026-07-28): session-wide fold can fast-forward on a PREVIOUS turn's progress or serve a previous turn's cells"
---

# Scope the resurrect ledger and crash tail to the interrupted turn, not the session

## Goal
Make Resurrect's exactly-once claim hold per *turn*. Today `resurrect`/`interrupted` fold the
whole-session trace: the `ResumeLedger` gathers `StatementCompleted` rows by content-addressed
`flow_key` only (`ResumeLedger::from_interrupted`, `crates/flux-lang/src/runtime.rs`), and
`crash_tail_cells` (`crates/flux-flow/src/resurrect.rs`) takes every `OpRecorded` after the last
statement boundary anywhere in the session. Events already carry `turn_id` and the store already has
`load_turn`, so turn-scoping was available and should be used.

Confirmed failure scenarios (2026-07-28 review):
1. **Identical-plan restart with zero progress**: turn 1 runs plan P to completion; turn 2
   re-accepts the identical plan (same `flow_key`) and crashes before its first
   `StatementCompleted`. The ledger returns turn 1's full completed set; resurrect fast-forwards
   everything, executes none of turn 2's side effects, and closes the turn `"resurrected"` with
   `diverged: None`. (The existing test `from_interrupted_keeps_only_the_last_execution_on_restart`
   covers only restarts *with* progress.)
2. **Mixed sessions**: a completed natively-dispatched turn records cells but no statement boundary,
   so its cells land in the crashed turn's `ResumeTape` and can be *served* — a matching
   `(op, input_hash)` gets a previous turn's output and its real side effect never fires. The
   leftover-cell latch fires only after the turn already closed on the wrong service.
3. **Purely native sessions**: no statement boundary at all → empty crash tail → every completed op
   of the crashed turn re-fires live. Exactly-once silently degrades to at-least-once for the whole
   turn on precisely the sessions the SDK produces.

## Acceptance
- [ ] The resume ledger and the crash-tail cell slice are built from the interrupted **turn's**
      events only (`turn_id`-scoped), not the whole session trace.
- [ ] Failing-first test for scenario 1: identical plan re-accepted in a later turn, crash before
      any progress → resurrect re-runs the turn from statement 0 (nothing fast-forwarded from the
      earlier turn).
- [ ] Failing-first test for scenario 2: authored turn → completed native turn → crashed authored
      turn; the native turn's cells are never served into the resurrected turn.
- [ ] Scenario 3 documented honestly: state what a purely native crashed turn can and cannot
      resurrect, and make `interrupted()`/`ResurrectReport` say so instead of silently re-firing.
- [ ] The related content-vs-turn keying in `rerun_pinned`'s turn filter and `replay_turns_prefix`
      (`crates/flux-flow/src/whatif.rs` — `flow_key` set membership lets a repeated identical plan
      leak the target turn's execution into the prefix) is fixed with the same turn attribution, or
      split into its own follow-up with a test pinning the leak.

## Progress
- (not started)

## Notes
- Same root cause across all findings: content-addressed plan identity (`flow_key`) with no turn
  attribution. Prefer threading `turn_id` through the fold over inventing a second identity.
- Keep the honest at-least-once window for an op interrupted mid-dispatch (no cell) — that part of
  the design is deliberate and documented; this story is only about cross-turn contamination.
