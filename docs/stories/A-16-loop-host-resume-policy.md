---
id: A-16
title: Loop-host resume policy + structured feedback contract (latch fold, denial guard, suffix-scoped approval)
pillar: Agent
status: ready
priority: 4
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: run_plan consumes the halt latch (fold over events.db, no new table), refuses hash-identical re-emission of denied statements, scopes approval to the suffix that will actually run, and feeds ✓/✗/· markers + machine-readable failure back to the planner
---

# Loop-host resume policy + feedback contract

## Goal
Wire L-22's primitive into the loop host (design: Part 2, `run_plan` order of operations): fold the
open halt latch from the event log, apply the denial re-emission guard, scope risk/approval to the
suffix that will actually run, execute resumable with the ledger, and return the structured
feedback contract (`failure{node, stmt, op, kind, fatal, message, completed[]}` + the ✓/✗/·-marked
transcript) so the model can repair surgically.

## Acceptance
- [ ] `FlowStore::open_halted_plan(session)` — fold over run-events (last `PlanHalted` with no later
      `PlanResumed`, plus that plan's `StatementCompleted` ledger), pattern of `once_lookup`
      (`crates/flux-flow/src/state.rs:83-149`); no new SQLite table. Cross-process by construction.
- [ ] `run_plan` halt arm: structured transcript (prefix outputs + `[plan halted at step N of M]` +
      rendered ✓/✗/· plan + kind-specific guidance, placed before the `cap_loop_feedback` tail) +
      machine-readable `failure` on the op Outcome. Failing-first:
      `run_plan_feeds_structured_halt_and_prefix_transcript`.
- [ ] Resume: `second_run_plan_consumes_latch_and_skips_completed_prefix` (latch is one-shot —
      `PlanResumed` appended even at zero skips: `unrelated_next_plan_consumes_latch_with_zero_skips`).
- [ ] Denial guard: `denied_statement_reemission_is_refused_not_redispatched` — a halt of kind
      denied/confirm_denied + a new plan containing the same `stmt_hash` returns an informational
      transcript without executing; a different approach flows normally.
- [ ] Approval scoping: `plan_risk_with_composites` sees only the to-run suffix (user not
      re-prompted for completed writes); the `flow.plan` observation carries ✓-done/•-to-run
      markers. Divergence before the failed index adds the "will RE-RUN (incl. side effects)"
      transcript warning.
- [ ] Guards: `LoopGuard` failure key `halt:{op}:{stmt}:{kind}` (same statement failing the same
      way escalates at existing STALL thresholds); silent-success guard untouched (byte-identical
      re-emission after a halt runs = retry of just the failed statement).
- [ ] Cross-turn: fresh-turn `plan()` injects one ephemeral `[resume context]` message when a latch
      is open.
- [ ] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic.)

## Notes
- Depends on L-22. The `Err` arm of `run_plan` (`loop_host.rs:742-757`) remains for infrastructure
  errors only.
- Open question parked for review here: should a High-risk prefix-edit re-run escalate to a confirm?
