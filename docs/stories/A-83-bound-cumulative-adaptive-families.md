---
id: A-83
title: Bound cumulative adaptive capability families
pillar: Agent
status: done
design: docs/designs/cumulative-adaptive-family-ceiling.md
note: "A later signal could append a fifth family even though intent and each signal were individually capped at four, deferring failure to schema expansion."
---

# Bound cumulative adaptive capability families

## Goal

Keep semantic capability expansion inside one durable four-family ceiling for the complete adaptive
turn, so repeated signals cannot create an oversized provider catalog after intent was accepted.

## Acceptance

- [x] Failing-first: `semantic_capability_signal_rejects_fifth_cumulative_family_before_expansion`
      reproduces an intent with four active families followed by a one-family signal; before the fix
      Flux accepted the fifth family and returned its operation.
- [x] `signal_capabilities` validates the deduplicated accumulated family union and rejects a fifth
      family before expanding operation schemas or mutating resumable adaptive state.
- [x] Re-signalling an active family does not consume another slot, and valid later expansion within
      the four-family ceiling retains its existing behavior.
- [x] The operation-count and schema-character ceilings remain independent and continue to apply to
      the exact selected family union.
- [x] The focused cumulative-limit and existing semantic-expansion tests pass.

## Progress

- 2026-07-14 — downstream catalog-budget review found that the declaration and each signal payload
  were bounded separately while the durable union was not. The failing-first regression returned
  `Ok(["fixture_4.inspect"])` before the union check was added.
- 2026-07-14 — the accumulated-union check and focused regressions are green.

## Notes

- This is a visibility bound, not an authorization rule. Registry, permission, authored-tool,
  approval, and guarded-IO ceilings remain unchanged.
- The provider-facing signal schema still bounds one payload to four entries; its description now
  also states that the complete active set is capped at four.
