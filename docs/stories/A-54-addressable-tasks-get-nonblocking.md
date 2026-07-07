---
id: A-54
title: Addressable tasks — task-state projection, non-blocking send, server-side tasks/get
pillar: Agent
status: backlog
epic: a2a-conformance
design: docs/designs/a2a-stateful-task-model.md
note: "Tier-3 foundation: everything else (cancel/resubscribe/push) builds on retained, addressable tasks"
---

# Addressable tasks — projection, non-blocking send, tasks/get

## Goal
Make an A2A `Task` addressable after the turn ends: fold a stream's events into a `Task` projection
(state + [A-52] history/artifacts), honor non-blocking `message/send` by running the turn in the
background and returning `submitted`/`working` immediately, and implement server-side `tasks/get`
(realm-scoped) so a client can poll a task to completion. This is the foundation the rest of Tier 3
builds on.

## Why (evidence)
- `crates/flux-server/src/a2a.rs` runs every send synchronously and returns a `completed` `Task`
  whose id no later call can act on; `configuration.blocking` is ignored.
- `tasks/get` is client-only (`crates/flux-a2a/src/client.rs`); the server has no handler.
- The substrate exists: `events.db` projections ([event-store-unification]), the D-69 realm key, the
  C-18 session tag + lazy TTL sweep. See [a2a-stateful-task-model](../designs/a2a-stateful-task-model.md).

## Acceptance
- [ ] A `task(events) -> Task` projection (state from lifecycle/turn events; history/artifacts from A-52),
      realm-scoped like the conversation projection.
- [ ] Non-blocking `message/send` (blocking absent/false) returns `submitted`/`working` + `task-id`
      immediately and runs the turn on a background task that records its lifecycle to `events.db`.
- [ ] The blocking fast path (`blocking: true`) is preserved bit-for-bit (no regression).
- [ ] Server-side `tasks/get` resolves `task-id` → current `Task` within the caller's realm;
      unknown/other-realm/swept id → `-32001 TaskNotFound`.
- [ ] Failing-first tests: a non-blocking send returns non-terminal immediately then `tasks/get`
      observes it reach `completed`; a blocking send is unchanged; `tasks/get` on an unknown id is
      `-32001`; a cross-realm `tasks/get` is `-32001` (not distinguishable from unknown).

## Notes
- `input-required`/`auth-required` (resume-on-`taskId` via the suspend/resume seam) rides this story's
  async model; split it out if the resume seam proves large.
- Likely a minor bump (router/serve `State` gains a task store). Epic:
  [a2a-conformance](../designs/a2a-conformance.md).
