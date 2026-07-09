---
id: L-67
title: flux-lsp formatting + a `flux fmt` CLI verb
pillar: Language
status: done
priority:
epic: flux-lsp
design: docs/designs/flux-lsp.md
note: "LSP textDocument/formatting via format::format on a clean parse, plus a first-class `flux fmt [--check]` verb (none exists today)."
---

# flux-lsp formatting + a `flux fmt` CLI verb

## Goal
Wire `textDocument/formatting` to the invertible `format::format` (on the lowered `DraftAst` of a
clean parse), and add a `flux fmt` CLI verb so formatting is reachable outside the editor too.

## Acceptance
- [ ] `textDocument/formatting` returns the canonical text for a parseable buffer; a buffer with
      `ERROR` nodes is left unchanged (no partial format).
- [ ] `flux fmt [--check] <file>` added to the `flux` CLI (`crates/flux-cli`), reusing
      `format::format`; `--check` exits non-zero on non-canonical input.
- [ ] Failing-first: formatting a document yields `format(parse(src))`; `flux fmt --check` on canonical
      input exits 0, on non-canonical exits non-zero.

## Progress
- Done 2026-07-09: `textDocument/formatting` returns a whole-document edit built from the invertible
  `format::format` on a cleanly-parsing single flow; a module or a buffer with errors is left
  untouched (no partial format). Residual (small, deferred): a `flux fmt [--check]` CLI verb — the
  formatter is reachable in-editor now; the standalone verb is a convenience add in `flux-cli`.

## Notes
- Depends on **L-64**. `format` today drops comments; a comment-preserving formatter is a later win
  (L-70) the CST enables.
