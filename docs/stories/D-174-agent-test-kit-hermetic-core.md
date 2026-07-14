---
id: D-174
title: Deterministic Agent Test Kit — hermetic core
pillar: Agent
status: backlog
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
note: "Phase 1 — ships the headline on shipped primitives; no engine surgery"
---

# Deterministic Agent Test Kit — hermetic core

## Goal
Give SDK consumers a real, cheap, deterministic test suite for their agent: record a run once,
commit it as a redacted golden fixture, and re-run the *real* agent offline in `cargo test` for $0 —
asserting on the canonical Flux-Lang plan (not a transcript). Delivers the epic headline entirely on
already-shipped primitives (`replay_session`, `Session::fork`, `run_diff`).

## Acceptance
- [ ] Failing-first: `Scenario::record` on a `Storage::dir` client writes a `tests/scenarios/<name>/`
      fixture (events.db + flow.db); `Scenario::load(path).replay()` re-runs it hermetically under a
      **deny-all approver + never-called provider** (proves zero live dispatch, model never hit).
- [ ] `Outcome` assertions: `assert_plan_snapshot` (insta-style on canonical Flux-Lang),
      `assert_calls`, `assert_never_calls`, `assert_text_contains`, `assert_cost_under`,
      `assert_faithful`. A failing assertion renders a plan-source + world diff.
- [ ] Fault door: `Scenario::inject_at(node, error)` (over shipped `Fork::inject`+`diff`) →
      `Counterfactual::assert_compensated_with` / `assert_diverges_at`.
- [ ] `FLUX_GOLDEN=update` re-records against a live client and rewrites the fixture.
- [ ] Round-trip fidelity property test: capture→save→load→replay is faithful (event ordering /
      rowid-insertion-order value ids preserved).
- [ ] Truncated-cell fixtures report an actionable "re-record with larger FLUX_CASSETTE_MAX_BYTES"
      error; `assert_faithful` treats a truncated replay as a diagnostic, never a silent pass.
- [ ] Behind a default-off `test-kit` cargo feature; default build stays dependency-free.

## Progress
- (not started — epic deferred; docs-only for now)

## Notes
- New: `crates/flux-sdk/src/test.rs`, `crates/flux-sdk/tests/agent_test_kit.rs`, `test-kit` feature in
  `Cargo.toml`, `mod test` + re-exports in `lib.rs`; a plan-source unified-diff renderer over
  `RunDiff.rows` in `crates/flux-events/src/projection.rs`.
- Reuse: `flux_flow::replay::replay_session` + `ReplayReport`, `Session::fork`/`inject`/`diff`
  (`crates/flux-sdk/src/session.rs`), `flux_events::run_diff` + `DiffRow` (`projection.rs:573`),
  `Storage::dir`, `Session::cost`, `Redactor`, the plan parser (`parse(format(ast))==ast`).
- Redactor parity: pin the fixture's redactor config so the dual-hash matcher can't drift.
- No dependency on D-175 (this path is pure-replay). CLI surface is D-179.
