---
id: C-23
title: "Stop double-counting sub-agent spend in all-sessions cost/efficiency rollups"
pillar: Core
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "the parent records a synthetic CallUsage for the child's total AND the child bills on its own stream in the shared audit store; cost_summary_all/efficiency_all fold over every stream and sum both, so flux usage 'All sessions' reports each task sub-agent's tokens twice"
---

# Stop double-counting sub-agent spend in all-sessions cost/efficiency rollups

## Goal
Make the default-on `flux usage` totals correct when sub-agents run. The parent turn records a synthetic
`CallUsage` for each sub-agent's total on the **parent** stream (`crates/flux-flow/src/engine.rs:664`),
while the child runs a full turn on **its own stream in the same shared audit store**
(`crates/flux-orchestrate/src/lib.rs:349`, store shared at `:297`). `cost_summary_all`
(`crates/flux-events/src/store.rs:735`) and `efficiency_all` (`:773`) fold over **every** stream and sum —
so the child's tokens appear in both the parent's rollup row and the child's own stream, and the extra
synthetic call inflates calls/turn and the cache-read share. A-08 avoided double-*persisting* the trail but
not double-*counting* the usage.

## Acceptance
- [ ] Failing-first test `cost_summary_all_does_not_double_count_correlated_children`: a parent turn +
      correlated child that bills N tokens reports N once in the all-sessions rollup (today 2N), and
      calls/turn excludes the synthetic parent-side call for cost purposes.
- [ ] One authoritative source is chosen: either exclude correlated child streams from the all-streams folds
      (streams whose `correlation_id` points at a parent in the set), or stop emitting the parent-side rollup
      `CallUsage` for cost — not both.
- [ ] Per-session (single-stream) reporting is unchanged.
- [ ] Design note records which source is authoritative and why.

## Progress
- 2026-07-03 DONE — authoritative source = exclude correlated child streams from the all-streams folds (new `aggregate_streams`); `cost_summary_all`/`efficiency_all` no longer double-count sub-agent spend; per-session reporting unchanged. Test: `cost_summary_all_does_not_double_count_correlated_children`. Full gate green.

## Notes
- Evidence: `crates/flux-flow/src/engine.rs:664`; `crates/flux-orchestrate/src/lib.rs:349`,`:297`;
  `crates/flux-events/src/store.rs:735`,`:773`.
- Residual of [A-08](A-08-subagent-audit-default.md) / [C-06](C-06-usage-cost-accounting.md).
  Design: [library-hardening](../designs/library-hardening.md).
