---
id: C-570
title: "Agents report typed progress and cooperatively yield upstream"
pillar: Core
status: ready
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-runtime, flux-flow, flux-orchestrate, flux-cli]
depends_on: [C-569]
note: "SpawnActivity is host telemetry; add durable child-authored reports, acknowledgement and a resumable safe-checkpoint yield"
priority: 10
---

# Report progress without claiming authority

## Goal

Let a long-running agent tell its parent or Fleet main what phase it reached, what needs attention and
where it can safely yield, using one bounded durable protocol rather than stdout or final prose.

## Acceptance

- [ ] Failing first, a blocking native `task` child emits observations but cannot produce a durable,
      acknowledged “red test established” progress record or yield a resumable cursor. The fixed
      child reports while its parent continues to await the terminal result.
- [ ] `AgentReport` has stable report/sequence, agent/session/parent/assignment/loop identities,
      phase/state, bounded redacted summary, optional completed/total units, evidence references,
      attention request and current budget projection. Persistence returns an acknowledgement/event
      id and is idempotent across retry/restart.
- [ ] Reports cannot transition a Board item, mark work done, change Fleet membership, assert
      unverified evidence or widen capability/budget. Host projections derive operational state from
      admitted state plus verified receipts.
- [ ] `candidate_ready` means implementation is frozen for C-572/C-587 review and reflection;
      `handoff_ready` is host-derived only after both mandatory receipts settle successfully. A
      worker cannot report itself past that barrier.
- [ ] Native `task` transports reports through the existing correlated child boundary without
      exposing thinking/tool-result content. Fleet and C-552 TaskAgentBackend map the same schema to
      their event streams; `SpawnActivity` remains compatible host telemetry.
- [ ] A loop may yield only at a declared safe checkpoint. Yield settles current usage and returns a
      typed partial terminal plus durable cursor; resume preserves exact assignment, session, loop
      binding, capabilities and budget. It is neither cancellation nor operator pause.
- [ ] Oversized, secret-bearing, duplicate, out-of-order and wrong-assignment reports fail closed or
      are safely elided with bounded diagnostics. Progress/report delivery never backpressures an
      agent indefinitely.
- [ ] Hermetic tests cover nested children, five concurrent workers, report-before-review, attention,
      cooperative yield, acknowledged resume and parent cancellation.

## Progress

- (not started)

## Notes

- A-140/A-141 own operator-initiated pause and in-flight effects. This story owns a worker choosing
  a checkpoint already declared by its loop.
- A-79's `SpawnActivity` is reused for correlation but not widened into an unauthenticated status
  mutation path.
