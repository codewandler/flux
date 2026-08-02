---
id: L-129
title: Add a project-classified multi-agent review example
pillar: Language
status: done
note: "classify repository evidence, derive exactly four review dimensions from only that classification, fan out, then synthesize a verdict"
---

# Add a project-classified multi-agent review example

## Goal

Ship a runnable `examples/review.flux` that demonstrates staged, project-adaptive review without
letting the dimension-selection model inspect or smuggle through the raw repository inventory.

## Acceptance

- [x] `review_flux_preserves_the_classification_only_boundary_and_bounded_fanout` proves the
      classifier consumes the directory inventory and Git history, while dimension derivation reads
      only the classifier result.
- [x] The flow derives exactly four structured review dimensions, runs four reviewer sub-agents in
      parallel, joins their reports, and returns a final verdict from a synthesis sub-agent.
- [x] The all-examples parse/lower sweep accepts the new example, and the examples index documents
      how to run it.

## Progress

- 2026-08-02: Started from user request recovered from Flux session `s_1732`; adding the structural
  contract failing-first before the flow.
- 2026-08-02: DONE. Added the staged flow plus read-only reviewer and toolless synthesizer roles.
  Failing-first proof: `review_example` failed because `examples/review.flux` did not exist, then
  passed with the classification-only seam and four-agent ceiling pinned. Green checks:
  `review_example`, `examples_validate`, `cst_agreement`, `canonical_corpus`, the customer-changelog
  website mirror, `flux-codegate`, and targeted `clippy -D warnings`.
- 2026-08-02: The later full workspace gate caught the root-example LSP census still pinned to 16;
  updated it to 17 and proved `review.flux` is LSP-clean across canonical, compact, JSON, and CST
  projections. Full workspace build/test/Clippy with warnings denied and `cargo fmt --all --check`
  are green.

## Notes

- `glob({ pattern: "*" })` is the built-in whole-workspace inventory and traverses through
  `flux-system`, which skips `.git`, `target`, and `node_modules` and caps results defensively.
- Four is intentionally authored rather than model-selected: it is enough independent coverage to
  be useful while keeping provider spend and delegated concurrency bounded and auditable.
