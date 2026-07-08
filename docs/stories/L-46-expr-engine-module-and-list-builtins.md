---
id: L-46
title: Extract `flux_lang::expr` module + list-aware builtins + dotted variable access
pillar: Language
status: ready
priority: 2
epic: data-transforms
design: docs/designs/data-transforms.md
note: "epic foundation: extract the expr engine so ops (L-47..L-50) can reuse the same predicate language the runtime and analyzer already share"
---

# Extract `flux_lang::expr` module + list-aware builtins + dotted variable access

## Goal
Extract the existing `Expr` engine from `crates/flux-lang/src/runtime.rs` into a **public**
`flux_lang::expr` module so `flux-tools` can reuse the exact evaluator (and its validator)
for op-side predicates. Add dotted variable access, an `ExprVal::Obj` variant so objects
stop stringifying, and the list-aware builtins the transform ops need. Foundation for the
whole `data-transforms` epic — everything else composes on top of one predicate
mini-language.

## Acceptance
- [ ] New public `crates/flux-lang/src/expr.rs` module carrying `Tok`, `tokenize_expr`,
      `expr_call_fn`, `is_known_expr_fn`, `validate_expr_formula`, `eval_expr_value`,
      `ExprVal` (with a new `Obj(serde_json::Map<String, Value>)` variant) — re-exported
      from the crate root as `flux_lang::expr`.
- [ ] `runtime.rs` and `analyze.rs` re-import from the new module; **all existing
      `flux-lang`/`flux-flow`/`flux-eval` tests still pass byte-identically**.
- [ ] Dotted identifier access — a formula referencing `it.a.b` (or any declared var with
      a dotted suffix) descends objects jq-style; missing keys / null / non-object hops
      resolve to `Str("")` (matching today's null mapping). Failing-first test:
      `expr_dotted_access_descends_objects_and_nulls_missing`.
- [ ] New expr builtins (registered in `expr_call_fn` **and** `is_known_expr_fn`):
      `sum(xs)`, `any(xs)`, `all(xs)`, `has(xs, v)`, `join(xs, sep)`, `split(s, sep)`
      returning `List`, `first(xs)`, `last(xs)`, plus single-`List`-arg overloads of
      `min(xs)` / `max(xs)`. `len(obj)` counts keys. Failing-first tests:
      `expr_list_builtins_semantics` and `expr_split_returns_list_val`.
- [ ] `ExprVal::Obj` truthy iff non-empty, `as_text` renders compact JSON, `len(obj)`
      returns key count. Failing-first test: `expr_val_obj_truthiness_and_render`.
- [ ] Analyzer's `validate_expr_formula` accepts the new builtins automatically (shared
      whitelist); an unknown function still errors with the existing diagnostic.
- [ ] SSOT regenerated: `Node::Expr` doc-comment reflects the extended surface; `UPDATE=1`
      run of `skill_in_sync` / `website_in_sync` / `skill_docs_in_sync` produces zero
      further diff.
- [ ] CHANGELOG entry under `[Unreleased]`.

## Progress
- (implementation not yet started)

## Notes
- Extraction is a pure re-locate; do not change semantics of existing builtins.
- Layer check verified: `flux-lang` (L0) has no `flux-tools` dep. Nothing else in
  `flux-lang` depends on `flux-tools`. Adding this module doesn't move the layer graph.
- The one non-trivial semantics extension is the `Obj` variant: today `from_json` on an
  object degrades to `Str(json_string)`, which means `len(obj_var)` returns character
  count. That is technically a behavior change — call it out in the CHANGELOG note.
- Follow the L-35 fix pattern (list length semantics) — same shape of change.
- Related: [flux-lang-evolution.md](../designs/flux-lang-evolution.md) §5.1 (transforms as
  pure ops, not nodes).
