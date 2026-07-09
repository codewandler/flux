---
id: L-53
title: Whole-flow dependency scheduler for nested read parallelism
pillar: Language
status: done
priority:
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
- [x] Failing-first test: independent read-only calls hidden inside nested object/list
      templates, `when` conditions, and child blocks are scheduled into parallel stages.
- [x] Failing-first test: a write, model call, approval-required op, or unknown-effect op
      creates a hard fence and prevents speculation across it.
- [x] The scheduler builds a symbol dependency graph from the analyzed HIR instead of
      relying on top-level statement shape.
- [x] `execute_plan` and the optimized execution path produce the same bound values and
      user-visible trace ordering for the same flow.
- [x] Docs and CHANGELOG explain which operations can be parallelized and which fences
      force sequential execution.

## Progress
- 2026-07-09 DONE. The scheduler now builds a whole-node symbol dependency graph: `summarize`
  walks each top-level statement's entire subtree (via the analyzer's exhaustive visitor) into
  (reads, writes, class) — class ReadOnly (all reachable ops registered + Read-only, no
  approval/durability construct), Barrier (nested `return`; execute_plan forbids return in a
  parallel stage), or Fenced (write/network/process effect, UNKNOWN op, or confirm/await/
  checkpoint/once/saga/thing). ReadOnly nodes are placed into dependency levels between fences
  (RAW/WAW/WAR hazard check vs every level) with an ORDER FLOOR (a node never lands below its
  predecessor's level), so the emitted schedule — and the replayed trace — stay in exact program
  order while independent whole nodes (a `when` block, a template bind, a plain read) share a
  parallel stage. Failing-first: 6 new optimizer tests (nested batching, pipelined levels,
  unknown-op fence, nested-write fence, confirm fence, nested-return exclusion; 4 failed on v1) +
  `scheduled_plan_matches_sequential_values_and_trace_order` in runtime.rs proving identical
  bound values AND identical user-visible op-event order vs `execute_flow`. All 19 pre-existing
  optimizer test shapes unchanged. Runtime untouched (`execute_plan` already runs arbitrary
  NodeId sets per parallel stage). Docs: execution-model page + module docs; CHANGELOG entry.
  Residual: intra-node stage plans (`BranchPlan`/`RepeatPlan` scaffolding) remain unemitted —
  nested bodies still execute via `exec_body` inside their (now parallelizable) whole node.

## Notes
- Epic: [flux-lang-agent-speed](../designs/flux-lang-agent-speed.md).
- Keep this below the existing Flux-Lang analyzer/runtime boundary; the optimizer may
  reorder only when effect metadata and symbol dependencies prove it safe.
