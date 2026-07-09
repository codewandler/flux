---
id: L-68
title: flux-lsp document symbols + go-to-definition
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lsp.md
note: "Navigation — a CST scope model with definition ranges powers documentSymbol + definition."
---

# flux-lsp document symbols + go-to-definition

## Goal
Build a CST scope/symbol model (bind sites, decl names, params) with definition ranges, and expose it
as `textDocument/documentSymbol` and `textDocument/definition`.

## Acceptance
- [ ] A scope model over the CST: `$var` binds (`bind`/`memo`/`each`/arrow/`parallel`-branch), decl
      names, and flow params, each with a definition `TextRange`.
- [ ] `documentSymbol` returns the flow/decl/bind outline; `definition` jumps a `$var` use to its bind.
- [ ] Failing-first: go-to-definition on a `$var` use resolves to its binding range; a document's
      symbol tree matches a fixture.

## Progress
- (not started — depends on L-59/L-64)

## Notes
- Depends on **L-64** (server) and the CST scope model. Filed for after the LSP core.
