---
id: A-01
title: Unify SDK onto FlowEngine, retire the classic Agent loop
pillar: Agent
status: done
priority: 1
design: docs/designs/flux-flow.md
---

# Unify SDK onto FlowEngine, retire the classic Agent loop

## Goal
Two turn loops coexisted: the pure-DAG `FlowEngine` (CLI/TUI/server) and the classic provider-native
`flux-agent::Agent`. Unify everything onto `FlowEngine` so there is **one loop everywhere**, then
delete `flux-agent::Agent`. Honors "no fallbacks / clean cutover" and removes a silent divergence.

## Acceptance
- [x] `flux_sdk::Client` runs on `FlowEngine` (rebuilt via `AgentSpec`), with the `TurnOutput` API
      preserved (test green). `FlowClient` (the declarative door) unchanged — two front-ends, one engine.
- [x] `flux-agent::Agent` **removed** (struct + `run_turn`/`run_turn_cancellable` + loop deleted). The
      `AgentSink` trait moved to `flux-flow`; the `flux-codegate` layering lint stays green.
- [x] The **second** classic-loop consumer — `flux-orchestrate::LocalSpawner` (sub-agent spawner behind
      `task`) — migrated to `FlowEngine` too (sub-agents now run the audited flux-lang loop).
- [x] SDK examples updated (`client_basic` runs on the unified engine, prose path).

## Progress
- Done. `flux-agent` is repurposed from "the old loop" into the **Agent pillar**: it owns `AgentSpec`
  (model, persona, skills, tool selection, permissions, settings) + `assemble`/`into_engine`
  (→ `FlowEngine`), keeps `DEFAULT_SYSTEM_PROMPT`, and absorbs the markdown `Role` format (moved from
  `flux-orchestrate`); it now depends on `flux-flow`. `AgentSink` lives in `flux-flow` (the engine
  crate). SDK `Client`, the orchestrate spawner, and the CLI all assemble their engine via `AgentSpec`.
  Gate green (`check`/`clippy`/`fmt`/`test` incl. codegate layering).

## Notes
- Scope grew beyond the original SDK-only framing: introduced a first-class agent-definition type
  (`AgentSpec`) — the home for "what an agent is" (model/prompt/skills/tools/settings) — and unified
  the three ad-hoc engine-assembly sites (CLI/SDK/orchestrate) onto it.
- Known limitation (deferred): token usage is not surfaced through the unified flux-lang loop
  (`turn_end(None)`), so `TurnOutput.usage` is `None` — consistent with the CLI's `--usage` no-op. A
  follow-up could thread usage from the planner through the loop, benefiting every surface.
- Files: `crates/flux-agent/src/{lib.rs,role.rs}`, `crates/flux-flow/src/agent_sink.rs`,
  `crates/flux-sdk/src/lib.rs`, `crates/flux-orchestrate/src/lib.rs`, `crates/flux-cli/src/main.rs`.
