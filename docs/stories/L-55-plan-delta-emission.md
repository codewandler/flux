---
id: L-55
title: Plan-delta emission for cheap safe repairs
pillar: Language
status: ready
priority: 11
epic: flux-lang-agent-speed
design: docs/designs/flux-lang-agent-speed.md
note: "KF3: let repair turns patch the previous AST, then materialize and validate the full plan before execution"
---

# Plan-delta emission for cheap safe repairs

## Goal
Let planner repair rounds emit a small patch against the previous Flux-Lang AST instead of
re-emitting the entire plan, while keeping execution behind the same full-AST analyzer,
policy, and audit gates.

## Acceptance
- [ ] A versioned plan-delta representation can replace, insert, delete, or edit nodes by
      stable path or node id without executing any partial plan.
- [ ] The engine materializes the delta into a complete `DraftAst`, normalizes it through
      the same model-ingress rules as full emissions, and runs the existing analyzer before
      any operation dispatch.
- [ ] Failed or malformed deltas produce repair feedback and do not mutate the accepted
      previous plan.
- [ ] Audit/state stores enough source material to reconstruct both the delta and the
      materialized full plan.
- [ ] Tests cover a successful one-node repair, malformed delta rejection, stale-base
      rejection, and an attempted hidden/denied op that remains gated after patching.

## Progress
- (implementation not yet started)

## Notes
- Epic: [flux-lang-agent-speed](../designs/flux-lang-agent-speed.md).
- This is an emission optimization, not a new execution semantics. The runtime still sees
  a complete analyzed plan.
