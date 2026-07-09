---
id: L-69
title: flux-lsp semantic tokens — Helix highlighting
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lsp.md
note: "Highlighting via LSP semantic tokens from the CST token stream — Helix has no tree-sitter-flux, so this is its highlighting path (near-free from the CST)."
---

# flux-lsp semantic tokens — Helix highlighting

## Goal
Emit `textDocument/semanticTokens` classified from the CST token stream (`SyntaxKind`) so Helix — which
has no `tree-sitter-flux` grammar — gets syntax highlighting from the server.

## Acceptance
- [ ] `initialize` advertises a semantic-tokens legend (keyword, op, string, number, `$symbol`,
      `@annotation`, comment, type…).
- [ ] Full-document semantic tokens map CST tokens to legend types + modifiers.
- [ ] Failing-first: a sample document's tokens cover keywords, op calls, strings, `$symbols`, and
      comments with the expected types.

## Progress
- (not started — depends on L-59/L-64)

## Notes
- Depends on **L-64** + the CST token stream. A native `tree-sitter-flux` grammar remains an explicit
  out-of-scope future track.
