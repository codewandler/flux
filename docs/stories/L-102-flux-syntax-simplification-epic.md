---
id: L-102
title: "Flux syntax simplification — one way to write each thing (epic)"
pillar: Language
status: ready
priority: 9
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang, flux-cli, flux-lsp]
note: "EPIC — simplify by subtraction: canonical dialect everywhere, a migration tool, then delete the legacy grammar; supersedes L-98/L-99's direction"
---

# Flux syntax simplification — one way to write each thing (epic)

## Goal

Make canonical Flux the *only* dialect an author meets: ship the missing `fluxlang fmt` migration
tool, move the corpus/docs/spec to the canonical column, then deprecate and remove the ~9 doubled
spelling dimensions the parser accepts today — shrinking the grammar, the four editor-grammar
mirrors, and the model prior to one consistent surface.

## Acceptance

- [ ] L-103 ships `fluxlang fmt` (CST-based, comment-preserving, `--check` mode).
- [ ] L-104 migrates the flagship corpus (agent-loop.flux, examples/, doc snippets) to the
      canonical dialect with zero semantic diffs (AST-equality gate).
- [ ] L-105 rewrites `docs/syntax.md` to a single dialect and relocates aspirational sections.
- [ ] L-106 emits deprecation diagnostics for every legacy spelling; L-107 removes them
      (breaking ⇒ MINOR) one release later.
- [ ] L-108–L-112 each land or are explicitly rejected with a recorded decision.
- [ ] L-98 (Tape) and L-99 (S-Flux) are re-evaluated against this epic's direction before any
      implementation starts.

## Progress

- 2026-08-01: Epic opened from the syntax-simplification proposal
  (docs/designs/flux-syntax-simplification.md), accepted by the user alongside the flux-lang
  subsystem review (docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md).

## Notes

- The organizing idea: the canonical grammar is already good — `format(ast)` emits it — but it is
  one of several accepted dialects and nothing pushes authors toward it. Simplify by subtraction,
  per the repo's no-fallbacks/clean-cutover doctrine, applied to grammar.
- The tree-sitter mirror cannot parse the canonical dialect today (7/15 canonical examples red,
  `.github/workflows/tree-sitter-corpus.yml:21-30`) — every doubled dimension doubles what four
  grammars must mirror. Grammar-surface reduction is mirror-cost reduction.
- Child stories: L-103 … L-112. Related hardening epic: L-113.
