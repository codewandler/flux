---
id: C-593
title: "Stop narrowing an authored ai_segment ceiling with the intent-router family cap"
pillar: Core
status: done
areas: [flux-flow]
note: "MAX_FAMILIES bounds what the model may route to; an operator-authored tools: ceiling has no model choice to bound"
---

# Stop narrowing an authored `ai_segment` ceiling with the intent-router family cap

## Goal

Let an operator-authored `ai_segment` run with the exact tool ceiling its author declared. The
adaptive intent router's four-family cap exists to keep a *model-chosen* capability selection narrow;
applying it to a ceiling the operator already fixed makes legitimate authored loops unrunnable.

## Acceptance

- [x] Failing first, `scoped_authored_ceiling_admits_more_families_than_the_intent_router_cap`
      (`crates/flux-flow/src/staged.rs`) proves a six-family selection is rejected for a
      model-declared routing selection and admitted for an authored ceiling.
- [x] `scoped_authored_ceiling_still_obeys_the_operation_budget` proves the exemption is scoped to
      the family cap alone: `MAX_NATIVE_TOOLS` and the schema-character budget still reject an
      oversized authored ceiling.
- [x] `IntentDeclaration::scoped` defaults to `false` under serde, so an adaptive state serialized by
      an older runtime resumes capped and `selected_specs_for_state` keeps its fail-closed posture.
- [x] `cargo test -p codewandler-flux-flow` is green (286 tests).

## Progress

- Implemented. `scoped_segment_state` marks the synthesized declaration `scoped: true`;
  `selected_specs` skips only the `MAX_FAMILIES` check for such a declaration.

## Notes

- Found in production, not in review. Fleet wave-253 dispatched four story workers on the authored
  `.flux/fleet/loops/story-implementation.flux` profile and **all four** failed identically before
  any tool ran:

  ```
  step `ai_segment` failed: adaptive capability declaration selected 6 distinct families;
  the maximum is 4
  ```

  That loop names a read/write/git/shell/datasource/system ceiling — six families — so no worker
  could ever start. The error also misattributed the failure to an "adaptive capability
  declaration" when the declaration was synthesized from an authored ceiling.
- The cap's intent is explicit in `INTENT_SYSTEM`: "Select only the smallest capability families
  needed." That instruction addresses the router model. `scoped_segment_state` documents the other
  half — "The authored segment already names its exact tool ceiling, so every discoverable family
  inside that ceiling is selected deterministically" — and then ran that deterministic union through
  the router's bound.
- Follow-up: [C-594](C-594-fleet-run-dry-run-builds-a-workspace-on-an-uncreated-worktree.md) covers
  the `flux fleet run --dry-run` failure found while diagnosing this.
