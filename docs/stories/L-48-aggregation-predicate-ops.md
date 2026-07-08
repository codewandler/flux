---
id: L-48
title: Aggregation & predicate ops — `sum`, `count_by`, `group_by`, `any`, `all`, `has`
pillar: Language
status: ready
priority: 4
epic: data-transforms
design: docs/designs/data-transforms.md
note: "kills the bespoke Rust boolean-emitters (candidates_empty/score_compare/grade) that only exist because expr has no text spelling — `any`/`all`/`has` compose with `when`/`until` today"
---

# Aggregation & predicate ops — `sum`, `count_by`, `group_by`, `any`, `all`, `has`

## Goal
Ship the missing aggregations plus the three deterministic boolean-emitters (`any`, `all`,
`has`) that plans can drop into `when`/`until` conditions *today*, replacing the family of
bespoke Rust ops (`candidates_empty`, `score_compare`, `grade`) that only exist because
`expr` has no native text spelling. A generic `reduce` is deliberately not shipped — see
design note.

## Acceptance
- [ ] `sum({items, path?})` — numeric sum; if `path` set, sum that dotted field per
      element; non-numeric element (or missing field required by `path`) → clear error
      naming the offender. Failing-first tests: `sum_of_numbers`,
      `sum_with_path_and_bad_element_errors`.
- [ ] `count_by({items, path})` — `[{key, count}]`, sorted count desc, key asc tiebreak,
      deterministic. Failing-first test: `count_by_orders_count_desc_then_key`.
- [ ] `group_by({items, path})` — `[{key, items}]`, first-seen key order (matches
      `dedupe`'s convention). Failing-first test:
      `group_by_first_seen_key_order`.
- [ ] `any({items, where?, vars?})` — `"true"` iff some element satisfies `where` (or
      is truthy when `where` omitted). Empty list → `"false"`. Failing-first test:
      `any_true_on_match_false_on_empty`.
- [ ] `all({items, where?, vars?})` — `"true"` iff every element satisfies. **Vacuously
      `"true"` on empty** — documented on the op description and reflected in the
      failing-first test `all_vacuously_true_on_empty_list`.
- [ ] `has({items, value})` — JSON-equality membership → `"true"`/`"false"`. Failing-first
      test: `has_equality_membership`.
- [ ] **Conformance tests pin op ↔ expr-builtin identical outputs** for `sum`/`any`/`all`/
      `has` (and `join`/`split`/`first`/`last`/`len` shipped in L-46/L-47). Test:
      `op_expr_builtin_conformance_matrix`.
- [ ] Ops usable directly in `when`/`until`/`assert` (call-cond position — no parser work
      needed). End-to-end test: `any_in_when_gates_a_flow_step`.
- [ ] All new ops registered in `register_cognition` and the `cognition` group; group
      description updated.
- [ ] `website/docs/language/ops.md` cognition-tools table gains a row per new op with
      one native-text example.
- [ ] CHANGELOG entry under `[Unreleased]`.

## Progress
- (implementation not yet started)

## Notes
- Generic `reduce{items, formula, init}` is deliberately **rejected** — see design doc
  "The core decision" and "Rejected alternatives". Every observed reduce need (aggregation
  reports, boolean short-circuits) is expressible as one of the six ops above.
- `"true"`/`"false"` string returns are the shipped convention for the analyzer's
  call-condition rule (`analyze.rs:1004`); they parse falsey/truthy via the runtime table
  in `when`/`until` positions.
- Depends on [L-46](L-46-expr-engine-module-and-list-builtins.md). Same crate/host wiring
  as [L-47](L-47-core-transform-ops.md).
