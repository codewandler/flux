---
id: L-51
title: Native expr in conditions and bind RHS — `when $count > 3`, `$ok = $score >= 0.8`
pillar: Language
status: done
priority: 7
epic: data-transforms
design: docs/designs/data-transforms.md
note: "the ergonomic pass: authors can finally write `when $x > 3` instead of `@json {\"kind\":\"expr\",...}`; the runtime seam already exists — this is pure parser/format work"
---

# Native expr in conditions and bind RHS

## Goal
Let native flux-lang text finally express comparisons and boolean logic directly in
`when`/`unless`/`until`/`assert` conditions and bind RHS positions. The `Expr` node
already exists and executes correctly; today's `@json`-only spelling is a documentation
scar (P6/P8-era shipping order). This story adds the parser fallback and the invertible
formatter rule so the sugar round-trips.

## Acceptance
- [x] Parser: in the four condition positions (`when`, `unless`, `until`, `assert`) and
      in bind RHS position, when the ordinary parse leaves a comparison/boolean-operator
      tail, re-lex the whole expression with the expr tokenizer extended to accept
      `$name` idents, and emit an `Expr` node with auto-built `vars: {name: $name}`.
      Dotted `$x.field` stays a dotted read on var `x` (needs L-46's dotted-access).
      Failing-first tests: `parse_when_native_comparison`,
      `parse_until_native_call_predicate` (`until len($queue) == 0`),
      `parse_bind_rhs_native_expr`, `parse_assert_native_expr_with_message`.
- [x] Formatter: `format` renders `Expr` natively **only when the lowering is invertible**
      — every formula variable maps to `{name: $name}` or a dotted read (`$name.a.b`) with
      no additional wrapping. Otherwise keep the `@json` spelling. Property test:
      `roundtrip_native_expr_preserves_json` over a corpus of hand-authored + emitted
      formulas.
- [x] Analyzer: op params whose schema carries `"format": "flux-expr"` (the `where` /
      `expr` params on the L-47/L-48 ops) get literal-string validation via
      `validate_expr_formula` at analysis time. A malformed literal predicate is
      **rejected before dispatch** with the expr validator's diagnostic. Failing-first
      test: `analyzer_rejects_bad_literal_flux_expr_predicate`.
- [x] No grammar ambiguity: any operator tail in a condition/bind-RHS position was an
      error path before this story — proven by an "if this test breaks, the fallback rule
      collides with an existing valid parse" corpus scan test.
- [x] SSOT regen: `syntax.md` gets a native-conditions section; `flows-and-syntax.md` /
      `control-flow.md` snippet updates; `UPDATE=1` on the three drift-guard tests
      produces zero further diff. Round-trip lock: `format(parse(text)) == text` for the
      new native forms.
- [x] CHANGELOG entry under `[Unreleased]`.

## Progress
- Parser and formatter support for native conditions/bind RHS was already present and is covered by
  `parse_when_native_comparison`, `parse_until_native_call_predicate`,
  `parse_bind_rhs_native_expr`, `parse_assert_native_expr_with_message`, and
  `roundtrip_native_expr_preserves_json`.
- Added the schema-driven analyzer hook for literal `format: "flux-expr"` parameters, plus
  `analyzer_rejects_bad_literal_flux_expr_predicate` and a positive sibling-`vars` case.
- Updated syntax/reference/website docs and the `[Unreleased]` changelog.

## Notes
- Deferred: native `|>` pipe. See design doc.
- Parser sites (starting points):
  - Conditions: `parse.rs` around `parse_when`/`parse_unless`/`parse_until`/`parse_assert`
    (near :1111).
  - Bind RHS: the `parse_full_expr` call from the bind grammar.
  - Field-access sugar precedent to mirror: `$plan.kind` at `parse.rs:1718` — same
    "sugar only when it round-trips" rule.
- Depends on [L-46](L-46-expr-engine-module-and-list-builtins.md). L-47/L-48 are useful
  but not blocking (this story can land empty of `where` predicates in text and still
  provide the `when $x > 3` value).
