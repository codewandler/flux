---
id: L-64
title: flux-lsp crate — tower-lsp server, text sync, diagnostics, Helix wiring
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lsp.md
note: "LSP MVP — the end-to-end Helix loop: a standalone flux-lsp binary (L6) publishing positioned diagnostics, wired to hx config-only."
---

# flux-lsp crate — tower-lsp server, text sync, diagnostics, Helix wiring

## Goal
Stand up the `flux-lsp` crate: a standalone stdio LSP server (tower-lsp) that syncs open buffers and
publishes **positioned** diagnostics from the tolerant CST parse (`ERROR` nodes) + `analyze` (ranges
via the L-59 side-map), wired into Helix so `hx foo.flux` works end-to-end.

## Acceptance
- [ ] New `crates/flux-lsp` (L6 binary `flux-lsp`); registered in root `Cargo.toml` members +
      workspace deps, the L6 arm of `flux-codegate` `layer()`, and `docs/architecture.md`.
- [ ] `initialize` advertises diagnostics; `didOpen`/`didChange`(debounced)/`didClose` maintain a
      document store + line-index; `publishDiagnostics` emits LSP `Range`s.
- [ ] `convert.rs` maps CST `TextRange` ↔ LSP `Range` via the line-index.
- [ ] `.helix/languages.toml` committed (the `[language-server.flux-lsp]` + `[[language]]` blocks).
- [ ] Failing-first integration test over an in-memory duplex: `didOpen` a bad buffer → a diagnostic
      at the expected range. Manual `hx` smoke documented.

## Progress
- (not started — depends on the CST foundation L-59)

## Notes
- Depends on **L-59** (the CST + range side-map). Deps: `flux-lang`, `flux-flow`, `flux-tools`,
  `flux-runtime`, `tower-lsp`, `lsp-types`, `tokio`. See [flux-lsp.md](../designs/flux-lsp.md).
