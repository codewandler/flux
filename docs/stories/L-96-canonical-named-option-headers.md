---
id: L-96
title: Canonical control headers use call-like named options
pillar: Language
status: done
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

- [x] Failing-first CST/AST-equivalence tests cover canonical named-option forms for `confirm`,
      `retry`, `loop`, `race`, `throttle`, `debounce`, `await`, `repeat`, and `each`.
- [x] The formatter emits `confirm "Open issue?", risk: medium` and the full triage fixture in the
      design round-trips exactly.
- [x] Optional values use `name: value`; primary operands and result binding remain readable, e.g.
      `retry 3, backoff: exponential, delay: 500ms -> out` and
      `loop for 10s, every: 1s, until: done -> last`.
- [x] Current fixed-order space-keyword forms remain accepted and lower to the identical AST.
- [x] Structural headers (`parallel`/`branch`, `match`/`case`, `try`/`catch`, `scope`/`finally`,
      `saga`/`step`/`undo`) retain their word-and-indentation forms.
- [x] CST formatting, syntax docs, reference examples, LSP highlighting/completion, randomized
      format round-trips, and the checked-in Flux corpus agree on the canonical spelling.
- [x] No `DraftAst`, analyzer, optimizer, or runtime semantics change; the full gate is green.

## Progress

- 2026-07-30: Landed. The rule is one line: **the first operand of a control header stays
  positional** (keeping its structural connector word — `for`, `in`), **everything after it is a
  `name: value` option**, and the result target stays `-> name`. The parser lifts each option —
  leading comma included — into a new `SyntaxKind::HEADER_OPTION`, so the header text either side of
  the option run is *exactly* the legacy space-keyword header and one lowering path decodes both.
  The change is **purely additive**: every previously-parsing header still parses to the identical
  `DraftAst`, which is why `website/docs` fences and the frozen `cst_agreement` AST hashes needed no
  edit.
- Canonical spellings now emitted: `confirm "…", risk: r` · `retry n, backoff: b, delay: d -> x` ·
  `loop for d, every: d, until: e -> x` · `repeat n, until: e -> x` ·
  `throttle "n", max: m, per: w` · `debounce "n", wait: w` · `await b = "s", when: e`.
- Two headers deliberately keep an all-positional shape because nothing follows their first operand:
  `race <timeout> [-> $b]` (`race timeout: 5s` is accepted as an alias but not emitted) and
  `each x in src [-> [flat] c]`. Both are covered by the equivalence table anyway.
- `until` on `repeat`/`loop` moved into the header as an option; the legacy first-body-line clause is
  still accepted, and supplying both is a parse error.
- Fixed a **pre-existing** CST-formatter bug this surfaced: `wants_space` inserted a space between a
  NUMBER and a following IDENT, so `delay: 500ms` reformatted to `500 ms` and the equivalence guard
  then rejected the whole file. Duration suffixes now keep the author's adjacency.

## Notes

- `crates/flux-lang/docs/syntax.md` used to mark comma-kwarg control headers aspirational and said
  the requested `confirm` form does not parse; it now documents the canonical form and records the
  legacy spellings as still accepted.
- **Editor-tooling mirrors are still owed** (no drift guard, per the workspace `AGENTS.md`): the
  website Prism grammar already lists `risk|backoff|delay|until|for|every|per`, but the two *new*
  option labels `max` and `wait` need adding there and in `flux-tree-sitter` / the TextMate and
  IntelliJ grammars. `website/` is outside this story's write set.
