---
id: D-174
title: Deterministic Agent Test Kit — hermetic core
pillar: Agent
status: done
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
- [x] Failing-first: `Scenario::record` on a client writes a `tests/scenarios/<name>/`
      fixture (events.db + flow.db); `Scenario::load(path).replay(&client)` re-runs it hermetically
      under a **deny-all approver + never-called provider** (proves zero live dispatch, model never
      hit — pinned by `replay_is_hermetic_under_deny_all_and_never_provider`, a counting op whose
      counter never moves).
- [x] `Outcome` assertions: `assert_plan_snapshot` (insta-style on canonical Flux-Lang, hand-rolled
      unified line diff, `FLUX_GOLDEN=update` rewrites), `assert_calls`, `assert_never_calls`,
      `assert_text_contains`, `assert_cost_under`, `assert_faithful` (+ non-panicking `faithful()`).
      A failing assertion renders the canonical plan source (+ divergence detail when present).
- [x] Fault door: `Scenario::inject_at(&client, node, value)` (over shipped `Fork::inject`+`diff`,
      via `flux_sdk::whatif::Counterfactual`) → `assert_compensated_with` / `assert_diverges_at`.
      Signature takes `&Client` (deviation from the design doc's sketch, justified in the plan:
      `replay`/`inject_at` need the executor's op catalog, and the deny-all/never-provider posture
      IS client config).
- [x] `FLUX_GOLDEN=update` re-records against a live client and rewrites the fixture (refuses to
      overwrite otherwise).
- [x] Round-trip fidelity property test: capture→save→load→replay is faithful across 3 differently
      shaped flows (`capture_save_load_replay_round_trip_is_faithful_across_flows`); ordering/turn-id
      remap covered directly in `flux-events` (`copy_session_to_reproduces_every_projection`,
      `copy_session_to_remaps_turn_ids_to_the_destination_session`).
- [x] Truncated-cell fixtures report an actionable "re-record with larger FLUX_CASSETTE_MAX_BYTES"
      error; `assert_faithful`/`faithful()` treat a truncated replay as a diagnostic, never a silent
      pass (`truncated_cell_reports_an_actionable_error`).
- [x] Behind a default-off `test-kit` cargo feature (`test-kit = ["dep:toml"]`); default build stays
      dependency-free (verified: `cargo build -p codewandler-flux-sdk` with no features pulls no
      `toml`, and `cargo clippy --all-targets` is clean on both feature configurations).

## Progress
- **Done** (2026-07-27). Shared foundations landed in `flux-events`/`flux-core` first:
  `EventStore::copy_session_to` (+ crate-private `EventBackend::append_at` on both the sqlite and
  postgres arms), `stmt_texts`/`DiffLineKind`/`render_run_diff` in `projection.rs` (hoisted from the
  CLI's `run_diff_cmd`), and a `Serialize`/`Deserialize` derive on `flux_core::Chunk` (adjacently
  tagged) with a round-trip test over every variant. `flux-sdk` gained crate-private
  `assembly::EngineAssembly`/`VariantOverrides` (retained at `ClientBuilder::build`, threaded onto
  `Client`/`Session`/`Fork`) and the un-gated `whatif::Counterfactual` module, then the `test-kit`
  feature + `crates/flux-sdk/src/test.rs` (`Scenario`, `Outcome`, `Manifest`, the redacting
  `RecordingProvider`, `ModelCallRecord`/`canonical_request`/`redact_and_hash_request` kept
  `pub(crate)` for D-176 reuse) and `crates/flux-sdk/tests/agent_test_kit.rs` (9 tests, all green).
  Gate: `cargo build`/`test`/`clippy -D warnings`/`fmt --all` all green on `codewandler-flux-sdk`
  (default + `test-kit`), `codewandler-flux-events`, `codewandler-flux-core`.
  **Incident + recovery**: mid-implementation, a concurrent session's `git merge` of the L-78 branch
  wiped this story's entire uncommitted working tree (caught, not caused, by this agent) — fully
  recovered from the auto-safeguard stash (`git stash list` → "safeguard pre-L78 merge D-174 work");
  no work lost, verified byte-identical after restore. `flux-flow` (D-175, a concurrent session) was
  deliberately left untouched per scope; it also transiently broke mid-session (`mod resurrect;`
  before the file existed) and self-resolved.
  Deviation from the plan file: the integration test uses `#![cfg(feature = "test-kit")]` (matching
  `tests/plugins.rs`'s existing convention) rather than a `[[test]] required-features` Cargo.toml
  stanza — same effect, follows the crate's established pattern.

## Notes
- New: `crates/flux-sdk/src/test.rs`, `crates/flux-sdk/tests/agent_test_kit.rs`, `test-kit` feature in
  `Cargo.toml`, `mod test` + re-exports in `lib.rs`; a plan-source unified-diff renderer over
  `RunDiff.rows` in `crates/flux-events/src/projection.rs`.
- Reuse: `flux_flow::replay::replay_session` + `ReplayReport`, `Session::fork`/`inject`/`diff`
  (`crates/flux-sdk/src/session.rs`), `flux_events::run_diff` + `DiffRow` (`projection.rs:573`),
  `Storage::dir`, `Session::cost`, `Redactor`, the plan parser (`parse(format(ast))==ast`).
- Redactor parity: pin the fixture's redactor config so the dual-hash matcher can't drift.
- No dependency on D-175 (this path is pure-replay). CLI surface is D-179.
