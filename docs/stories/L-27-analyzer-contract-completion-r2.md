---
id: L-27
title: "Analyzer contract completion R2 — route/verify eval_arg positions + expr formula"
pillar: Language
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "L-21 closed each/jq/parse but the route selector and verify expect are still unguarded eval_arg positions, and Node::Expr's formula string is never validated against its own vars map — each is a plan shape the analyzer accepts and the runtime rejects, costing a repair round"
---

# Analyzer contract completion R2 — route/verify eval_arg positions + expr formula

## Goal
Finish the L-16/L-21 analyzer-contract work: three more positions where the analyzer accepts what the runtime
rejects, each costing a wasted compile-repair round on a plan the analyzer could reject up front with a
bind-it-first hint.

1. **`route` selector** — a non-`call` selector is resolved via `eval_arg`
   (`crates/flux-lang/src/runtime.rs:2469`, which accepts only lit/var/obj/list) but the analyzer only
   `check_node`s it (`analyze.rs:1333`) with no `check_eval_arg_position`.
2. **`verify` `expect`** — same shape (`runtime.rs:2370` vs `analyze.rs:1270`).
3. **`expr` formula** — `Node::Expr`'s analyzer arm only checks the `vars` sub-values (`analyze.rs:1272`);
   it never tokenizes the `formula` string nor checks that every identifier it references is a key of `vars`.
   The runtime tokenizes/evaluates it and errors with "invalid expr formula" (`runtime.rs:3435`). Unlike the
   deferred `{{sym}}`-in-`Fmt` case, `vars` is an explicit unambiguous scope → checkable with zero false
   positives.

## Acceptance
- [ ] Failing-first analyzer tests: `route jq(".intent") { … }` and `verify { expect: fmt("{x}") }` each
      produce a diagnostic (with a bind-it-first hint), mirroring L-21's each/jq/parse guards.
- [ ] Failing-first analyzer test: `expr("count > threshold", vars: {count: $n})` (formula ident `threshold`
      absent from `vars`) produces a diagnostic; a malformed formula produces a parse diagnostic.
- [ ] Fix: `check_eval_arg_position` on the route selector (non-call arm) and verify `expect`; tokenize the
      `expr` formula (reuse the runtime tokenizer) and diagnose idents not in `vars`.

## Progress
- 2026-07-03 DONE — `route` selector + `verify` `expect` now `check_eval_arg_position`; `Node::Expr` formula validated against its `vars` via new `validate_expr_formula`. Tests: `non_call_route_selector_is_a_diagnostic`, `non_eval_arg_verify_expect_is_a_diagnostic`, `expr_formula_ident_absent_from_vars_is_a_diagnostic`, `malformed_expr_formula_is_a_parse_diagnostic` (+ no-false-positive passes). Full gate green.

## Notes
- Evidence: `crates/flux-lang/src/runtime.rs:2469`,`:2370`,`:3078`,`:3435`;
  `crates/flux-lang/src/analyze.rs:1333`,`:1270`,`:1272`.
- Folds two audit findings. Residual of [L-16](L-16-analyzer-contract-completion.md) /
  [L-21](L-21-flux-lang-v1-residual-burndown.md). Design: [library-hardening](../designs/library-hardening.md).
