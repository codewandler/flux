---
id: L-105
title: "docs/syntax.md teaches one dialect and stops contradicting itself"
pillar: Language
status: ready
priority: 14
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang]
note: "P4 — fix the mandatory-$ contradiction, move aspirational sections to the evolution doc, document `?` and `do`, one legacy-spelling appendix"
---

# docs/syntax.md teaches one dialect and stops contradicting itself

## Goal

The authoritative spec currently teaches the retired dialect (§ Symbols: "`$` is mandatory on
every symbol reference", syntax.md:253-256) while § Calls says bare is canonical (401-402), uses
`$` spellings in most later examples, interleaves aspirational grammar (`watch`, `type`
declarations, `expr(…)` call form, `@kind(…)`) with implemented grammar, and omits shipped syntax
(the `?` lenient-access suffix, the `do` spelling, `parse`'s `"form"` target). Rewrite it to one
dialect: canonical spellings in every section, legacy forms in a single appendix, aspirational
material relocated to `docs/designs/flux-lang-evolution.md`.

## Acceptance

- [ ] The § Symbols/§ Calls contradiction is gone; the stated rule is: bare identifiers, `$` only
      as the keyword-collision escape (matching `format.rs:507-513`).
- [ ] Every example in the file is canonical (`fluxlang fmt --check`-clean once L-103 exists).
- [ ] Aspirational sections are moved out; what remains is all implemented, and a doc test or the
      existing in-sync guards cover any snippet the tooling can parse.
- [ ] `?` (jq optional), `do` (with its deprecation status), and `parse`'s `"form"` are documented
      (review findings F10/F11 doc-side).
- [ ] One "Legacy spellings" appendix lists every still-accepted form with its canonical
      replacement — the input contract for L-106's diagnostics.

## Progress
-

## Notes

- Evidence lines in the review: syntax.md:253-256 vs 401-402 (contradiction), syntax.md:1000
  (missing `"form"`, code at analyze.rs:1810).
- reference.md's stale hand-written expr whitelist (1080-1087) is L-120's; this story is
  syntax.md-scoped.
