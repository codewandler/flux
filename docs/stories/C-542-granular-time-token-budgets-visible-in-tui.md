---
id: C-542
title: "Granular time and token budgets with limits, visible in the TUI during execution"
pillar: Core
status: ready
priority: 30
epic:
design:
areas: [flux-lang, flux-flow, flux-tui, flux-cli]
note: "budget = declared spend target, limit = hard stop; both per run and per granular unit, surfaced live in the TUI"
---

# Granular time and token budgets with limits, visible in the TUI during execution

## Goal

An execution can declare granular budgets — wall-clock time and token spend — with hard limits at
both the whole-run and per-unit level (agent, step, or tool dispatch), and the TUI shows live
consumption against those budgets while the run executes, so an operator sees overrun risk before
the limit trips rather than after.

## Acceptance

- [ ] A run accepts a declared time budget and token budget with an optional hard limit for each;
      granularity covers at least the whole run and one sub-unit level (per agent or per step).
      *(First pass — confirm the exact granularity levels.)*
- [ ] Hitting a hard limit stops the affected scope cleanly: in-flight work is terminated at a safe
      boundary, the result names which budget tripped, and a failing-first test proves the stop.
- [ ] Budget consumption (spent vs declared, for both time and tokens) is visible in the TUI while
      the run executes, updating as spend accrues; a failing-first test proves the TUI surface
      renders the budget state.
- [ ] Exceeding a *budget* without a *limit* warns visibly but does not stop execution; the
      distinction is tested.
- [ ] The gate is green in both workspaces.

## Progress

- (not started)

## Notes

- Filed 2026-08-05 via /track:story from the roadmap coordinator session; Goal/Acceptance are a
  first-pass draft from the title — refine granularity levels and the TUI presentation before
  dispatch.
- Likely touchpoints: dispatch/budget accounting in flux-lang/flux-flow, TUI rendering beside the
  usage observatory (see C-531's tool-sink pairing for the event-attribution precedent).
