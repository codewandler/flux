---
id: L-43
title: Text scalar binds must preserve boolean/number types (stop stringifying)
pillar: Language
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-008: bare native-text binds `$n = 1` / `$ok = false` become strings, which then breaks `match` and structured output; scalar literals in text bind position should keep their JSON scalar type"
---

# Text scalar binds must preserve boolean/number types

## Goal
In native Flux-Lang text, bare scalar binds like `$n = 1` and `$ok = false` are bound as **strings**
(`"1"`, `"false"`) rather than the number/boolean scalars they look like. Downstream that silently
breaks `match` arms and structured output that expect the typed value. A scalar literal in text bind
position should carry its JSON scalar type.

## Why (evidence)
- Beta F-008: "Bare text binds like `$n = 1` and `$ok = false` become strings in ways that affect
  `match` and structured output."

## Acceptance
- [ ] `$n = 1` binds the number `1`, `$x = 1.5` the number `1.5`, `$ok = false`/`$ok = true` the
      boolean, and `$s = "1"` still the string `"1"` (explicit quotes = string).
- [ ] `match` over a numeric/boolean bind matches the typed arms (the failing case from the report);
      structured output carries the scalar type.
- [ ] Failing-first parser/lowering test round-tripping `$n = 1` / `$ok = false` to typed JSON
      scalars, plus a `match` test that fails on today's stringified behavior.
- [ ] Decide + document the boundary: which text positions coerce to string (if any) vs. preserve
      scalars, so the rule is predictable (e.g. template leaves vs. bind RHS).

## Progress
- 2026-07-08 **DONE.** Root cause was runtime, not parse (the parser already types scalars): the
  `eval_pure_node` `Lit` arm stored every literal as `Value::String`. Fixed it to bind scalar
  **number/bool** literals as their natural JSON type; strings, `null`, and object/array literals keep
  the canonical JSON-as-string form (op-arg marshaling + the null→`""` truthiness idiom depend on it).
  Also made `Value::to_json` render an integral f64 as a JSON integer (`5`, not `5.0`) — this fixes
  both display and `match` equality (an integral `Number` compares equal to a literal `PosInt` arm).
  Boundary documented in the story/code. Tests:
  `scalar_binds_keep_their_json_type_for_match_and_output`; updated `value_to_json_is_natural` /
  `value_from_json_round_trips` to the new (correct) integral-canonicalization convention.

## Notes
- Grounds against the text parser / bind lowering in flux-lang; compare with the object/list
  value-template scalar handling (which already distinguishes literals).
- Epic: [beta-hardening](../designs/beta-hardening.md).
