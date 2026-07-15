---
id: L-81
title: Depth-guard the flux-lang parsers, expr evaluator, and composite calls
pillar: Language
status: done
priority: 4
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "DoS (High) — unbounded recursion → uncatchable SIGABRT from .flux source AND LLM-emitted plans"
---

# Depth-guard the flux-lang parsers, expr evaluator, and composite calls

## Goal
Stop untrusted input from aborting the process. The hand-rolled recursive-descent parser, the `expr`
formula evaluator, and recursive composite ops have no depth limit and no `catch_unwind`; nested
`(`/`!`/`List<…>` or `op f(){ call f() }` overflow the stack → `SIGABRT` (uncatchable), breaking the
modules' documented "never panics" contract. Reproduced empirically (60–80k tokens). Reachable from any
`.flux` source *and* from LLM plans (an `expr` formula is an opaque string that bypasses serde's depth cap).

## Acceptance
- [x] Failing-first tests: deeply-nested formula / input / composite recursion returns a bounded
      error instead of aborting — `deeply_nested_formula_is_bounded_not_aborting`,
      `deeply_nested_input_is_bounded_not_aborting`, `recursive_composite_op_is_depth_bounded_not_aborting`.
- [x] Shared recursion-depth guard (RAII `enter`/`leave` tracker) threaded through the `expr_*` chain,
      the CST parser (`expr`/`prefix`/`primary`/`obj_expr`/`list_expr`/`type_ref`), and the `cst_decode`
      setting parsers.
- [x] Call-depth ceiling enforced in `execute_composite_call`, wired to `CompositeLimits`.

## Progress
- **2026-07-15 — DONE (full workspace gate green: `cargo test`/`clippy -D warnings`/`fmt`).** Bounded
  depth guards added across `parser.rs`, `expr.rs`, `cst_decode.rs` and a call-depth ceiling in
  `runtime.rs::execute_composite_call`; deeply-nested input now returns a bounded error instead of a
  SIGABRT. Verified by the three named failing-first tests + the full suite.

## Notes
- `crates/flux-lang/src/expr.rs:510`; `parser.rs:1102,534,1226,1249`; `cst_decode.rs:1951`;
  `runtime.rs:434,3169,3187,3322,3516`; `program.rs:128` (`CompositeLimits`).
- Design: [harness-hardening](../designs/harness-hardening.md).
