---
id: D-151
title: Session observability — history/turns/run_trace/cost/efficiency + pricing feature
pillar: Agent
status: done
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
- [x] Failing-first: after two turns, `cost(&table)` returns a `ModelCost` row with non-zero USD
      for a priced mock model; `turns()` has two summaries; `history()` alternates
      user/assistant.
- [x] `pricing` feature: `cargo build -p codewandler-flux-sdk` (no features) has no
      flux-credentials in `cargo tree`; with the feature, `flux_sdk::pricing::load_pricing_table`
      resolves.
- [x] Projection types (`Message`, `TurnSummary`, `RunEvent`, `ModelCost`,
      `EfficiencySummary`) nameable via `flux_sdk::observe`.

## Progress
- **Done (unreleased).** `Session` gained `turns()`/`run_trace()`/`cost(&PricingTable)`/`efficiency()`
  (`crates/flux-sdk/src/session.rs`) — pure reads over `engine.events` projections (`history()` was
  wave 1). `flux_sdk::observe` re-exports `TurnSummary`/`RunEvent`/`ModelCost`/`EfficiencySummary`
  (`Message` already there); `PricingTable` re-exported at crate root.
- Opt-in `pricing` cargo feature (`default = []`, `pricing = ["dep:flux-credentials"]`) adds
  `flux_sdk::pricing::load_pricing_table` (built-in rates + `~/.flux/pricing.toml` overlay). Default
  build pulls no `flux-credentials` (verified via `cargo tree`: 0 without, 1 with). `Session::cost`
  takes any `PricingTable`, so `PricingTable::builtin()` works featureless.
- Failing-first tests: `session_projections_report_turns_history_and_cost` (two priced turns →
  `turns()` len 2, `history()` user/assistant×2, `cost()` non-zero USD via a `PricingTable` with a
  `set("priced-mock", …)` rate; `run_trace`/`efficiency` smoke) and a `#[cfg(feature = "pricing")]`
  `pricing_feature_exposes_the_loader`.
- CHANGELOG + WHATS-NEW updated + website mirror. Gate green (workspace test 2160; SDK
  `--features pricing` 43 lib; clippy incl. pricing feature / fmt / codegate). **Not committed/released.**
  Wave 2 of the sdk-surface epic (D-147..D-151) is now complete.

## Notes
- Pure reads over `engine.events` (pub); projections at
  `crates/flux-events/src/store/mod.rs:614-770`. Depends on D-142.
