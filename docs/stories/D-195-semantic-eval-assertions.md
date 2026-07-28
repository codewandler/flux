---
id: D-195
title: Semantic eval assertions — LLM-judge grading in the SDK test-kit
pillar: Agent
status: done
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
- 2026-07-28: Implemented end-to-end on `main` (uncommitted).
  - `flux_sdk::test::Rubric` (explicit `provider/model`, `#[non_exhaustive]`) and `Verdict`
    (`passed` + `rationale`, `assert_pass()`) added to `crates/flux-sdk/src/test.rs`.
  - `Scenario::judge(client, criterion, target, &rubric) -> Result<Verdict>` and the panicking
    `Scenario::assert_judge(...) -> Verdict` convenience. The judge's own model call is built
    deterministically (fixed system prompt + `Criterion:`/`Text under test:` template), hashed via
    the existing `redact_and_hash_request`, and looked up against a new `judge.jsonl` fixture file
    (same `ModelCallRecord` shape as `model.jsonl`, reused as-is; accumulates additively since many
    distinct judge assertions can share one fixture — see the new module-doc section and the
    fixture-format bullet).
  - Cassette contract: a hash HIT is served from `judge.jsonl` without ever touching
    `client`'s provider (proven with a `NeverProvider` that panics if invoked — same proof idiom
    `replay` uses). A hash MISS is a hard `Err` naming `FLUX_GOLDEN=update` unless that env var is
    set, in which case the call is made live against `client`'s real provider (spends once) and
    committed. A changed `target`/`criterion`/`rubric.model` changes the hash, so a stale verdict
    is never silently reused (content-addressed staleness, no separate check needed).
  - Failing-first: `crates/flux-sdk/tests/judge_assertions.rs` was written first against the
    not-yet-existing API (the crate failed to *compile*, the correct failing-first shape for a
    brand-new surface) and passes now with 4 tests covering: no-hidden-spend-on-miss, record-once/
    replay-free-from-cassette (+ the committed record's `model` field proving the explicit
    rubric model was used, not a default), the staleness/re-record contract on a changed target,
    and the rationale surfacing on a FAIL verdict via `Verdict::assert_pass`.
  - Docs: `website/docs/sdk/agent-lab.md` gained a "Judge assertions — grading text output"
    subsection under Test (with a runnable-shaped example + the `FLUX_GOLDEN=update` recipe), a
    `judge.jsonl` row in the fixture-format table, and the "Limits (v1)" bullet was reworded (it
    used to claim "no LLM-judged scoring" outright; now scoped to "not a corpus-wide eval harness").
  - Gate: `cargo test -p codewandler-flux-sdk` (default features) and
    `cargo test -p codewandler-flux-sdk --features test-kit` both green (incl. the new file, 4/4,
    and all pre-existing `agent_test_kit.rs`/`agent_golden.rs`/`whatif.rs` suites unaffected).
    `cargo clippy -p codewandler-flux-sdk [--features test-kit] --tests -- -D warnings` clean.
    `cargo fmt` clean on the touched files (whole-workspace `fmt --check` was not run/applied —
    other uncommitted sessions share this tree and its diffs are not mine to fix or format).
  - Left as-is per explicit instruction: status kept `in-progress` and the Acceptance checkboxes
    below are intentionally left unchecked pending the coordinating session's own close-out pass
    (no CHANGELOG entry added yet for the same reason).

## Notes
- Builds directly on the Deterministic Agent Lab spine (D-174…D-180: cassette scopes, redacted
  fixtures) and borrows grading vocabulary from `flux-eval`.
