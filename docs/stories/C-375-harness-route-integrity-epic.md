---
id: C-375
title: "Harness route integrity — the requested route must be runnable, and completion must prove it ran (epic)"
pillar: Agent
status: backlog
epic: harness-route-integrity
design: docs/designs/harness-route-integrity.md
note: "EPIC — the two 2026-07-31 harness reviews produced ZERO board items; C-255 was scoped to the security reviews only. flow_run has no path parameter, routes to the family described as 'pure deterministic', has no preflight, and nothing binds completion to a route"
---

# Harness route integrity

## Goal

Make a route-specific request executable, checkable before mutation, and provable afterwards —
inside the staged narrowing design, not by widening the surfaced catalog.

## Acceptance

- [x] C-376 lets `flow_run` address a workspace flow path and return a route receipt.
- [ ] C-377 routes the flow ops into a family whose description is true of them.
- [ ] C-378 adds an exact-flow preflight that separates inspectable from executable.
- [ ] C-379 binds completion to the route the user required.
- [ ] C-380 executes `examples/commit.flux` in a hermetic end-to-end test.
- [ ] C-381 measures first-pass routing and gives first-party families routing hints.
- [ ] No story in this epic proposes an ambient all-tools catalog; H explicitly rejects that reading
      and nothing found during validation challenges progressive narrowing itself.

## Progress

- 2026-08-01 — opened from validation of HAR-01/02/03/05 and ROUTE-01.
- 2026-08-02 — C-376 done: the model-facing runner can now execute a freshly read, confined
  workspace `.flux` path and returns the route identity needed by C-379.

## Notes

- Scope honesty: HAR-01/HAR-02 are runtime claims about a live staged turn. Validation established
  that the binding mechanisms they say are missing **are** missing, not that the specific reported
  session behaved as described — no authoritative transcript reader exists to check (that is C-387).
