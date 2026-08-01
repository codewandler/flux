---
id: L-114
title: "Statement-block nesting joins the L-81 depth guard (parser SIGABRT)"
pillar: Language
status: ready
priority: 3
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang]
note: "Review F1, HIGH — ~200 nested when blocks (~9 KB on a 2 MiB stack) abort the process; the guard covers expressions/types only"
---

# Statement-block nesting joins the L-81 depth guard (parser SIGABRT)

## Goal

Restore the crate's stated invariant ("it never aborts", parser.rs:4-5): deeply nested statement
blocks must return a bounded `ParseError`, not overflow the stack. Reproduced at 0.45.0: 900
nested `when` blocks SIGABRT `fluxlang compile` on the default 8 MiB stack; ~200 levels suffice on
a 2 MiB tokio-worker stack.

## Acceptance

- [ ] Failing-first: extend `deeply_nested_input_is_bounded_not_aborting`
      (`parser.rs:1728-1770`) with a nested-*statement* leg at depth 20,000 — the axis the current
      test omits — asserting a bounded diagnostic, no abort.
- [ ] The same input through `lower_cst`/`cst_decode` (which recurse per block with no depth
      field) is also bounded — either guarded or converted to a worklist; test both `parse` and
      `parse_program` entries.
- [ ] The tolerant editor path (`parse_cst` alone → `highlight`, `format_source`) survives the
      same input.
- [ ] A WASM/portable note: verify or adjust `MAX_PARSE_DEPTH` for the `flux_portable` build's
      smaller stack (open question from the review).

## Progress
-

## Notes

- Suggested fix: thread the existing `enter()`/`leave()` guard (`parser.rs:171-183`,
  `MAX_PARSE_DEPTH = 128`) through `block` (parser.rs:782), `statement` (parser.rs:804), and
  `block_if_indented`; add a depth counter to the lowerer's recursive walk
  (`cst_decode.rs:206-231, 360-384`). The expression evaluator's guard is the pattern to copy —
  thread-local counter with RAII decrement, tested with a leak check (`expr.rs:383-418, 1062-1084`).
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F1. Fourth instance of the
  "guard tested against its own assumptions" pattern — the new test must cover the previously
  missing axis, not re-prove the covered ones.
