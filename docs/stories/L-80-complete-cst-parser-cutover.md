---
id: L-80
title: Complete the CST parser cutover
pillar: Language
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: CST lowering still delegates semantic acceptance to the legacy line parser
---

# Complete the CST parser cutover

## Goal

Make the lossless CST the only accepting Flux-Lang parser while preserving the current `DraftAst`,
diagnostics, ranges, formatting, and runtime semantics.

## Acceptance

- [x] `parse`/`parse_program` lower semantic AST solely from the CST; `lower_cst` no longer calls or
      re-materializes source for the legacy line parser.
- [x] Before removal, corpus/property tests compare every legacy-accepted language fixture and pin
      AST equivalence, error classification/location, comments/ranges, and format→parse round trips.
- [x] `parse_with_ranges`, LSP diagnostics/completion/hover/semantic tokens, formatter comment
      preservation, module/composite parsing, and the standalone `fluxlang` workbench remain correct.
- [x] The legacy accepting implementation is deleted or reduced to a compatibility wrapper over the
      CST path; no syntax feature requires edits in two accepting parsers.
- [x] Malformed/incomplete editor input remains tolerant in the CST while strict compile APIs retain
      their existing refusal behavior.
- [x] Language reference/status/design docs are updated, and all language skill/website sync tests
      pass; highlighting grammars change only if syntax actually changes.

## Progress

- 2026-07-14 — `parse` and `parse_program` now strictly lower the sole tolerant CST through
  structured `cst_decode`; the legacy accepting parser is gone. Shipped-corpus agreement,
  all-node round-trip properties, range/LSP/module/workbench, tolerant-error, and sync tests pass.
  The nine legacy-accepted shipped text fixtures are checked against independent SHA-256 AST
  evidence frozen from the retired parser at commit `6e691962a1e4c2f435d78d0466d12121af4799c2`;
  the corpus also pins exact token/comment/declaration/body ranges and executable round trips.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Explicit residual of [L-59](L-59-cst-lower-to-draftast-repoint-parse.md), which established the CST
  lowering path while retaining semantic delegation for compatibility.
