---
id: C-365
title: Close the const/static/field alias blind spot and record the macro-body one
pillar: Core
status: backlog
epic: structural-gate-blind-spots
design: docs/designs/structural-gate-blind-spots.md
note: "C-263's closure review closed the `let` alias hole; the const/static/struct-field twin was never closed. Separately, syn never parses macro token streams — a repo-wide blind spot affecting EVERY source gate"
---

# Close the alias blind spot and record the macro-body one

## Goal

Make callable-alias capture complete, and stop every syn-based gate from implying coverage of code
it structurally cannot see.

## Acceptance

- [ ] The direct-I/O visitor captures callable aliases bound by `const`, `static` and struct fields,
      not only `let` (`crates/flux-codegate/src/lib.rs:755` handles `syn::Local` only).
- [ ] Failing-first fixture per binding form, in the gate's own self-tests.
- [ ] The macro-body limitation is stated explicitly in each syn-based gate's documentation —
      direct-I/O, raw-process, port-impl, pin census, registration census — rather than left
      implicit.
- [ ] Where a cheap mitigation exists (e.g. scanning macro token streams textually for the forbidden
      constructors), it is applied or its absence is reasoned.

## Progress

- 2026-08-01 — both mutations confirmed by inspection during validation.

## Notes

- `tokio::select! { r = tokio::fs::read(p) => .. }` is the realistic instance, not a contrived one.
