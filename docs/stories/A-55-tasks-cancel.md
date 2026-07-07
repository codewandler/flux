---
id: A-55
title: tasks/cancel — cancel an in-flight task via the realm-scoped CancellationToken registry
pillar: Agent
status: backlog
epic: a2a-conformance
design: docs/designs/a2a-stateful-task-model.md
note: "Tier-3: generalizes the SSE-disconnect CancellationToken to an out-of-band signal; needs A-54"
---

# tasks/cancel

## Goal
Implement `tasks/cancel`: fire a retained, in-flight task's `CancellationToken` from an out-of-band
JSON-RPC request (not just an SSE disconnect), move the task to `canceled`, and answer
`-32002 TaskNotCancelable` when the task is already terminal.

## Why (evidence)
- flux already cancels a streaming turn on SSE disconnect via a `CancellationToken` `drop_guard`
  (`crates/flux-server/src/a2a.rs`, `run_turn_cancellable`); `tasks/cancel` is the same token fired by
  a request. Today the method has no server code and returns `-32004` (A-50).
- Depends on A-54's addressable tasks (a handle keyed by `task-id`).

## Acceptance
- [ ] A realm-scoped registry maps `task-id` → the in-flight run's `CancellationToken` (generalizing
      the SSE drop-guard).
- [ ] `tasks/cancel` (within the caller's realm) fires the token; the run stops between plan rounds
      and the task projects as `canceled`.
- [ ] A terminal task → `-32002 TaskNotCancelable`; an unknown/other-realm id → `-32001 TaskNotFound`.
- [ ] Failing-first tests: cancel of a running task transitions it to `canceled` and stops the run;
      cancel of a completed task → `-32002`; cancel of an unknown id → `-32001`.

## Notes
- Only *live* (in-process) tasks are cancelable; a retained-but-finished task is terminal by
  definition. Epic: [a2a-conformance](../designs/a2a-conformance.md).
