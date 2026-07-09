---
id: L-58
title: Tolerant event parser + rowan green tree with ERROR-node recovery
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lang-cst.md
note: "CST foundation — GATED on isolation. The error-recovery core: a hand-written event parser that always completes a tree, wrapping unexpected input in ERROR nodes and resyncing at NEWLINE/DEDENT."
---

# Tolerant event parser + rowan green tree with ERROR-node recovery

## Goal
Build the tolerant parser stage: a hand-written recursive descent over the L-57 token stream that
emits start/token/finish/error events, builds a **rowan** green tree, and **always completes a tree**
— wrapping unexpected input in `ERROR` nodes and resyncing at the next `NEWLINE`/`DEDENT` so a live,
incomplete buffer still yields a full CST plus a diagnostic list.

## Acceptance
- [ ] New `crates/flux-lang/src/cst.rs` — event parser + green-tree builder + a thin typed layer
      (`SyntaxNode` wrappers with accessors) over the current grammar's node set.
- [ ] Every node/token carries a `TextRange` (spans are structural).
- [ ] Failing-first `multi_error_buffer_yields_complete_tree`: a buffer with ≥2 independent errors
      produces one complete tree with N `ERROR` nodes at the correct ranges — no early abort.
- [ ] Failing-first `resync_at_block_boundary`: an error inside one statement does not corrupt the
      parse of sibling statements at the same indent.

## Progress
- (not started — gated on isolation; depends on L-57)

## Notes
- **Gated** on isolation (see design). Depends on **L-57**.
- Reproduces the *current* native grammar only; the 16 `@json`-only nodes get productions in
  L-60–L-63. Behaviour-preserving lowering is L-59.
