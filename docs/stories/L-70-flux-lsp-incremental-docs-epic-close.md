---
id: L-70
title: Incremental reparse + comment-preserving format + docs/packaging + epic close
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lsp.md
note: "Epic close — the CST payoffs (incremental reparse, comment-preserving format) + install/Helix docs + distribution + final gate."
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
- [ ] Docs: an install/usage page for `flux-lsp` + the Helix `languages.toml` recipe; `flux-lsp` added
      to the release/distribution flow.
- [ ] CHANGELOG entries; roadmap epic narrative moved proposed → shipped; full gate green in both
      workspaces.

## Progress
- (not started — depends on the LSP core L-64–L-67)

## Notes
- Depends on **L-64–L-67**. Scope here is optional polish + closeout; trim freely.
