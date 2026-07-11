---
id: D-158
title: FlowClient streaming — execute_with_sink + execute_streamed
pillar: Agent
status: backlog
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 4 — flow runs stop being observability-blind (ExecSink drops everything but names)"
---

# FlowClient streaming — execute_with_sink + execute_streamed

## Goal
Flow executions become observable: `FlowClient::execute_with_sink(ast, sink)` streams every
dispatch to a consumer `AgentSink`; `execute_streamed(ast)` is the owned-`AgentEvent` variant
(same machinery as D-145).

## Acceptance
- [ ] Failing-first: the sink receives `tool_result` for every dispatched op in an `execute`
      (today's `ExecSink` records only names, `crates/flux-sdk/src/flow.rs:613-622`).
- [ ] `execute_streamed` yields events live during a slow mock op.
- [ ] `execute`/`execute_with` behavior unchanged (existing tests green).

## Progress
- (pending)

## Notes
- Depends on D-145 (AgentEvent). Seeding paths (`execute_with`) keep their fresh-store isolation.
