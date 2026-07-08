---
id: L-44
title: Make `parse` composable in object-template leaves and direct returns
pillar: Language
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-012: `parse` works in bind position but is rejected in object-template leaves and direct returns — other pure nodes compose there, so `parse` is inconsistently restricted"
---

# Make `parse` composable in object-template leaves and direct returns

## Goal
`parse` works in bind position but is rejected as an object-template leaf and as a direct return
value. Other pure nodes compose in those positions, so `parse` is inconsistently restricted — either
allow it everywhere a pure node is valid, or document the restriction as deliberate with a clear
diagnostic.

## Why (evidence)
- Beta F-012: "`parse` works in bind position but was rejected in object-template leaves and direct
  returns."

## Acceptance
- [ ] `parse(...)` is accepted as an object-template leaf (`{ data: parse($x) }`) and as a direct
      return, consistent with other pure nodes — **or**, if intentionally restricted, the validator
      emits a specific diagnostic saying so (not a generic rejection).
- [ ] Failing-first test covering `parse` in an object-template leaf and a direct return (currently
      rejected), asserting the chosen behavior.
- [ ] The language docs' composability rules reflect the outcome (ties into the F-007 doc work in
      [C-46](C-46-beta-docs-truth-pass.md)).

## Progress
- 2026-07-08 **DONE.** Chose to allow `parse` everywhere a pure node is valid (it *is* pure). Two
  sites: added `Node::Parse` to the analyzer's template-leaf whitelist (`check_template_leaf`) plus a
  `Parse` arm in the runtime `eval_template` (recovers the coerced value's natural JSON type); and made
  `eval_return` delegate all non-`call`/`var` nodes to `eval_pure_node`, so `parse` (and `jq`/`expr`/
  `fmt`) compose as direct returns instead of erroring "unsupported return expression". Tests:
  `analyze_accepts_parse_as_a_template_leaf` + `parse_composes_as_a_direct_return`.

## Notes
- Ground against the value-template validation in flux-lang and the pure-node whitelist for template
  leaves/returns; align with [A-37](A-37-parse-enforcement.md) (parse enforcement).
- Epic: [beta-hardening](../designs/beta-hardening.md).
