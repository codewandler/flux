---
id: A-27
title: "Route the identical-plan skip transcript through the stall guard"
pillar: Agent
status: done
priority: 7
epic: review-hardening
design: docs/designs/review-hardening.md
note: "the A-05 identical-plan skip returns its informational transcript directly, bypassing guard_transcript, so the transcript-stall counter/force-stop never advances — a model re-emitting the byte-identical succeeded plan spins the full 25-round repeat budget (a planner call per round) instead of force-stopping"
---

# Route the identical-plan skip transcript through the stall guard

## Goal
Make the A-05 identical-plan skip honor the stall-guard contract its own comment promises. The skip path
returns its transcript directly — `return Ok(serde_json::json!({ "transcript": "[loop-guard] This EXACT
plan already ran SUCCESSFULLY …" }))` (`crates/flux-flow/src/loop_host.rs:665-674`) — without calling
`self.guard_transcript(...)`, unlike the plan-error path (`:757`). So the transcript-stall counter never
advances on repeated skips and `force_stop` never arms: a model that keeps re-emitting the byte-identical
already-succeeded plan (the exact silent-success confusion A-05 targets) spins through the full flux-lang
repeat budget (up to 25 planner rounds), paying a planner model call each round, instead of ending after
`STALL_STOP` rounds.

## Acceptance
- [x] Failing-first test: a loop that re-emits the identical succeeded plan every round escalates the
      transcript-stall counter and force-stops after `STALL_STOP` rounds. Today it runs to the repeat budget.
- [x] Fix: pass the skip transcript through `guard_transcript(...)` like the other return paths.
- [x] A single skip (non-repeated) still returns its informational transcript with no premature stop.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded 🟠 (wasted spend + long dead turn). Verified the skip
  path returns before the guard while the plan-error path at `:757` routes through it.
- 2026-07-03 fixed: `crates/flux-flow/src/loop_host.rs` — the identical-plan skip in `run_plan_dispatch`
  now routes its transcript through `self.guard_transcript(...)` (extracted the fixed message into the new
  `IDENTICAL_PLAN_SKIP_MESSAGE` constant, reused by the fix and the test) instead of returning it directly.
  New failing-first test (now passes): `loop_host::tests::identical_plan_skip_escalates_and_force_stops_on_repeats`
  — re-emits the identical succeeded plan repeatedly and asserts the transcript-stall counter escalates and
  `force_stop` arms exactly at `STALL_STOP`, with the underlying op dispatched only once across every round
  (pre-fix, `force_stop` never armed and the op transcript stayed the raw skip message forever). The
  existing `run_plan_skips_an_identical_plan_after_success` test continues to pass unmodified, confirming a
  single (non-repeated) skip still returns its informational transcript untouched with no premature stop.
  Gate green: `cargo test -p flux-flow` (183 passed), `cargo clippy -p flux-flow --all-targets -- -D
  warnings` (clean), `cargo fmt -p flux-flow`.

## Notes
- Evidence: `crates/flux-flow/src/loop_host.rs:665-674` (skip returns pre-guard) vs `:757` (error path guarded).
- Residual of [A-05](A-05-legible-silent-success-feedback.md) / [A-20](A-20-stall-guard-resource-aware.md).
  Design: [review-hardening](../designs/review-hardening.md).
