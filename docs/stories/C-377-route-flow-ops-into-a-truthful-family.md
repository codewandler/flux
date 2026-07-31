---
id: C-377
title: Route the flow ops into a capability family whose description is true of them
pillar: Agent
status: backlog
epic: harness-route-integrity
design: docs/designs/harness-route-integrity.md
note: "flow_list/flow_render route to workspace.read; flow_run has empty effect+access sets so virtual_family drops it into `core` — 'Pure and generally useful deterministic operations' — for a Risk::Medium NonIdempotent flow runner"
---

# Route the flow ops into a capability family whose description is true of them

## Goal

Stop the routing layer from advertising the flow runner under a description that says it does not
act, and stop discovery and execution from living in different families.

## Acceptance

- [ ] A `flows` group carries `flow_list`, `flow_run` and `flow_render` with a description that
      names executing stored flows, and `surface_when` matchers so an indirect request routes to it.
- [ ] `virtual_family` (`crates/flux-flow/src/staged.rs:1540-1563`) cannot classify a `Risk::Medium`
      non-idempotent op as `core`; a spec with empty effect and access sets but a non-trivial risk
      tier gets a different fallback or is rejected at catalog-coherence time.
- [ ] Failing-first: a test over the production catalog asserting all three flow ops land in one
      family, and that no `Risk::Medium`-or-higher op is classified `core`.

## Progress

- 2026-08-01 — filed from validation of HAR-01. This is the structural hazard behind the reported
  symptom, independent of what the reviewed session actually surfaced.

## Notes

- Ungrouped ops are always advertised (`crates/flux-runtime/src/lib.rs:2047-2057`), so `flow_run` is
  *available*; the defect is which family it is presented under and with what description.
