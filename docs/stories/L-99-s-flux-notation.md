---
id: L-99
title: "S-Flux — a self-delimiting Lisp projection of DraftAst"
pillar: Language
status: ready
priority: 26
epic: flux-notation-workbench
design: docs/designs/flux-notation-workbench.md
areas: [flux-lang]
note: "Data-only S-expressions with named fields and `(ast {...})` escape — no macros, eval, or embedded Lisp runtime"
---

# S-Flux — a self-delimiting Lisp projection of `DraftAst`

## Goal

Offer agents and transport code a self-delimiting prefix notation that is nearly mechanical to
construct from the AST while remaining readable enough to inspect.

## Acceptance

- [ ] The grammar defines lists, literals, symbols, node heads, named field keywords, and the
      `(ast {...})` raw-node escape without reader macros or executable forms.
- [ ] Failing-first tests parse the shared triage fixture to the canonical Flux AST and pin its
      deterministic S-Flux rendering.
- [ ] Native-core plus escape property tests satisfy `parse_sflux(format_sflux(ast)) == ast`.
- [ ] Unbalanced forms, duplicate fields, unknown node heads, wrong arity, and invalid nested
      statement/value positions produce located diagnostics.
- [ ] The implementation is a data codec only: it provides no macro system, evaluator, host calls,
      or bypass around normal Flux analysis and dispatch.

## Progress

- (not started)
