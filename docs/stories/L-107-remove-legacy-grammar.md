---
id: L-107
title: "Remove the legacy grammar (breaking ⇒ MINOR)"
pillar: Language
status: backlog
priority: 32
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang, flux-lsp]
note: "P5b — one release after L-106: the strict parser rejects legacy spellings; the tolerant CST keeps recognizing them for the quick-fix only"
---

# Remove the legacy grammar (breaking ⇒ MINOR)

## Goal

One release after L-106's deprecation window: the strict `parse`/`parse_program` path rejects the
legacy spellings with an error naming the canonical replacement. The grammar surface the four
editor mirrors must describe shrinks accordingly.

## Acceptance

- [ ] Each legacy dimension's fixture flips from "warns" (L-106) to "errors with the canonical
      replacement named" — failing-first per dimension.
- [ ] The tolerant CST still recognizes legacy forms *only* to power the LSP canonicalize
      quick-fix; strict parse refuses them.
- [ ] Round-trip property tests and `cst_agreement` are updated deliberately (the frozen oracle
      covers the retired parser's corpus — record what changes and why in the story, per the
      oracle's own contract).
- [ ] Editor-grammar mirrors updated in the same pass: Prism, tree-sitter (with L-118), and a
      grep of the TextMate/IntelliJ grammars (unguarded — AGENTS.md flux-lang §mirrors).
- [ ] Release notes: breaking ⇒ MINOR per the repo SemVer rule; `WHATS-NEW.md` entry with the
      one-command migration (`fluxlang fmt`).

## Progress
-

## Notes

- Do not start before L-104 is fully landed and one release has shipped with L-106's warnings —
  the removal must never strand a shipped `.flux` file without a mechanical fix.
