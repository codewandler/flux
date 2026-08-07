---
id: C-663
title: "Workers show live token and tool-call activity as it happens"
pillar: "Core"
status: ready
priority: 10
areas: [flux-tui]
epic: fleet-harness-throughput
---

# Workers show live token and tool-call activity as it happens

## Goal

The Workers section shows lifecycle state but no live signal, so a working worker and a wedged one
look identical until the turn ends.

Half the data already exists. `.flux/fleet/activity.ndjson` carries `flux.fleet-activity/v1`
records — `tool_call`, `tool_result`, `approval`, `plan`, `turn_end`, each with agent, operation and
outcome — appended as workers stream. Nothing renders them.

Tokens are the missing half: the activity schema has no token fields. Spend is measured, in the
C-542 budget vocabulary (`BudgetUsageEvent`, the `budget.projection` observation), but it lives in
the agent's own session rather than in the fleet's activity stream.

## Acceptance

- [ ] Each worker row shows live tool-call activity — current operation and a running count —
      derived from the existing `flux.fleet-activity/v1` records.
- [ ] Token in/out per worker is surfaced, by forwarding the existing budget usage into the
      activity stream rather than recomputing spend anywhere.
- [ ] No surface recalculates totals; the ledger stays the only accountant, as C-542 requires.
- [ ] Updates are incremental — the pane tails the activity log rather than re-reading it.
- [ ] A worker with no recent activity is visibly distinct from one that is streaming, so a corpse
      is legible without `/proc`.
- [ ] Reads stay inside a stated byte budget, consistent with the bounded projections in C-562.
