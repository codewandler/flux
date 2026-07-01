---
id: A-04
title: Enforce evidence-gated op surfacing in the self-hosted loop (bash escaped its opt-in)
pillar: Agent
status: ready
priority: 1
note: the loop-host planner builds an UNGATED OpRegistry — every op (incl. `bash`) is advertised every turn, and a model-named hidden op resolves + executes; README's "bash is opt-in" is currently false on the main path
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
- [ ] Failing-first: a `run_turn` through the self-hosted loop in a workspace with **no** shell
      signal produces a planner catalog WITHOUT `bash` (assert on the ops the loop-host `plan` op
      hands `compile_turn` — e.g. a test seam exposing the advertised set, or a mock provider that
      records the system prompt and asserts `- bash(` is absent).
- [ ] Failing-first: a **model-emitted** plan naming a grouped, non-surfaced op (e.g. `bash` with
      shell off) is rejected at analysis with a clear diagnostic ("op `bash` is not enabled: opt in
      via `enable_shell`…") and fed back to the model — it must NOT dispatch. Reuse the L-11
      capability-scoping seam rather than a new mechanism.
- [ ] Pre-authored flows (`flux flow run <file>`, composites, the agent-loop itself) still resolve
      hidden-group ops — the enforcement applies to model-emitted plans only.
- [ ] `groups.active` observation is recorded per loop iteration (it silently disappeared with the
      cutover too — `record_active_groups` is only called from the preview path).
- [ ] Live re-check: `flux run --yes "delete the file x.txt"` in a fresh scratch repo either uses a
      dedicated op or reports that shell is disabled — `bash` does not execute.

## Progress
- (not started)

## Notes
- Found during the 2026-07-01 harness e2e review. Root cause pinned: `loop_host.rs` `plan()`
  (`OpRegistry::new(...).with_owned_composites(...)`, no advertised filter) vs `engine.rs`
  `advertised_registry` (correct, but only used by `plan_turn`/one-shot).
- `detect_signals` (`crates/flux-runtime/src/lib.rs:478`) is correct — `shell` only fires on
  `FLUX_ENABLE_BASH`. The signal layer is fine; the consumer forgot to apply it.
- Also observed: `pytest` was advertised in a repo with no `pyproject.toml`/`requirements.txt` —
  same cause (nothing is filtered).
