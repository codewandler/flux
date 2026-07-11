---
id: D-148
title: Sub-agents on the classic Client
pillar: Agent
status: done
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
- [x] Failing-first: a turn whose plan calls `task(role, …)` runs the child through the parent's
      envelope (mock role registry); child usage observation lands in the session's run trace.
- [x] Default wall-clock applied when `SpawnLimits` unset; overridable via
      `SubAgents::with_limits`.
- [x] `SubAgents`/`SpawnLimits`/`Role`/`RoleRegistry` nameable via `flux_sdk::subagents`.

## Progress
- **Done (unreleased).** `ClientBuilder::with_sub_agents(SubAgents)` added
  (`crates/flux-sdk/src/lib.rs`): stashes the bundle (applying the 10-min default `wall_clock` when
  unset, mirroring `FlowClient`); `build` registers `TaskTool` before the custom-name snapshot (so
  `task` rides the `tools`-subset re-admit) and threads `sub_agents.into_spawner(system)` into the
  dispatch `ToolContext` via `with_spawner` — no new dispatch path, same envelope as `FlowClient`.
- New `flux_sdk::subagents` re-export module: `SubAgents`, `SpawnLimits`, `Role`, `RoleRegistry`,
  `ProviderFactory`, `parse_role` (`SubAgentApprover` stays private in `flux-orchestrate`, not
  exported). `flux-orchestrate` is now a directly-named `use` in `flux-sdk` (already a dep).
- Failing-first test `with_sub_agents_runs_a_delegated_task_and_records_child_usage`
  (`crates/flux-sdk/src/lib.rs`): a `DelegatingMock` planner emits a `task(worker, …)` plan; a
  `WorkerMock` child bills 1000/200 tokens; asserts `out.tool_calls` contains `task` AND the parent
  turn's recorded usage (read via `event_store().turns`) folds in the child's spend — i.e. the child
  ran through the parent's envelope and its usage reached the session's run trace.
- CHANGELOG + WHATS-NEW updated (+ website mirror regenerated). Gate green (workspace
  test 2154 / clippy / fmt / codegate). **Not yet committed or released.**

## Notes
- Mirror `FlowClient::with_sub_agents` (`crates/flux-sdk/src/flow.rs:289`); TaskTool registered +
  `ToolContext::with_spawner` before `AgentSpec::assemble`. Depends on D-142/D-143.
