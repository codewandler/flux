---
id: L-96
title: Canonical control headers use call-like named options
pillar: Language
status: ready
priority: 17
epic: flux-notation-workbench
design: docs/designs/flux-notation-workbench.md
areas: [flux-lang, flux-lsp]
note: "Complete L-93: emit `confirm \"…\", risk: medium` and consistent named options while accepting legacy space-keyword headers"
---

# Canonical control headers use call-like named options

## Goal

Complete L-93's readable normal syntax by giving parameterized control headers the same
comma-plus-label vocabulary as named call inputs, without hiding structural control-flow words.

## Acceptance

- [ ] Failing-first CST/AST-equivalence tests cover canonical named-option forms for `confirm`,
      `retry`, `loop`, `race`, `throttle`, `debounce`, `await`, `repeat`, and `each`.
- [ ] The formatter emits `confirm "Open issue?", risk: medium` and the full triage fixture in the
      design round-trips exactly.
- [ ] Optional values use `name: value`; primary operands and result binding remain readable, e.g.
      `retry 3, backoff: exponential, delay: 500ms -> out` and
      `loop for 10s, every: 1s, until: done -> last`.
- [ ] Current fixed-order space-keyword forms remain accepted and lower to the identical AST.
- [ ] Structural headers (`parallel`/`branch`, `match`/`case`, `try`/`catch`, `scope`/`finally`,
      `saga`/`step`/`undo`) retain their word-and-indentation forms.
- [ ] CST formatting, syntax docs, reference examples, LSP highlighting/completion, randomized
      format round-trips, and the checked-in Flux corpus agree on the canonical spelling.
- [ ] No `DraftAst`, analyzer, optimizer, or runtime semantics change; the full gate is green.

## Progress

- (not started)

## Notes

- `crates/flux-lang/docs/syntax.md` currently marks comma-kwarg control headers aspirational and
  explicitly says the requested `confirm` form does not parse.
