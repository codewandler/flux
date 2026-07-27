---
id: D-176
title: Tune — world-pinned re-plan and counterfactual A/B
pillar: Agent
status: done
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
note: "Phase 3 — Session::what_if + Scenario::check; depends on D-175"
---

# Tune — world-pinned re-plan and counterfactual A/B

## Goal
Re-run a recorded session under exactly one changed variable (model / prompt / a substituted tool
output) with the rest of the world byte-frozen, so the resulting `run_diff` is a pure causal readout
— the "should I ship this change?" regression gate, offline and ~$0 for pure substitutions.

## Acceptance
- [x] `Session::what_if()` builder: `.turn`, `.model`, `.system_prompt`, `.substitute`,
      `.substitute_at`, `.off_tape(Halt|Live)`, `.run() -> Counterfactual`.
- [x] `Counterfactual`: `session()`, `diff()` (vs original via `run_diff`), `first_divergence()`,
      `hermetic()`, `cost()`.
- [x] Failing-first: a pure `.substitute(op, output)` re-runs the identical plan with no model call;
      `diff()` shows only the intended `DiffRow::Output` and `hermetic()==true`
      (`pure_substitution_is_hermetic_and_makes_zero_model_calls` proves ZERO further `stream()`
      calls on the SAME provider `Arc` the recording used; `pure_substitution_costs_zero` prices it
      at `$0`; `counterfactual_session_is_itself_replayable` proves the result is a real session).
- [x] Honesty guard: a `model`/`system_prompt` variant that re-plans and reads a different input
      reports `hermetic()==false` and localizes the divergence — never a faked complete diff
      (`system_prompt_replan_is_non_hermetic_with_a_localized_divergence`,
      `off_tape_halt_latches_loudly_on_the_replan_path`).
- [x] `Scenario::check(&Client)` (Test Kit Engine 2) installs `Frozen(golden_tape)` on
      `run_turn_pinned` and returns a classified `Report { diff, plan_changed, left_world }` — plus
      `model_served`/`model_live` (the golden `model.jsonl` is pinned by a `ServingProvider`; a
      request it doesn't cover falls through live and is COUNTED, never silently served) and
      `is_clean()`/`render()`.
- [x] `Client::what_if_over(sessions, WhatIfSpec) -> SweepReport` (per-session diff, %-of-corpus
      changed, total offline spend); a session that can't be opened is an isolated `Err` row.

## Progress
- **Done** (2026-07-28). New `crates/flux-flow/src/whatif.rs` holds the two rerun drivers neither
  existing driver could serve (`replay_session` writes to a throwaway scratch store; `replay_prefix`
  goes live at the cut): `rerun_pinned` (re-executes a recorded session's accepted plans into a
  correlated destination session entirely under a caller-pinned scope — no model call reachable at
  all) and `replay_turns_prefix` (hermetically rebuilds the turns before a re-plan target, erroring
  loudly if the prefix itself can't replay faithfully). `crates/flux-sdk/src/whatif.rs` gained
  `WhatIf`/`WhatIfSpec`/`SweepOutcome`/`SweepReport` on top of D-174's `Counterfactual` (which grew
  `from_sessions` + the honest `hermetic` flag), and `crates/flux-sdk/src/test.rs` gained
  `ServingProvider` + `Scenario::check`/`Report`. 6 new tests in
  `crates/flux-sdk/tests/whatif.rs`, 2 in `crates/flux-flow/src/whatif.rs`, 2 added to
  `crates/flux-sdk/tests/agent_test_kit.rs`.
- **Two defects found by the failing-first tests, both fixed at the root rather than papered over:**
  1. `flux_events::run_diff` aligns on the executed-statement ledger, which a **natively dispatched**
     turn never writes (the adaptive loop dispatches directly and records the equivalent Flux-Lang
     program as replay metadata only — `flux_flow::staged::record_host_flow`). So a recorded SDK
     session had *zero* statement rows while its interpreter-executed rerun had three, and the diff
     read as a wholesale plan rewrite; two natively dispatched runs vacuously compared "identical".
     `run_diff` now falls BOTH sides back to their flat dispatch sequence (`op` + `input_hash` as
     content identity) when either side has no statement ledger. Also fixes `flux diff` for SDK
     sessions.
  2. Neither `Frozen` nor `Replay` re-records a SERVED dispatch (by design — nothing ran live), so a
     fully-served re-drive recorded no cells at all and diffed as "every statement vanished".
     `rerun_pinned` already solved this with a self-recording sink; that sink
     (`flux_flow::whatif::RerunRecordingSink`) is now public and `Scenario::check` — which drives
     `run_turn_pinned` directly, not `rerun_pinned` — reuses it instead of duplicating it.
- Gate green in both workspaces (build/test/clippy `-D warnings`/fmt) plus `flux-codegate`, and
  `codewandler-flux-sdk` clippy clean on both the default and `test-kit` feature configurations.

## Notes
- Depends on D-175 (`Frozen` scope, `run_turn_pinned`, model cassette). Policy mode is split out to
  D-177 (needs the authorize-only executor entry).
- Reuse: `replay_prefix` (`fork.rs`), `switch_model_for_session` (`engine.rs:349`),
  `assemble_with_loop` (`engine.rs:248`), correlated-session mint + `record_fork_plan`, `run_diff`.
- New: the model-cassette `Provider` decorator (record/serve keyed by a canonicalized, redacted
  request hash via `sha256_hex`), `WhatIf`/`Counterfactual`/`OffTape`/`Divergence`/`SweepReport`.
