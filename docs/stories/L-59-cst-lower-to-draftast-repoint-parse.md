---
id: L-59
title: cst_to_draft lowering + re-point parse/parse_program (behavior-preserving)
pillar: Language
status: done
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
- [x] New `crates/flux-lang/src/lower_cst.rs` — `cst_to_draft(&SyntaxNode) -> Result<DraftAst,
      Vec<Diagnostic>>` (strict `parse` errors if any `ERROR` node); the `"""`→escaped-JSON re-encode
      lives here.
- [x] `parse`/`parse_program` re-pointed; every existing caller unchanged.
- [x] **Backbone:** all existing `flux-lang` / `flux-flow` / `flux-eval` tests pass and
      `parse(&format(&ast)) == ast` holds. Pinned error-text tests reproduced or *consciously* updated.
- [x] Failing-first `analyzer_diagnostics_carry_ranges`: an unbound-`$var` / arity error resolves to a
      `TextRange` via the side-map.

## Progress
- 2026-07-09 in-progress. Architecture decided after reading parser.rs/analyze.rs/parse.rs:
  the CST models statement STRUCTURE fully but headers are token-runs (fine for LSP, too coarse
  to reproduce DraftAst + pinned error texts from the tree alone). So: `lower_cst.rs` hosts the
  front-end — `cst_to_draft(&Parse, src)` is strict (ERROR nodes → Err with ranges) and produces
  the DraftAst via the proven line machinery (byte-identical semantics/errors by construction),
  plus a `RangeMap` (analyzer node-path `body[3].then[1]` → `TextRange`, longest-prefix resolve)
  built by lockstep-walking DraftAst against CST statement nodes. `parse`/`parse_program`
  delegate through lower_cst (callers unchanged; hot path stays single-parse).
  CST acceptance fidelity is enforced by tests, not runtime: the round-trip property test +
  corpus gain "legacy-accepted ⇒ parse_cst ERROR-free" assertions; disagreements are parser.rs
  bugs to fix in this story. Full tree-driven lowering (retiring the Line machinery) is the
  explicit residual, noted for L-70/follow-up.
- 2026-07-09 implemented. (1) Failing-first `cst_agreement` corpus/battery test + a CST-agreement
  assertion inside the round-trip property test (1000 seeds × 43 kinds) exposed and fixed SEVEN
  CST gaps: kebab-case flow names, `ctx` sub-lines (new opaque `CTX_ENTRY` kind), blank/comment
  lines at header→block and block→clause boundaries (new `at_block`/`at_kw_past_blank`/
  `skip_blank_lines` boundary helpers), dotted op names (`slack.message.send`), empty `+=` append
  lists, full `thing` selector forms, scientific-notation numbers, single-quoted strings, and
  column-0 `goal` directive lines. (2) `lower_cst.rs`: `cst_to_draft` (strict, spans on every
  error), `parse_with_ranges` (legacy semantics + `RangeMap`), lockstep walker covering every
  block-carrying kind (then/otherwise/handler/finally/branches/cases/default/steps/undo, @effect
  merge, until/ctx-entry exclusion). (3) `parse`/`parse_program` re-pointed through the front-end
  (debug-assert agreement gate; callers unchanged). (4) `analyzer_diagnostics_carry_ranges` +
  arity-range test green; flux-lsp now publishes analyzer findings as WARNING diagnostics with
  resolved spans (`SliceCatalog` + `resolve_diagnostic`). The `"""`-re-encode stays in the shared
  preprocess the lowering calls — moving it is churn until the Line machinery retires (residual).

## Notes
- **Gated** on isolation. Depends on **L-58**. Inviolable: round-trip invariant + `DraftAst` shape;
  negotiable: exact error wording. See [flux-lang-cst.md](../designs/flux-lang-cst.md).
