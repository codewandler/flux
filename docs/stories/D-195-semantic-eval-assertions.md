---
id: D-195
title: Semantic eval assertions — LLM-judge grading in the SDK test-kit
pillar: Agent
status: backlog
priority:
epic:
design:
note: "Scenario asserts on the canonical plan (exact, deterministic); add the complementary axis — rubric-based judge assertions for text outputs (assert_judge), with the judge call itself recorded/replayed through the cassette so CI stays $0 and hermetic after first record; bridges the Deterministic Agent Lab and flux-eval into a user-facing quality-regression harness"
---

# Semantic eval assertions — LLM-judge grading in the SDK test-kit

## Goal
`flux_sdk::test::Scenario` asserts on the canonical plan — exact and deterministic. Add the
complementary axis for *text* outputs: rubric-based judge assertions
(`assert_judge("answer cites the refund policy", rubric)`), with the judge's own model call
recorded and replayed through the cassette so `cargo test` stays hermetic and $0 after the first
recording. Embedders get a quality-regression harness for their agents, not just a determinism one.

## Acceptance
- [ ] `Scenario` (feature `test-kit`) gains a judge assertion: rubric + target output → graded
  verdict with a rationale surfaced on failure — failing-first test.
- [ ] The judge call flows through the cassette: first (record) run spends; replay runs construct
  no provider and spend nothing — asserted the same way the existing test-kit proves hermeticity.
- [ ] A changed agent output invalidates the recorded judge verdict loudly (re-record required),
  never silently passes against a stale grade.
- [ ] Judge model/config explicit per assertion or per scenario — no hidden default spend.
- [ ] Recipe documented beside the existing test-kit docs.

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

## Notes
- Builds directly on the Deterministic Agent Lab spine (D-174…D-180: cassette scopes, redacted
  fixtures) and borrows grading vocabulary from `flux-eval`.
