---
id: L-59
title: cst_to_draft lowering + re-point parse/parse_program (behavior-preserving)
pillar: Language
status: backlog
priority:
epic: flux-lsp
design: docs/designs/flux-lang-cst.md
note: "CST foundation KEYSTONE — GATED on isolation. Projects the CST to today's DraftAst; re-points parse/parse_program; range side-map for analyzer diagnostics. All existing tests + the round-trip invariant stay green."
---

# cst_to_draft lowering + re-point parse/parse_program (behavior-preserving)

## Goal
Project a clean CST to today's `DraftAst` exactly, re-point `parse`/`parse_program` onto
`lex → parse → (strict) cst_to_draft`, and record a `DraftAst` node-path → `TextRange` side-map so
the message-only `analyze::Diagnostic` resolves to real LSP ranges. This is the keystone: it keeps
`analyze`/`format`/runtime/optimizer/planner/data-transforms **unchanged**.

## Acceptance
- [ ] New `crates/flux-lang/src/lower_cst.rs` — `cst_to_draft(&SyntaxNode) -> Result<DraftAst,
      Vec<Diagnostic>>` (strict `parse` errors if any `ERROR` node); the `"""`→escaped-JSON re-encode
      lives here.
- [ ] `parse`/`parse_program` re-pointed; every existing caller unchanged.
- [ ] **Backbone:** all existing `flux-lang` / `flux-flow` / `flux-eval` tests pass and
      `parse(&format(&ast)) == ast` holds. Pinned error-text tests reproduced or *consciously* updated.
- [ ] Failing-first `analyzer_diagnostics_carry_ranges`: an unbound-`$var` / arity error resolves to a
      `TextRange` via the side-map.

## Progress
- (not started — gated on isolation; depends on L-58)

## Notes
- **Gated** on isolation. Depends on **L-58**. Inviolable: round-trip invariant + `DraftAst` shape;
  negotiable: exact error wording. See [flux-lang-cst.md](../designs/flux-lang-cst.md).
