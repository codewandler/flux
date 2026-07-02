---
id: A-10
title: Per-turn token budget ceiling — an enforced spend bound on the agent loop
pillar: Agent
status: ready
priority: 8
note: usage is accounted per call but nothing enforces a ceiling — the only runaway bounds are 25 iterations × up to 8 compile steps; a budget node caps op-dispatch count in a scope, never the loop's model spend
---

# Per-turn token budget ceiling

## Goal
Give the loop an enforced spend bound. Verified 2026-07-02: per-call usage is captured
(`engine.rs:578-603`, `CallUsage`) and the loop host tallies it (`loop_host.rs:172,429`), but
nothing enforces a ceiling — a pathological turn is bounded only by the 25-iteration cap times
up-to-8 provider calls per `plan()` (compile repairs). The `budget` node caps op-dispatch count
within a plan scope (`flux-lang runtime.rs:2175-2183`), not model spend.

## Acceptance
- [ ] **Failing-first:** `turn_ends_honestly_when_token_budget_exhausted` (flux-flow) — a mock
      provider whose first call reports large usage + emits a plan, budget below that: the turn's
      answer names the budget and the provider is called exactly once more (the check preempts
      round 2); fails today (runs to plan exhaustion).
- [ ] Unit: **tokens** (total across this turn's planner calls, the tally already in
      `EngineLoopHost.usage`). Not USD in v1 — unpriceable models would make a USD ceiling
      silently unenforced; USD rides the same seam later.
- [ ] Enforcement at the top of `plan()` right after the `force_stop` check, mirroring the
      stall-stop pattern: return an honest `{kind:"chat"}` budget-exceeded answer (persists as the
      assistant message) + a `turn.budget_exceeded` observation (durable via C-14).
- [ ] Config surface & precedence: `--turn-budget <tokens>` > `FLUX_TURN_TOKEN_BUDGET` >
      `.flux/config.toml` `[limits] turn_token_budget`. **Default OFF** (iteration cap + stall
      guards already bound normal turns; a default ceiling would break long legitimate turns).
      Threaded via `AgentSpec.turn_token_budget` → `FlowEngine::assemble` → loop host field.
- [ ] Full gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P8 of the round).

## Notes
- Documented limitation: sub-agent spend reaches the parent only at turn end (via observation), so
  it is not counted mid-turn; the planner-call loop is the runaway vector this targets.
- A budget smaller than one planner call ends every turn after round 1 — acceptable (user-set),
  and the message says so.
