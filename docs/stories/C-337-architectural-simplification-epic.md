---
id: C-337
title: "Architectural simplification — fewer assembly paths, smaller modules, less compatibility debt (epic)"
pillar: Core
status: backlog
epic: architectural-simplification
design: docs/designs/architectural-simplification.md
note: "EPIC — simplify inside the existing L0–L6 boundaries: one safe execution-environment assembly path, expired compatibility APIs removed, subsystem-sized files split into modules, and crate ownership re-audited"
---

# Architectural simplification — fewer assembly paths, smaller modules, less compatibility debt (epic)

## Goal

Reduce the codebase's local complexity without weakening the architecture that already works. Keep
the L0–L6 dependency rule and the authorization → approval → guarded-IO envelope intact, while
removing parallel compatibility paths, making safety-critical environment assembly harder to omit,
and splitting subsystem-sized implementation files into reviewable internal modules.

## Acceptance

- [x] A design doc ([architectural-simplification.md](../designs/architectural-simplification.md))
      records the evidence, ordering, boundaries, and seven workstreams from the architecture review.
- [ ] The epic is broken into bounded implementation stories before code changes begin; every
      behavioral or public-API change names a failing-first test, and mechanical module moves preserve
      public re-exports and pass the standing gate.
- [ ] `ExecutionEnvironment` has one explicit assembly path whose mandatory authorization, approver,
      registry, permissions, and guarded system inputs cannot be omitted; resource limits and other
      surface-wide invariants are applied once and observed by wiring tests.
- [ ] Compatibility APIs already marked for removal are retired in a planned minor release, including
      the obsolete agent/app assembly doors and lenient role-loading fallbacks; production code no
      longer needs their `#[allow(deprecated)]` bridges.
- [ ] The largest implementation units are split into cohesive internal modules without adding crates
      or changing the L0–L6 dependency graph; `flux-runtime` and `flux-codegate` lead the sequence.
- [ ] The current 37-crate workspace has a recorded ownership/consumer audit. Every leaf is either
      consumed, explicitly justified as an independently useful published/optional boundary, or
      removed/folded within its layer; published and deliberate L0 boundaries are not merged merely
      to reduce the count.
- [ ] `AgentSpec` gains a migration design for grouped settings and construction without public-field
      sprawl, scheduled only for a deliberate breaking API window.
- [ ] Completed roadmap history is archived behind stable links, leaving `docs/roadmap.md` focused on
      current status, active epics, and pending decisions.

## Progress

- 2026-07-31 — epic and design filed from a repository-wide architecture review. No implementation
  stories or code changes have started.

## Notes

- Preserve, do not redesign, the central safety architecture: every real effect still traverses
  `Executor::dispatch`, and crate dependencies still point only to the same or a lower layer.
- Highest-value sequence: typed environment assembly → expired compatibility cleanup →
  `flux-runtime`/`flux-codegate` module splits → remaining large-file splits → crate audit →
  `AgentSpec` breaking-window design → roadmap archive.
- Design: [architectural-simplification.md](../designs/architectural-simplification.md).
