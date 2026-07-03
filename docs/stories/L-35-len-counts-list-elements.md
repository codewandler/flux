---
id: L-35
title: "len() on a list must count elements, not characters of the joined JSON string"
pillar: Language
status: done
priority: 2
epic: flux-lang-evolution
note: "s_362: `$count = len($rs_files)` on a glob list returned 8542 (char count of the stringified array), not 232 (elements) — ExprVal has no List variant, so from_json stringifies arrays; the model confabulated an explanation around the wrong number"
---

# len() on a list must count elements

## Goal
Fix a silent-wrong-answer footgun in the expression evaluator. `ExprVal` (flux-lang runtime,
~:3402) has only `Num/Str/Bool`; `ExprVal::from_json` (~:3446) stringifies a JSON array into its
compact JSON, and `len` (~:3840) counts `chars()` of that string — so `len(glob("**/*.rs"))`
returned 8542 (characters) instead of 232 (paths) in s_362, and the model papered over the
contradiction with a confabulated answer. Add an `ExprVal::List` variant (from_json arm before the
catch-all; arms in `as_num`/`as_text`/`truthy`) and make `len` return element count for lists,
char count for strings (unchanged).

## Acceptance
- [x] Failing-first test in flux-lang's runtime tests: `len` over a bound glob-style JSON array
      returns the element count (mirror `len_counts_arrays_and_strings` in
      flux-tools/src/cognition.rs:1282, which pins the cognition-op side).
- [x] `len("hello") == 5` unchanged; `truthy`/`as_text` behavior for lists pinned.
- [x] Existing expr/`jq` tests pass unchanged.

## Progress
- 2026-07-03 filed from s_362 forensics (read-only agent produced the line-anchored patch shape;
  see the session report in the tracker).
- 2026-07-03 implemented: added `ExprVal::List(Vec<serde_json::Value>)` (flux-lang/src/runtime.rs)
  with arms in `as_num` (`None` — lists don't participate in arithmetic), `as_text` (compact JSON),
  `truthy` (non-empty), and `from_json` (an `Array` arm before the catch-all). `len`'s
  `expr_call_fn` arm now matches on `ExprVal::List` for element count, falling through to the
  existing `as_text().chars().count()` for everything else (strings unchanged). Root-caused a second
  layer beyond the missing variant: op results are stored as JSON *strings* (the same quirk `jq`'s
  `jq_parse_input` already re-parses for), so a symbol bound from e.g. `glob(...)` arrives at
  `resolve_expr_vars` as a `Value::String` holding array text, not a native array — `resolve_expr_vars`
  now runs `jq_parse_input` before `ExprVal::from_json` so a string-stored array/object unwraps to
  its native JSON shape before typing. Also added the required `ExprVal::List` arm to
  `eval_template`'s `Node::Expr` conversion (`J::Array(items)`) for match-exhaustiveness. New
  failing-first tests: `expr_len_counts_list_elements_not_stringified_chars` and
  `expr_list_truthy_and_as_text` (pure `ExprVal`-level, mirroring `ev()`-style tests) plus the
  end-to-end regression `expr_len_over_a_bound_op_result_counts_array_elements_not_chars` (binds `$files`
  from an `echo` op call carrying `["a.rs","b.rs","c.rs"]` as its JSON-string content, then
  `len(files)` via a native `Node::Expr` — failed before the fix with `left: "22" (stringified-JSON
  chars) right: "3" (elements)`, i.e. the exact s_362 shape; green after). `len('hello') == 5` and all
  pre-existing `expr_*`/`jq_*` tests pass unchanged. Gate green: `cargo test -p flux-lang -p
  flux-tools` (219 + 77 tests), `cargo clippy -p flux-lang -p flux-tools --all-targets -- -D
  warnings` clean, `cargo fmt -p flux-lang -p flux-tools --check` clean, `cargo test --workspace`
  green (no other test relied on the old char-count behavior).
