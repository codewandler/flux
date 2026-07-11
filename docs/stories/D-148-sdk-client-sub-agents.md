---
id: D-148
title: Sub-agents on the classic Client
pillar: Agent
status: ready
priority: 7
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 2 — with_sub_agents parity; cancellation now reaches the task tool"
---

# Sub-agents on the classic Client

## Goal
`ClientBuilder::with_sub_agents(SubAgents)` — the `task` tool + spawner on the conversational
door, wired pre-assemble with the same 10-minute default wall clock as `FlowClient`, and with
cancellation now reachable through `Session::stream().cancel()`.

## Acceptance
- [ ] Failing-first: a turn whose plan calls `task(role, …)` runs the child through the parent's
      envelope (mock role registry); child usage observation lands in the session's run trace.
- [ ] Default wall-clock applied when `SpawnLimits` unset; overridable via
      `SubAgents::with_limits`.
- [ ] `SubAgents`/`SpawnLimits`/`Role`/`RoleRegistry` nameable via `flux_sdk::subagents`.

## Progress
- (pending)

## Notes
- Mirror `FlowClient::with_sub_agents` (`crates/flux-sdk/src/flow.rs:289`); TaskTool registered +
  `ToolContext::with_spawner` before `AgentSpec::assemble`. Depends on D-142/D-143.
