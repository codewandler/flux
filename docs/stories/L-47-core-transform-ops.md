---
id: L-47
title: Core transform ops — `map`, `filter.where`, `flatten`, `skip`, `join`, `split`
pillar: Language
status: done
priority: 3
epic: data-transforms
design: docs/designs/data-transforms.md
note: "closes the biggest anti-pattern — plans using `ai.extract` (\"Return ONLY a JSON array\") as a stand-in for deterministic map/filter"
---

# Core transform ops — `map`, `filter.where`, `flatten`, `skip`, `join`, `split`

## Goal
Ship the missing deterministic list transforms so plans stop prompting the model to reshape
data. `map` and `flatten` remove the `ai.extract`/`each -> flat` workarounds; the extended
`filter` accepts a real predicate (via the L-46 expr engine); `skip`/`join`/`split` fill in
the small-but-load-bearing gaps that force bespoke Rust ops today (`candidates_advance`,
manual join-into-fmt).

## Acceptance
- [x] New module `crates/flux-tools/src/transform.rs`. `flux-tools/Cargo.toml` adds
      `flux-lang.workspace = true`.
- [x] `map({items, path|expr, vars?})` — `path` plucks a dotted field per element
      (missing → `null`); `expr` evaluates a formula with `it` bound to the element and
      returns the scalar result. Exactly one of `path`/`expr` required.
      Failing-first tests: `map_path_plucks_dotted_field`, `map_expr_evaluates_it`.
- [x] `filter` extended: accepts `where` + optional `vars`, mutually exclusive with the
      existing `by`/`equals` mode. `by` also accepts dotted paths (e.g. `"author.username"`).
      Same dotted-`by` extension applied to `sort` and `dedupe`. Failing-first tests:
      `filter_where_keeps_matching_predicate`, `filter_where_and_by_mutually_exclusive`,
      `sort_and_dedupe_by_accept_dotted_paths`.
- [x] `flatten({items, depth?})` — flatten `depth` levels (default 1, cap 8; higher = 400).
      Non-array elements pass through; string elements that are JSON arrays re-parse
      (the C-10 rule `merge` already applies). Failing-first test:
      `flatten_depth_one_and_two`.
- [x] `skip({items, n})` — drop first `n`; `n <= 0` returns items unchanged;
      `n >= len` returns `[]`. Failing-first test: `skip_drops_first_n`.
- [x] `join({items, sep?})` (default sep `"\n"`) — strings passed as-is, non-strings
      compact-JSON; result is plain text, not JSON.
      Failing-first test: `join_stringifies_and_joins`.
- [x] `split({s, sep?, trim?})` (default sep `"\n"`, `trim` off) — result is a JSON array;
      `""` returns `[]`. Failing-first test: `split_returns_json_array`.
- [x] Legacy `filter` (bare + `by`+`equals` mode) truthiness aligned to the runtime table
      (`""`/`"false"`/`"0"` become falsey, matching everywhere else). CHANGELOG note that
      this could subtly reclassify elements whose kept-value was the string `"false"`.
- [x] All new ops registered in `flux-tools/src/lib.rs::register_cognition` and the
      `cognition` group in `flux-tools/src/groups.rs`; group description updated.
- [x] Predicate error message special-cases leading `.` and `$` with rewrite hints
      (*"element fields are `it.<field>`"* / *"symbols go in `vars`"*). Failing-first test:
      `where_hint_on_leading_dot_and_dollar`.
- [x] `website/docs/language/ops.md` cognition-tools table gains a row per new op with
      one native-text example.
- [x] CHANGELOG entry under `[Unreleased]`.

## Progress
- Done 2026-07-09: core transforms live in `flux_tools::transform`, are registered through the
  cognition pack, documented in ops tables, and covered by the named failing-first tests. Also fixed
  `validate_expr_formula` so dotted arithmetic predicates validate before dispatch.

## Notes
- Reuse `flux_lang::expr::validate_expr_formula` for predicate validation; keep runtime
  and analyzer paths consistent.
- Predicate ops set `format: "flux-expr"` on their `where`/`expr` schema fields — L-51
  will pick it up for literal-string analyzer validation.
- Existing helpers to reuse: `arr_or_empty`, `arr_param`, `parse_json_array_string`,
  `is_truthy` (or the aligned `ExprVal::truthy` if we align in this story).
- Depends on [L-46](L-46-expr-engine-module-and-list-builtins.md).
