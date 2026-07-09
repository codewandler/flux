---
id: L-73
title: Public editor-setup docs page (Helix flagship) + LSP docs pass
pillar: Language
status: done
priority:
epic: flux-lsp
design: docs/designs/flux-lsp.md
note: "Pulls the docs bullet of L-70 forward: website editors page, Helix highlight→LSP recipe, cross-links, flux-lsp crate README."
---

# Public editor-setup docs page (Helix flagship) + LSP docs pass

## Goal
One public page that takes a user from a plain `.flux` buffer to full editor support —
tree-sitter colour plus `flux-lsp` intelligence — with Helix as the flagship recipe, plus a
consistency pass over the surrounding syntax/LSP/editor docs.

## Acceptance
- [x] `website/docs/language/editors.md` exists and is registered in the Flux-Lang → Reference
      sidebar: Helix covers highlighting first, the LSP on top, and `hx --health flux`
      verification; Neovim/Zed/IntelliJ sections stay within tested truth (no invented config).
- [x] The LSP capability table lists only shipped features (diagnostics, completion, hover,
      formatting) and names go-to-def/symbols/semantic tokens as not yet implemented.
- [x] `tooling.md` §Editor support is a slim pointer (anchor preserved); getting-started, tour,
      overview, and flows-and-syntax link the page; root README and docs/README retargeted.
- [x] `crates/flux-lsp/README.md` added; the flux-lsp design doc points at the public page; L-70
      no longer double-owns the docs bullet.
- [x] `cd website && npm run build` green (link check via `onBrokenLinks: 'throw'`); board
      regenerated; CHANGELOG entry added.

## Progress
- 2026-07-09: page written from the recipe verified live on Helix 25.07.1 (grammar fetch/build,
  version-matched query copy from the fetched source, `use-grammars` tip, LSP on top,
  `hx --health flux`). Sidebar + Prism `lua` registered; tooling slimmed; cross-links placed;
  crate README + design-doc pointer added. First `npm run build` green.

## Notes
- Source material: `.helix/languages.toml` (repo-local wiring), the flux-tree-sitter README
  (Neovim recipe), `docs/designs/flux-lsp.md` (feature truth: L-68/L-69 backlog).
- Zed has no tested recipe — the page says so honestly rather than inventing steps.
