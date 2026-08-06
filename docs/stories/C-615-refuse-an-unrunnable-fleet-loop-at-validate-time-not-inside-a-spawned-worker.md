---
id: C-615
title: "Refuse an unrunnable Fleet loop at validate time, not inside a spawned worker"
pillar: "Core"
status: done
epic: fleet-harness-throughput
areas: [flux-cli, flux-flow]
done_override: "Implemented and tested in main: AgentLoopBinding::validate_analysis runs the same analysis pass engine assembly runs, with an assumed-operations registry; test analysis_refuses_an_unbound_match_subject_that_runtime_validation_accepts. Four waves died on a static loop defect before this existed."
---

# Refuse an unrunnable Fleet loop at validate time, not inside a spawned worker

## Goal

Make `flux fleet validate` prove that every loop a Fleet policy can select is actually runnable, so a
static defect in an authored loop costs a second on the operator's terminal instead of a wave.

## Acceptance

- [x] `AgentLoopBinding::validate_analysis` runs the same `analyze_flow` pass engine assembly runs,
      without constructing an engine or a provider.
- [x] `fleet validate` resolves every `loop_policy` binding, analyzes it, and reconciles it against
      the ceiling of each `agent_templates` entry bound to that task kind.
- [x] A loop naming a toolchain operation the assigned repository will not surface is refused, named
      per repository.
- [x] Failing first, a test proves `validate_runtime` accepts a source that `validate_analysis`
      refuses — the gap itself.
- [x] `CAPABILITY_BUNDLES` is pinned to the domain of `capability_operations` by a drift test.

## Notes

**The defect this closes.** `fleet validate` asserted only that `.flux/fleet.toml` existed. The real
analysis lived behind `FlowEngine::new`, so an authored loop carrying a purely static error was first
refused *inside an already-spawned worker*. The coordinator reports a child's stderr, and that
diagnostic arrives below the sandbox note and a two-screen plugin-coherence warning — so it surfaced
as `transient-worker: agent … emitted an invalid event stream` and the actual message was never read.

Four waves were spent on statically detectable errors in one loop:

| wave | real cause | how it presented |
| --- | --- | --- |
| 289 | `match $check.result` — a field access, not a bound symbol | `invalid event stream` |
| 293 | same | `invalid event stream` |
| 296 | `npm` granted by the `node` bundle but never installed in a Rust workspace | mid-turn ceiling error |
| 299 | segment exhausted its round budget and the error escaped `repeat` | turn failed, 6 files uncommitted |

Waves 289/293 also produced a **false attribution**: the shared error string was blamed on an
unrelated sandbox change, which was reverted for nothing. Wave 286 had failed the same way *before*
that change existed, and wave 293 failed the same way *after* the revert.

**Two mechanisms, deliberately separate.**

- Static analysis catches structural defects and unknown operations. It runs against built-ins + the
  dev pack + the authored-loop control plane, plus the *granted ceiling* as assumed operations
  (`OpRegistry::with_assumed_ops`). The assumption is required, not a shortcut: the datasource pack
  (`search`/`get`/`sources`/`batch_get`/`relation`/`list`) is only registered once an agent is
  assembled with configured sources, so no static registry can hold it.
- Toolchain surfacing is a filesystem question, checked per repository against the root marker
  (`node`→`package.json`, `python`→`pyproject.toml`/`requirements.txt`, `go`→`go.mod`,
  `make`→`Makefile`).

**Residual gap.** Assumed operations carry no parameter or type information, so argument checks on
them degrade to permissive. A wrong argument *name* on a datasource op still reaches runtime. Closing
that needs a static op-spec catalogue independent of assembly, which is a larger change than this
story.

- Related: [C-603](C-603-bound-story-work-by-checkpoints-not-one-round-budget.md) — the checkpoint
  loop this validates; wave-299 proved its recovery path was missing a `try`.
