---
id: L-66
title: flux-lsp hover — op signatures/schemas + node-kind & prelude docs
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lsp.md
note: "LSP hover via token-at-offset hit-testing on the CST → op signature / node-kind doc / prelude doc."
---

# flux-lsp hover — op signatures/schemas + node-kind & prelude docs

## Goal
`textDocument/hover`: find the CST token at the offset and render the right documentation — an op's
signature/schema for an op call, the node-kind doc for a keyword, the prelude-type doc for a type.

## Acceptance
- [ ] Token-at-offset hit-testing on the CST (trivial given structural spans).
- [ ] Op hover renders `OpSignature` (description, params/types, effects/risk/idempotency) via
      `OpRegistry::get`.
- [ ] Keyword hover renders the `schema::node_kind_rows()` doc; type hover renders
      `prelude::prelude_type_rows()`.
- [ ] Failing-first: hover over an op call returns its signature; over a keyword returns its doc.

## Progress
- (not started — depends on L-64)

## Notes
- Depends on **L-64**. Reuses the `catalog.rs` cache from L-65.
