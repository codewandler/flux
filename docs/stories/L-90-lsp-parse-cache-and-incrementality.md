---
id: L-90
title: Per-document parse cache, real incremental reparse, semantic-token range/delta
pillar: Language
status: backlog
epic: flux-lsp-round-2
design: docs/designs/flux-lsp-round-2.md
note: did_change applies ranged edits then full-reparses (main.rs:237-250 → refresh:169), which is not the rowan node reuse L-70 asked for; and every handler re-parses from text per request (format_document:107, semantic_tokens:1142, signatures_for_document:81) — semantic tokens are full-only, `range: Some(false)` at main.rs:211
---

# Per-document parse cache, real incremental reparse, semantic-token range/delta

## Goal

Make each keystroke cost one parse instead of several, so the round-2 features that need *more*
syntactic context (cursor-aware completion, CST hover) are affordable on a large buffer.

## Why (evidence)

- `Backend.docs` (`crates/flux-lsp/src/main.rs:146`) stores `Url → String` — text only, no tree.
- `did_change` (`main.rs:237-250`) applies each ranged edit to the stored string and calls `refresh`
  (`main.rs:165-177`), which runs `flux_lang::parser::parse_cst(text)` over the whole buffer. The
  L-70 acceptance asked for "incremental reparse wired for `didChange` (rowan node reuse)"
  (`docs/stories/L-70-flux-lsp-incremental-docs-epic-close.md:21`); what shipped is incremental
  *sync* — the reparse is still whole-document.
- Every other handler re-parses independently: `signatures_for_document` (`main.rs:81`) on each
  completion, hover, and semantic-tokens request; `format_document` (`main.rs:104` and `:107`) twice
  per format, plus twice more in the comment guard (`:124`); `semantic_tokens` (`main.rs:1142`).
- `refresh` already documents that this class of bug was found once before — "previously this path
  parsed the buffer three times per keystroke — review finding, 2026-07-09" (`main.rs:166-168`).
- Semantic tokens advertise `full: Bool(true)`, `range: Some(false)` and no delta
  (`main.rs:209-211`), so a client that renders them re-serializes the entire token stream on every
  change.

## Acceptance

- [ ] The document store holds the text *and* its `Parse`; every handler reads the cached tree
      instead of calling `parse_cst` itself.
- [ ] `did_change` updates the cached tree incrementally (rowan node reuse) with an
      equivalence test proving the incremental result is identical to a full reparse of the final
      text — extending `incremental_edits_match_full_reparse` (`main.rs:1719`) from text equality to
      tree equality.
- [ ] Semantic tokens gain `range` support and `full/delta` with `result_id` (today `result_id:
      None`, `main.rs:1192`), or the advertised capability is corrected to match what is
      implemented — no capability claimed without a handler.
- [ ] Failing-first: a test counting parses per `didChange` + completion + hover cycle, red at the
      current count and green at one.
- [ ] A measured before/after on a large `.flux` buffer recorded in the story's Progress log.

## Progress
- (not started)

## Notes
- Land this before or with L-85/L-86 — both add syntactic work per request.
- Range/delta semantic tokens are the lowest-value item in the epic: Helix does not render LSP
  semantic tokens at all (`docs/designs/flux-lsp.md:104`); the audience is VS Code / Neovim.
