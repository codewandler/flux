---
id: D-151
title: Session observability — history/turns/run_trace/cost/efficiency + pricing feature
pillar: Agent
status: ready
priority: 10
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 2 — the recorded-but-invisible projections reach the embedder"
---

# Session observability — projections + the pricing feature

## Goal
`Session` exposes the EventStore projections that are already recorded for every turn:
`history()`, `turns()`, `run_trace()`, `cost(&PricingTable)`, `efficiency()`; a `pricing` cargo
feature (dep: flux-credentials) adds `load_pricing_table()` for `Session::cost` ergonomics.

## Acceptance
- [ ] Failing-first: after two turns, `cost(&table)` returns a `ModelCost` row with non-zero USD
      for a priced mock model; `turns()` has two summaries; `history()` alternates
      user/assistant.
- [ ] `pricing` feature: `cargo build -p codewandler-flux-sdk` (no features) has no
      flux-credentials in `cargo tree`; with the feature, `flux_sdk::pricing::load_pricing_table`
      resolves.
- [ ] Projection types (`Message`, `TurnSummary`, `RunEvent`, `ModelCost`,
      `EfficiencySummary`) nameable via `flux_sdk::observe`.

## Progress
- (pending)

## Notes
- Pure reads over `engine.events` (pub); projections at
  `crates/flux-events/src/store/mod.rs:614-770`. Depends on D-142.
