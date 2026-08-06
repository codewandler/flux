---
id: C-542
title: "One time/token budget vocabulary with hard limits and live projections"
pillar: Core
status: ready
priority: 30
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-lang, flux-flow, flux-tui, flux-cli]
note: "foundation for C-571 — budget target versus hard limit, common envelope/usage events and live TUI projection"
---

# Use one budget vocabulary from runtime to screen

## Goal

Define one typed target-versus-hard-limit vocabulary for wall-clock time, model calls and token spend,
with attributed usage events and live projections that C-571 can allocate hierarchically across a
Fleet.

## Acceptance

- [x] A typed budget envelope distinguishes a soft target from a hard limit and carries wall time,
      model calls plus input/output/total tokens. Usage events name run, agent/session, turn and
      loop-segment attribution without double-counting child rollups.
- [x] A run and at least one child/segment scope accept the envelope. Hitting a hard limit stops at a
      documented safe boundary with a typed scope/dimension/spent/limit result; a failing-first test
      proves the stop and an in-flight effect is never reported stopped when it is still finishing.
- [x] Budget consumption (spent vs declared, for both time and tokens) is visible in the TUI while
      the run executes, updating as spend accrues; a failing-first test proves the TUI surface
      renders the budget state.
- [x] Exceeding a target without a hard limit warns visibly but does not stop execution; the
      distinction and one-warning behavior are tested.
- [x] The JSON/event contract is the single source for CLI/TUI projections and for C-571's durable
      Fleet reservation/settlement ledger; no surface recalculates totals independently.
- [ ] The gate is green in both workspaces.

## Progress

- 2026-08-05 — vocabulary landed in `crates/flux-core/src/budget.rs`: `BudgetEnvelope` (soft target
  beside hard limit) over `BudgetDimension` (wall time, model calls, input/output/total tokens),
  `BudgetUsageEvent` with run/session/turn/segment `BudgetAttribution`, and `BudgetLedger` as the
  only accountant — a duplicate `event_id`, a pre-summed child `rollup` and an ancestor's event are
  each ignored with a distinct `BudgetCharge` reason. `crates/flux-core/tests/budget_envelope.rs`
  pins target-vs-limit, one-warning, charge-once, elapsed-not-summed wall time and the JSON contract.
- 2026-08-05 — enforcement: `EngineLoopHost` (`crates/flux-flow/src/loop_host.rs`) owns one ledger,
  charges each model call and wall-clock sample exactly once, and publishes the projection as the
  `budget.projection` observation (`flux_evidence::KIND_BUDGET_PROJECTION`). `--turn-budget` is now
  expressed as a turn-scope hard limit in that vocabulary. `ensure_stage_budget`
  (`crates/flux-flow/src/staged.rs`) refuses the next round at the stage's safe boundary *after* the
  finished round is charged, so an in-flight call is never reported stopped; a target never stops a
  stage.
- 2026-08-05 — surfaces: the TUI header carries a live `budget Σ1.6k/4.0k tok` segment with
  `over target` / `limit` states plus transcript notes, and `CliSink`
  (`crates/flux-cli/src/rendering.rs`) projects the same published payload as a yellow target warning
  or a red hard-limit stop line. Both read the ledger's own figures; neither re-derives a total.
- Residual: the full repository gate (and the untouched `plugins/` workspace) runs at the wave
  integration boundary, not from this story worktree — the last acceptance box is the integrator's.

## Notes

- 2026-08-05 — reconciled with C-568/C-571. This story owns the shared local vocabulary and live
  projection; C-571 owns cross-worker reservation/settlement and C-130 adds currency/rolling quotas.
