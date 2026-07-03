---
id: L-26
title: "Optimizer must see reads inside object/list call args — fix batch/CSE soundness"
pillar: Language
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "collect_var_reads hand-rolls recursion and drops Obj/List/Fmt/Expr under a `_ => {}` — a symbol read inside a named-arg object is invisible to the RAW/WAR/WAW check, so a reader parallelizes with its writer and CSE reuses stale values; silent wrong output on the canonical named-arg form"
---

# Optimizer must see reads inside object/list call args — fix batch/CSE soundness

## Goal
Close a soundness hole in the CSE/batch optimizer. `collect_var_reads` hand-rolls its own recursion and only
descends into `Var`/`Lit`/`Call`/`Jq`/`Parse`, swallowing `Obj`/`List`/`Fmt`/`Expr` under a `_ => {}`
(`crates/flux-lang/src/optimize.rs:180`) — contradicting the module's own soundness claim (`:17`). Liveness
(`collect_reads_deep`) and plan-risk (`walk_node`) both route through the exhaustive `for_each_node`; only
this collector doesn't. It drives both `Batch::independent` (parallelization) and `cse_aliases`
(invalidation).

## Acceptance
- [ ] Failing-first test `batch_split_sees_object_arg_reads` using the canonical named-arg form:
      `$dir = glob("src/**")` then `grep({pattern:"TODO", path:$dir})` must **not** be placed in one
      `Stage::Parallel` (today it is → grep resolves `$dir` unbound / stale).
- [ ] Failing-first test `cse_invalidated_by_object_arg_rebind`: an intervening `$dir` rebind between two
      `grep({path:$dir})` calls prevents aliasing the second to the first (today it aliases → stale result).
- [ ] Fix: replace the hand-rolled match with `for_each_node`, or add the `Obj`/`List`/`Fmt`/`Expr` descent arms.
- [ ] A property test asserting the read-set soundness invariant (every `Var` read anywhere in the call args is
      collected).

## Progress
- 2026-07-03 DONE — `collect_var_reads` now routes through the exhaustive `for_each_node` (shared `collect_leaf_read` helper), so reads inside object/list/fmt/expr call args are seen by batch-split + CSE. Tests: `batch_split_sees_object_arg_reads`, `cse_invalidated_by_object_arg_rebind`, `collect_var_reads_sees_every_var_in_nested_args` (property). Full gate green.

## Notes
- Evidence: `crates/flux-lang/src/optimize.rs:180` (the `_ => {}`), `:17` (soundness claim), `:405`
  (existing test uses a direct var arg, misses the object-arg form); production path
  `crates/flux-sdk/src/flow.rs:415`.
- Residual of the CSE/dead-step optimizer (flux-lang Tier-2). Design: [library-hardening](../designs/library-hardening.md).
