---
id: L-24
title: Reified-await ledger fold — loop-side checkpoint∘await (keep the prefix across an await)
pillar: Language
status: backlog
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: today a reified await inside a loop plan abandons all post-await work; with the ledger, the post-await re-emission fast-forwards the completed prefix — the loop-side case of the deferred checkpoint∘await composition
---

# Reified-await ledger fold

## Goal
When a loop plan hits a top-level `await`, it is reified (not a turn suspension) and today the
completed prefix is lost to the follow-up plan. Fold awaits into the halt-latch machinery: append
`PlanHalted{kind:"awaiting"}` with the same statement ledger, so when the model re-emits the plan
with the awaited information incorporated, the completed prefix fast-forwards instead of re-running.

## Acceptance
- [ ] A reified await in resumable mode records the halt latch + ledger; the follow-up re-emission
      fast-forwards the matching prefix. Failing-first:
      `post_await_reemission_keeps_completed_prefix`.
- [ ] The engine's pre-authored await-suspension path (suspensions table) is untouched.
- [ ] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic; post-MVP.)

## Notes
- Depends on L-22 + A-16. The cross-suspension-latch composition for authored flows stays deferred
  (evolution-impl-plan "Deferred (v1)").
- Current reified-await behavior pinned at `await_inside_a_plan_is_reified_not_a_turn_suspension`
  (`crates/flux-flow/src/engine.rs:1449-1491`).
