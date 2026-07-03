---
id: C-31
title: "Failed planner turns must record their token usage — compile_turn's Err path drops accumulated Usage"
pillar: Core
status: done
epic: parse-resilience
design: docs/designs/parse-resilience.md
note: "s_360's failed turn made ~8 provider calls (~37k input each) yet persisted NO call_usage event — flux usage undercounts exactly the turns that waste the most tokens"
---

# Planner usage survives failure

## Goal
`compile_turn` accumulates `Usage` across every provider call of the planner consultation (up to
`max_steps` of them) but returns it only in the `Ok` tuple — the final `Err` drops it on the floor.
Confirmed in s_360: the failed turn (8 planner calls) persisted no `call_usage` event, while the
next successful turn did. Cost accounting must be failure-independent: return the accumulated
usage alongside the error and have the engine's plan step record `call_usage` for failed
consultations too, so `flux usage` (and the C-06/C-15 cost rollups) count them.

## Acceptance
- [x] Failing-first test: a planner consultation that exhausts its step budget surfaces the same
      accumulated usage a successful one would (per-call token sums included), and the engine
      records a `call_usage` event for the failed turn. Today the usage is lost.
      → compile level: `failed_consultation_still_returns_accumulated_usage`; loop level:
      `failed_plan_consultation_still_records_its_usage` (turn tally + per-call record; the
      engine's `record_call_usage_events` already runs on every termination path and skips
      `total()==0`, so the events follow).
- [x] The turn still fails with the same error text (C-17/F2 semantics unchanged) — only the
      accounting side-channel is added (asserted in both tests).
- [x] All `compile_turn`/`compile_turn_with_arm` callers compile against the adjusted
      `(Result<TurnOutput>, Usage)` shape: the loop-host plan op accounts usage BEFORE branching
      on the outcome; `compile_once` and one-shot `plan()` document the deliberate no-ledger drop;
      REPL `/plan` keeps its display-only semantics; the emission A/B harness now reports real
      spend on failed tasks instead of zeros.
- [x] `MaxTokens`-truncation and neither-plan-nor-answer error returns inside the loop carry their
      usage the same way — usage is a `&mut` out-parameter of the inner loop, so EVERY exit
      (including `?` on a provider error) preserves it by construction.

## Progress
- 2026-07-03 filed from the s_360 diagnosis (missing call_usage event verified against events.db;
  the successful follow-up turn in the same session recorded one).
- 2026-07-03 **done**: `compile_turn`/`compile_turn_with_arm` → `(Result<TurnOutput>, Usage)` with
  the loop body as an inner fn writing usage through an out-param; loop-host accounts before
  branching; failing-first tests at both levels; full gate green.

## Notes
- Site: `crates/flux-flow/src/compile.rs` (`compile_turn_with_arm` return shape) + the plan-step
  call site that persists `call_usage`.
- Shape decision in-story: `(Result<TurnOutput>, Usage)` vs an error type carrying usage — pick
  whichever keeps callers type-driven and mechanical.
- Epic: [parse-resilience](../designs/parse-resilience.md).
