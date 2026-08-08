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

- [ ] A typed budget envelope distinguishes a soft target from a hard limit and carries wall time,
      model calls plus input/output/total tokens. Usage events name run, agent/session, turn and
      loop-segment attribution without double-counting child rollups.
- [ ] A run and at least one child/segment scope accept the envelope. Hitting a hard limit stops at a
      documented safe boundary with a typed scope/dimension/spent/limit result; a failing-first test
      proves the stop and an in-flight effect is never reported stopped when it is still finishing.
- [ ] Budget consumption (spent vs declared, for both time and tokens) is visible in the TUI while
      the run executes, updating as spend accrues; a failing-first test proves the TUI surface
      renders the budget state.
- [ ] Exceeding a target without a hard limit warns visibly but does not stop execution; the
      distinction and one-warning behavior are tested.
- [ ] The JSON/event contract is the single source for CLI/TUI projections and for C-571's durable
      Fleet reservation/settlement ledger; no surface recalculates totals independently.
- [ ] The gate is green in both workspaces.

## Progress

- (not started)

## Notes

- 2026-08-05 — reconciled with C-568/C-571. This story owns the shared local vocabulary and live
  projection; C-571 owns cross-worker reservation/settlement and C-130 adds currency/rolling quotas.
