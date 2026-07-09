---
id: L-70
title: Incremental reparse + comment-preserving format + docs/packaging + epic close
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lsp.md
note: "Epic close — the CST payoffs (incremental reparse, comment-preserving format) + distribution + final gate. Install/Helix docs shipped early in L-73."
---

# Incremental reparse + comment-preserving format + docs/packaging + epic close

## Goal
Cash the remaining CST payoffs and close the epic: incremental reparse for large buffers, a
comment-preserving formatter (today `format` drops comments), a docs page (install `flux-lsp` + wire
Helix), a distribution path, and the closing gate.

## Acceptance
- [ ] Incremental reparse wired for `didChange` (rowan node reuse) with an equivalence test vs. full
      reparse.
- [ ] Comment-preserving formatting path (CST-driven) with a round-trip test that keeps comments.
- [ ] `flux-lsp` added to the release/distribution flow (flip `dist = false`). The install/Helix
      docs shipped early in L-73 (`website/docs/language/editors.md` + crate README).
- [ ] CHANGELOG entries; roadmap epic narrative moved proposed → shipped; full gate green in both
      workspaces.

## Progress
- (not started — depends on the LSP core L-64–L-67)

## Notes
- Depends on **L-64–L-67**. Scope here is optional polish + closeout; trim freely.
- The docs bullet was pulled forward into **L-73** (public editor-setup page, done 2026-07-09);
  what remains here is distribution + the CST payoffs.
