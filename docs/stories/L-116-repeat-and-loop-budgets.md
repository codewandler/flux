---
id: L-116
title: "`repeat` gets the loop budget discipline; budget scope is decided"
pillar: Language
status: ready
priority: 8
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang, flux-flow]
note: "Review F3+F4, MEDIUM — repeat has no iteration budget/transcript cap/yield (timeout can never fire over a pure body); loop budgets are per-activation, doc says per-execution"
---

# `repeat` gets the loop budget discipline; budget scope is decided

## Goal

The `Repeat` arm (`runtime.rs:1956-2021`) has none of the three protections the `loop` arm carries
(iteration budget `runtime.rs:2496-2508`, `cap_transcript`, `yield_now`): a wire-supplied AST that
skipped `lower()` can carry `max: u32::MAX` and spin ~4.3e9 iterations, and a pure body has no
yield point so an enclosing `timeout` (tokio::time::timeout, `runtime.rs:3020-3034`) can never
fire. Separately, the documented "per-execution" budget (`runtime.rs:42`) is actually
per-node-activation (function-local counter), so nested loops multiply. Fix `repeat`; decide and
align the budget-scope semantics.

## Acceptance

- [ ] Failing-first: a `repeat` with `max` above `DEFAULT_MAX_LOOP_ITERATIONS` through
      `execute_flow` (no analysis) fails with the budget error instead of spinning; a pure-body
      `repeat` inside `timeout` actually times out (the yield point exists).
- [ ] `cap_transcript` applies on the repeat path (transcript ring bounded).
- [ ] Budget scope decided and enforced-as-documented: either a per-execution counter threaded
      through `exec_body` (nested `each`/`repeat`/`loop` share one budget) or the doc-comment
      rewritten to per-activation with the multiplication risk stated; a nested-loops test pins
      whichever is chosen.
- [ ] Answered in the story: does any production path reach `execute_flow` without `lower()`?
      (flux-flow read — determines the real-world severity and belongs in the closing note.)

## Progress
-

## Notes

- Suggested fix shape: mirror the `loop` arm's three lines into `Repeat`; for scope, prefer one
  per-execution `u64` budget owned by the execution context — smallest diff that makes
  `runtime.rs:42`'s comment true.
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F3, F4.
