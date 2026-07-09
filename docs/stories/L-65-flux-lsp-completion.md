---
id: L-65
title: flux-lsp completion — ops, node-kinds, prelude types, in-scope $vars
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lsp.md
note: "LSP completion from the Rust SSOT catalogs + a CST scope walk; cursor context from the token at the offset."
---

# flux-lsp completion — ops, node-kinds, prelude types, in-scope $vars

## Goal
`textDocument/completion` sourced from the authoritative catalogs and the CST: op names/signatures,
node-kind keywords, prelude types, and the `$vars` in scope at the cursor — chosen by cursor context.

## Acceptance
- [ ] Op completion from `flux_flow::registry::OpRegistry::{op_names,signatures}` (label + detail +
      doc from `OpSignature`).
- [ ] Keyword/node-kind completion from `schema::node_kind_rows()`; type completion from
      `prelude::prelude_type_rows()`.
- [ ] In-scope `$var` completion from a CST scope walk (bind/each/arrow/parallel-branch sites visible
      at the offset).
- [ ] Context selection: `$`/`@` sigil, statement head vs. arg position.
- [ ] Failing-first: completion at a chosen offset returns the expected op/keyword/`$var` set.

## Progress
- (not started — depends on L-64)

## Notes
- Depends on **L-64**. Build the registry once (`catalog.rs`); the IntelliJ `FluxVocabulary.kt` is
  reference-only — catalogs are the SSOT.
