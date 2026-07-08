---
id: L-56
title: Automatic context slicing for planner and model ops
pillar: Language
status: backlog
epic: flux-lang-agent-speed
design: docs/designs/flux-lang-agent-speed.md
note: "KF4: derive the minimum model-visible context from HIR symbol reads, op schemas, and policy-visible evidence boundaries"
---

# Automatic context slicing for planner and model ops

## Goal
Reduce planner and model-op tokens by sending only the symbols, fields, evidence windows,
and diagnostics needed for the next decision, derived from the analyzed plan and operation
schemas rather than handwritten prompt trimming.

## Acceptance
- [ ] Context slicing computes per-call dependencies from HIR symbol reads, field access
      paths, operation schemas, and planner repair diagnostics.
- [ ] Model ops and planner feedback receive the sliced context by default, with an audit
      record of which symbols/evidence were included and why.
- [ ] Token budgets are enforced before dispatch using exact host-provided counts when
      available and a deterministic fallback when not.
- [ ] Private, hidden, secret-derived, or policy-denied symbols are never included unless
      explicitly referenced and permitted for that model-visible boundary.
- [ ] Tests cover sliced model-op input, deterministic budget trimming, excluded private
      evidence, and equivalence for a flow whose full context would exceed the budget.

## Progress
- (implementation not yet started)

## Notes
- Epic: [flux-lang-agent-speed](../designs/flux-lang-agent-speed.md).
- This should compose with existing `context` projection work in `flux-runtime`; do not add
  a model-visible bypass around redaction or policy.
