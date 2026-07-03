---
id: A-26
title: "Measure the per-turn token budget against cumulative billed tokens"
pillar: Agent
status: done
priority: 6
epic: review-hardening
design: docs/designs/review-hardening.md
note: "the A-10 turn budget compares against replace-style usage (only outputs sum; input/cache are overwritten each call), so `used` tracks last-call context occupancy, not the turn's cumulative billed tokens — a runaway 20-call loop re-paying ~90k input each time never trips the ceiling it exists to enforce"
---

# Measure the per-turn token budget against cumulative billed tokens

## Goal
Make the A-10 per-turn token budget actually bound a runaway turn. The gate computes
`let used = self.usage.lock().unwrap().total();` (`crates/flux-flow/src/loop_host.rs:437`), but
`Usage::accumulate` sums only output/reasoning and **replaces** the input/cache fields each call
(`flux-core` stream.rs), so `total()` measures the last call's context occupancy plus summed outputs —
not the turn's cumulative billed tokens. A stuck loop making 20+ planner calls, each re-paying ~90k input
(millions billed overall), keeps `used` near ~90k + outputs and never trips `--turn-budget` /
`FLUX_TURN_TOKEN_BUDGET`. The budget silently fails to bound exactly the runaway cost it exists to cap.

## Acceptance
- [x] Failing-first test: with a small turn budget and a stubbed multi-call loop that re-pays input each
      round, the budget trips once cumulative billed tokens exceed the ceiling. Today it never trips.
- [x] Fix: sum against a cumulative-billed total. The field-wise-summed `turn_calls` data needed for a true
      per-turn total already exists beside `self.usage` — use it rather than the replace-style snapshot.
- [x] No behavioural change when the budget is unset or when a single call stays under it.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded 🟠 (a cost-control mechanism that silently no-ops on the
  case it targets). Verified: `accumulate` is replace-style on input/cache, sum-only on outputs.
- 2026-07-03 fixed: `crates/flux-flow/src/loop_host.rs` — added `EngineLoopHost::cumulative_billed_tokens`
  (sums each per-call `Usage::total()` out of the existing `calls` record) and switched the `--turn-budget`
  gate in `plan()` to read it instead of `self.usage.lock().unwrap().total()`'s replace-style snapshot.
  New failing-first tests (both now pass): `loop_host::tests::token_budget_trips_on_cumulative_billed_tokens_across_calls`
  (two 110-token rounds against a 200 budget trip the THIRD `plan()` call before it reaches the provider —
  reproduced the bug pre-fix: the old replace-style total stayed at ~120 and let the third call through) and
  `loop_host::tests::token_budget_unset_or_single_call_under_it_is_unaffected` (no-regression: unset budget
  and a single under-budget call behave exactly as before). Gate green: `cargo test -p flux-flow` (183
  passed), `cargo clippy -p flux-flow --all-targets -- -D warnings` (clean), `cargo fmt -p flux-flow`.

## Notes
- Evidence: `crates/flux-flow/src/loop_host.rs:437`; `flux-core` `Usage::accumulate` / `total()` (input/cache
  replaced, outputs summed); the sibling `turn_calls` field carries the field-summed data.
- Residual of [A-10](A-10-turn-token-budget.md). Design: [review-hardening](../designs/review-hardening.md).
