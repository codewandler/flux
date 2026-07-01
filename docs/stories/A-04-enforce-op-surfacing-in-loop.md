---
id: A-04
title: Enforce evidence-gated op surfacing in the self-hosted loop (bash escaped its opt-in)
pillar: Agent
status: done
note: FIXED — the engine computes the turn's surfaced set once (`surfaced_for_turn`) and hands it to the loop host; `compile_turn` rejects model-emitted plans naming hidden ops (unconditionally, even on the last repair step); pre-authored `flow run` stays unrestricted; live-verified (bash refused without opt-in, works with FLUX_ENABLE_BASH=1; catalog shrank 34.5k→32.5k ctx)
---

# Enforce evidence-gated op surfacing in the self-hosted loop (bash escaped its opt-in)

## Goal
Restore (and this time *enforce*) evidence-gated op surfacing on the real turn path. Since the
self-hosted loop cutover, `EngineLoopHost::plan` builds `OpRegistry::new(executor.registry())`
**without** `.with_advertised(…)` (`crates/flux-flow/src/loop_host.rs` ~380), so the
`FlowEngine::advertised_registry` gating (`crates/flux-flow/src/engine.rs:423`) only applies to the
`/plan` preview paths — the loop that actually runs every turn advertises the FULL catalog. Two
consequences, both observed live (2026-07-01, scratch python repo, no `.flux/` config, no
`FLUX_ENABLE_BASH`):

1. **Safety/docs violation:** the model planned and the runtime **executed** `bash("rm victim.txt")`
   under `--yes` (sessions s_310/s_311) — the `shell` group opt-in documented in the README and in
   `bash`'s own ToolSpec description gated nothing. Advertisement was never the only hole:
   `OpRegistry::get` is deliberately unfiltered (for pre-authored flows), so even a properly gated
   catalog doesn't stop a model that *names* a hidden op — the planner prompt itself teaches it that
   `bash` exists.
2. **Cost:** the unfiltered catalog (~all built-ins + toolchain ops) is the bulk of the ~34k-token
   fixed prompt every planner call pays (see A-03).

## Acceptance
- [x] Failing-first: a `run_turn` through the self-hosted loop in a workspace with **no** shell
      signal produces a planner catalog WITHOUT `bash`
      (`loop_host::tests::plan_advertises_only_the_turn_surfaced_ops` — a recording provider
      asserts `- bash(` is absent and `- read(` present).
- [x] Failing-first: a **model-emitted** plan naming a grouped, non-surfaced op is rejected at
      compile with a clear diagnostic and fed back for repair
      (`compile::tests::hidden_op_plan_is_rejected_and_repaired`), and — beyond the original
      criterion — rejected even on the FINAL repair step where ordinary diagnostics are tolerated
      (`hidden_op_plan_is_rejected_even_on_the_final_repair_step`): safety can't depend on the
      repair budget. Enforcement is `OpRegistry::hidden_ops_in` consulted by `compile_turn`
      (the registry's existing advertised-set seam, not a new mechanism).
- [x] Pre-authored flows stay unrestricted: `flow run` passes `advertised: None` explicitly, and
      `hidden_ops_in` returns nothing for an unrestricted registry
      (`hidden_ops_in_reports_only_registered_unadvertised_calls`); composites are never hidden.
- [x] `groups.active` is recorded once per turn on the loop path (`surfaced_for_turn` →
      `record_active_groups`), restoring the observation the cutover dropped. (Recorded per turn,
      not per iteration — signals are probed per turn by design, matching the pre-cutover engine.)
- [x] Live re-check: `flux run --yes -m aws "delete the file victim3.txt"` now reports shell is
      disabled and suggests `FLUX_ENABLE_BASH=1` — `bash` did not execute; with
      `FLUX_ENABLE_BASH=1` it is advertised and executes. Bonus: the gated catalog shrank the
      fixed prompt (ctx 34.5k → 32.5k in the scratch repo).

## Progress
- **DONE (2026-07-02).** `engine.rs`: extracted the shared `surfaced_op_names` computation (one
  source of truth for the preview registries AND the loop); `run_turn_cancellable` computes the
  turn's advertised set once, records `groups.active`, and hands it to
  `EngineLoopHost::set_turn(…, advertised)`. `loop_host.rs`: `TurnCtx.advertised` +
  `plan()` applies `.with_advertised(…)`. `registry.rs`: `hidden_ops_in(&[Node]) -> Vec<String>`
  (registered ∧ not composite ∧ not advertised). `compile.rs`: emit_plan branch rejects hidden-op
  plans unconditionally with an actionable diagnostic. CLI `flow run` passes `None` (pre-authored
  path unrestricted). 4 new tests; full gate green.

## Notes
- Found during the 2026-07-01 harness e2e review. Root cause pinned: `loop_host.rs` `plan()`
  (`OpRegistry::new(...).with_owned_composites(...)`, no advertised filter) vs `engine.rs`
  `advertised_registry` (correct, but only used by `plan_turn`/one-shot).
- `detect_signals` (`crates/flux-runtime/src/lib.rs:478`) is correct — `shell` only fires on
  `FLUX_ENABLE_BASH`. The signal layer is fine; the consumer forgot to apply it.
- Also observed: `pytest` was advertised in a repo with no `pyproject.toml`/`requirements.txt` —
  same cause (nothing is filtered).
