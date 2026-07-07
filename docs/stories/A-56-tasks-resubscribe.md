---
id: A-56
title: tasks/resubscribe — re-attach an SSE stream to a live or retained task
pillar: Agent
status: backlog
epic: a2a-conformance
design: docs/designs/a2a-stateful-task-model.md
note: "Tier-3: replay a task's status/artifact updates then follow live; needs A-54"
---

# tasks/resubscribe

## Goal
Implement `tasks/resubscribe`: open an SSE stream that replays a task's status/artifact updates from
its event stream and then follows the live run to its terminal event — so a client that started a
non-blocking task (or dropped a `message/stream`) can re-attach and observe it to completion.

## Why (evidence)
- No server code today (`-32004`, A-50). The frame shapers already exist:
  `flux_a2a::server::status_update_value` and `artifact_update_value` (A-52).
- Depends on A-54 (retained task + its event stream) and shares the live-update broadcast the async
  model introduces.

## Acceptance
- [ ] `tasks/resubscribe` for a retained `task-id` (within the caller's realm) returns an SSE stream
      that replays the task's prior status/artifact updates in order, then yields live updates until a
      terminal event, mirroring `message/stream`'s framing.
- [ ] A terminal task streams its final state and closes; an unknown/other-realm id → `-32001`.
- [ ] Failing-first tests: resubscribe to a running task observes ≥1 `working` frame then the terminal
      frame; resubscribe to a finished task yields the terminal frame and closes; unknown id → `-32001`.

## Notes
- Reuses the SSE plumbing (`Sse`, keep-alive, disconnect-cancels via the drop-guard) already in
  `message/stream`. Epic: [a2a-conformance](../designs/a2a-conformance.md).
