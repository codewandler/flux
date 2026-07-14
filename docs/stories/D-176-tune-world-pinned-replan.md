---
id: D-176
title: Tune — world-pinned re-plan and counterfactual A/B
pillar: Agent
status: backlog
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
- [ ] `Session::what_if()` builder: `.turn`, `.model`, `.system_prompt`, `.substitute`,
      `.substitute_at`, `.off_tape(Halt|Live)`, `.run() -> Counterfactual`.
- [ ] `Counterfactual`: `session()`, `diff()` (vs original via `run_diff`), `first_divergence()`,
      `hermetic()`, `cost()`.
- [ ] Failing-first: a pure `.substitute(op, output)` re-runs the identical plan with no model call;
      `diff()` shows only the intended `DiffRow::Output` and `hermetic()==true`.
- [ ] Honesty guard: a `model`/`system_prompt` variant that re-plans and reads a different input
      reports `hermetic()==false` and localizes the divergence — never a faked complete diff.
- [ ] `Scenario::check(&Client)` (Test Kit Engine 2) installs `Frozen(golden_tape)` on
      `run_turn_pinned` and returns a classified `Report { diff, plan_changed, left_world }`.
- [ ] `Client::what_if_over(sessions, WhatIfSpec) -> SweepReport` (per-session diff, %-of-corpus
      changed, total offline spend).

## Progress
- (not started — epic deferred; docs-only for now)

## Notes
- Depends on D-175 (`Frozen` scope, `run_turn_pinned`, model cassette). Policy mode is split out to
  D-177 (needs the authorize-only executor entry).
- Reuse: `replay_prefix` (`fork.rs`), `switch_model_for_session` (`engine.rs:349`),
  `assemble_with_loop` (`engine.rs:248`), correlated-session mint + `record_fork_plan`, `run_diff`.
- New: the model-cassette `Provider` decorator (record/serve keyed by a canonicalized, redacted
  request hash via `sha256_hex`), `WhatIf`/`Counterfactual`/`OffTape`/`Divergence`/`SweepReport`.
