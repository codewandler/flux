---
id: A-59
title: Correlate direct `flow run` sub-agent children to the parent stream
pillar: Agent
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-016 (beta rec #3): direct `flux flow run` task() children get agent_id=subagent:<role> but correlation_id:null, so `replay --sub-agents` won't recurse; normal `flux run` agent turns already set correlation correctly — bring the direct flow-run path to parity"
---

# Correlate direct `flow run` sub-agent children to the parent stream

## Goal
A direct `flux flow run` can execute `task(...)` and the child sub-agent does run, but its stream is
recorded with `agent_id=subagent:<role>` and `correlation_id: null`. Because the child isn't
correlated to the parent session, `replay --sub-agents` cannot recurse into it. Normal `flux run`
agent turns already set correlation correctly — bring the direct `flow run` path to the same
contract so sub-agent replay works regardless of how the flow was launched.

## Why (evidence)
- Beta F-016: "Direct `flux flow run` can execute `task(...)`, but child streams had
  `agent_id=subagent:<role>` and `correlation_id:null`, so `replay --sub-agents` did not recurse.
  Normal `flux run` agent turns set correlation correctly."
- The contract already exists — [A-08](A-08-subagent-audit-default.md) makes child runs land
  correlated (`correlation_id = parent session`) for the CLI/app spawners. The direct `flow run`
  spawner just isn't setting it.

## Acceptance
- [ ] A `task(...)` executed under `flux flow run` records its child stream with
      `correlation_id = parent session id` (matching the A-08 contract that `flux run` satisfies).
- [ ] `replay --sub-agents` on a direct `flow run` recording recurses into the child stream(s).
- [ ] Failing-first test: run a flow with a `task(...)`, assert the child event stream carries the
      parent correlation id (not null), and assert `replay --sub-agents` visits the child.
- [ ] No regression to `flux run` agent-turn correlation (existing subagent.trace/replay tests stay green).

## Progress
- 2026-07-08 **DONE.** `execute_flow_with_composites` and `execute_flow_resumable_with_composites`
  (flux-flow) now install the run's session on the executor context
  (`executor.context().set_session(session_id)`), mirroring `FlowEngine::run_turn`. So a `task(...)`
  child spawned under a direct `flux flow run` reads `ctx.session_id()` and correlates
  (`correlation_id = parent session`) instead of recording `null`; `replay --sub-agents` recurses.
  In the agent loop the value was already set, so this is a no-op there. Placed the fix in flux-flow
  (not the CLI) so every direct caller gets parity and it is unit-testable. Test:
  `execute_flow_with_composites_sets_the_session_for_subagent_correlation` (flux-flow).

## Notes
- Beta rec order #3.
- Find where the direct `flow run` path constructs the sub-agent spawn context vs. where `flux run`
  does, and thread the parent session id into the child audit context there.
- Relevant epics: [time-machine](../designs/time-machine.md) (replay), and A-08 (correlation contract).
- Epic: [beta-hardening](../designs/beta-hardening.md).
