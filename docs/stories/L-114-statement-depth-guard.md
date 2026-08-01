---
id: L-114
title: "Statement-block nesting joins the L-81 depth guard (parser SIGABRT)"
pillar: Language
status: in-progress
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

- [x] Failing-first: extend `deeply_nested_input_is_bounded_not_aborting`
      (`parser.rs:1728-1770`) with a nested-*statement* leg at depth 20,000 — the axis the current
      test omits — asserting a bounded diagnostic, no abort.
      → `parser.rs:1805-1817`, at depth **2,000** rather than 20,000 (see Progress).
- [x] The same input through `lower_cst`/`cst_decode` (which recurse per block with no depth
      field) is also bounded — either guarded or converted to a worklist; test both `parse` and
      `parse_program` entries.
      → guarded: `cst_decode.rs:206-251` (`MAX_LOWER_DEPTH`, RAII counter) + its own failing-first
      test `cst_decode.rs:2763`; both strict entries asserted in `parser.rs:1856-1868`.
- [x] The tolerant editor path (`parse_cst` alone → `highlight`, `format_source`) survives the
      same input. → `parser.rs:1880-1889` (`format_source` was the *second* SIGABRT at the base).
- [x] A WASM/portable note: verify or adjust `MAX_PARSE_DEPTH` for the `flux_portable` build's
      smaller stack (open question from the review).
      → **verified, no adjustment needed**: `wasm_parity.rs:192` drives the portable module to the
      guard's own worst case (4× the ceiling) and it agrees with the native engine without trapping.

## Progress

- **Landed the guard at `block_if_indented`** (`parser.rs:328`), the single cut through the
  statement-nesting cycle `block` → `statement` → a block statement → `block_if_indented`. Over the
  cap the whole indented region is swallowed into one `ERROR` node by the new iterative
  `error_block` (`parser.rs:344`), so the tree stays lossless and the rest of the file still parses.
  `MAX_PARSE_DEPTH` is unchanged at 128 and now shared with `cst_decode`.
- **The lowerer is guarded too**, with the `expr.rs` shape the Notes named: a thread-local counter
  with an RAII decrement, `MAX_LOWER_DEPTH = 256`, checked at compile time to stay above the
  parser's ceiling so no program the parser accepts can fail to lower.
- **Depth 2,000, not 20,000, on the statement axis.** Statement blocks are indentation-delimited, so
  a depth-`n` fixture costs O(n²) source bytes: 20,000 levels is a 400 MiB string. 2,000 is a ~4 MiB
  fixture and already twice the ~900 levels the review measured. The lowerer's own test uses a
  hand-built green tree (no indentation cost) but stops at 3,000 for a different reason — see below.
- **What actually aborted at the base**, measured rather than assumed: `parse_cst` alone survives
  2,000 levels (253 ms, zero diagnostics — it just hands a 2,000-deep tree downstream); `parse()`
  and `format_source()` both SIGABRT on it. So the review's "parser SIGABRT" is really the *lowerer*
  at ~900–2,000 levels, with the parser itself going over at ~6,000.
- **Adjacent, filed not fixed: rowan's green-node `Drop` is recursive** and aborts at ~4,000 levels
  of nesting, independent of any flux code. Unreachable from `.flux` source now that the parser caps
  tree depth at 128 blocks, but it bounds what any hand-built-tree test can use.

## Notes

- Suggested fix: thread the existing `enter()`/`leave()` guard (`parser.rs:171-183`,
  `MAX_PARSE_DEPTH = 128`) through `block` (parser.rs:782), `statement` (parser.rs:804), and
  `block_if_indented`; add a depth counter to the lowerer's recursive walk
  (`cst_decode.rs:206-231, 360-384`). The expression evaluator's guard is the pattern to copy —
  thread-local counter with RAII decrement, tested with a leak check (`expr.rs:383-418, 1062-1084`).
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F1. Fourth instance of the
  "guard tested against its own assumptions" pattern — the new test must cover the previously
  missing axis, not re-prove the covered ones.
