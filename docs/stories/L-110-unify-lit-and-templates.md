---
id: L-110
title: "Braces are always a value — unify lit and obj/list templates"
pillar: Language
status: backlog
priority: 38
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang]
note: "P8 — node kind must not depend on whether the content happens to be valid JSON; gives all-literal/empty templates a native spelling"
---

# Braces are always a value — unify lit and obj/list templates

## Goal

`{…}`/`[…]` currently parse to a `lit` when the content is valid JSON and to an `obj`/`list`
template otherwise (syntax.md § value templates) — the AST node kind of a value depends on whether
the author quoted their keys, and all-literal or empty templates are unspellable natively
(`@json`-only). Make every brace/bracket a template at parse time and normalize all-literal
templates to `lit` in lowering (or make the runtime treat the two identically) so the distinction
stops being author-visible.

## Acceptance

- [ ] Failing-first: `x = { "a": 1 }` and `x = { a: 1 }` produce semantically identical execution
      (same bound value, same trace shape); an empty `{}` / `[]` template has a native spelling
      and round-trips without `@json`.
- [ ] Wire compatibility: JSON ASTs carrying either node kind keep decoding; a
      normalization test pins `lit ⇄ all-literal-template` equivalence both directions.
- [ ] Round-trip property generator updated: all-literal and empty templates join the pools; the
      kind-census assertion still passes.
- [ ] `@json` remaining uses are enumerated in the spec (non-identifier names only, if L-109 has
      landed).

## Progress
-

## Notes

- Decide normalization direction in lowering (template→lit) vs interpreter equivalence; prefer
  lowering so the optimizer and analyzer see one shape.
