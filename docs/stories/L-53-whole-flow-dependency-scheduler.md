---
id: L-53
title: Whole-flow dependency scheduler for nested read parallelism
pillar: Language
status: ready
priority: 9
epic: flux-lang-agent-speed
design: docs/designs/flux-lang-agent-speed.md
note: "KF1: extend optimization from local read batching to a whole-HIR symbol DAG with effect and approval fences"
---

# Whole-flow dependency scheduler for nested read parallelism

## Goal
Make Flux-Lang automatically run independent read-only work in parallel across the whole
plan, including nested blocks and templates, while preserving the same values, trace
shape, approval ordering, and policy behavior as sequential execution.

## Acceptance
- [ ] Failing-first test: independent read-only calls hidden inside nested object/list
      templates, `when` conditions, and child blocks are scheduled into parallel stages.
- [ ] Failing-first test: a write, model call, approval-required op, or unknown-effect op
      creates a hard fence and prevents speculation across it.
- [ ] The scheduler builds a symbol dependency graph from the analyzed HIR instead of
      relying on top-level statement shape.
- [ ] `execute_plan` and the optimized execution path produce the same bound values and
      user-visible trace ordering for the same flow.
- [ ] Docs and CHANGELOG explain which operations can be parallelized and which fences
      force sequential execution.

## Progress
- (implementation not yet started)

## Notes
- Epic: [flux-lang-agent-speed](../designs/flux-lang-agent-speed.md).
- Keep this below the existing Flux-Lang analyzer/runtime boundary; the optimizer may
  reorder only when effect metadata and symbol dependencies prove it safe.
