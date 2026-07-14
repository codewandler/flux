---
id: L-69
title: flux-lsp semantic tokens — for clients that render them
pillar: Language
status: done
priority: 8
epic: flux-lsp
design: docs/designs/flux-lsp.md
note: "Semantic tokens from the CST token stream — NOT a Helix path (Helix renders tree-sitter only; highlighting now comes from codewandler/flux-tree-sitter). Value: VS Code/Neovim + semantic distinctions a grammar can't make."
---

# flux-lsp semantic tokens — for clients that render them

## Goal
Emit `textDocument/semanticTokens` classified from the CST token stream (`SyntaxKind`) for LSP
clients that render them (VS Code; Neovim layered over tree-sitter). **Not a Helix feature**:
Helix (as of 25.07) colours via tree-sitter only, and Flux highlighting there is covered by
[`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter). The residual
value here is *semantic* classification a grammar can't do — e.g. a registry-known op vs an
unknown identifier, bound vs unbound `$symbols`.

## Acceptance
- [ ] `initialize` advertises a semantic-tokens legend (keyword, op, string, number, `$symbol`,
      `@annotation`, comment, type…).
- [ ] Full-document semantic tokens map CST tokens to legend types + modifiers.
- [ ] Failing-first: a sample document's tokens cover keywords, op calls, strings, `$symbols`, and
      comments with the expected types.

## Progress
- (not started — depends on L-59/L-64)

## Notes
- Depends on **L-64** + the CST token stream.
- 2026-07-09: `codewandler/flux-tree-sitter` shipped (grammar + queries for Helix/Neovim/Zed),
  which removes the original "Helix highlighting" motivation entirely — re-scoped as above and
  deprioritized accordingly.
