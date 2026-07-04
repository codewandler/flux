---
id: D-53
title: events.db plan_source exporter — flux-native corpus mining (the L-38 hedge cash-out)
pillar: Core
status: ready
priority: 3
epic: flux-planner-ship
design: docs/designs/flux-planner-ship.md
note: "every accepted plan since v0.2.15 carries parseable plan_source; pairing it with its originating user turn yields zero-LLM-cost NL→flux corpus rows that compound with real flux usage"
---

# events.db plan_source exporter

## Goal
`flux corpus export` (or a flux-model pipeline stage — placement decided in design
review): walk `~/.flux/events.db`, pair each accepted `PlanAttempted.plan_source`
(L-38, present since v0.2.15) with the user turn that produced it, and emit corpus-shaped
JSONL rows (`{id, nl_goal, source, provenance{session, turn}, flux_rev}`) compatible with
flux-model's validation ladder (`flux-corpus check` re-validates at export time — a
plan_source from an older flux_rev may no longer lower).

## Why
The Claude Code episode pool is nearly drained (107 eligible episodes left of 3,527 —
measured 2026-07-05) and costs ~$0.07/sample to distill. flux-native rows cost zero LLM
calls, are ALREADY canonical text (no generation step), and compound with real usage —
the long-term corpus supply for the local planner.

## Acceptance
- [ ] Exporter reads events.db read-only (sqlite_query-grade safety), pairs plan_source
      with the originating user instruction, skips rows where plan_source is None
      (pre-L-38, oversized) or the pairing is ambiguous — precision over recall.
- [ ] Every exported row re-validates through the flux-model ladder (lower_ok + cycle_ok
      at CURRENT flux HEAD) before corpus entry; stale-rev rows are dropped and counted.
- [ ] C-22 redaction guarantee restated at the export boundary: plan_source is already
      redacted at record time; nl_goal (the user turn) gets the same secret-scrub pass
      capture.py applies.
- [ ] Failing-first test: a seeded events.db with two accepted plans (one pre-L-38 row
      without plan_source) exports exactly one valid corpus row.

## Notes
- Design decision to settle first: flux CLI subcommand vs flux-model pipeline stage
  reading the db directly. Leaning CLI (`flux corpus export`) so the schema knowledge
  stays in flux; flux-model consumes plain JSONL either way.
