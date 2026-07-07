---
id: A-53
title: Stateful A2A task model (design) — addressable async tasks for get/cancel/resubscribe/non-blocking/push
pillar: Agent
status: backlog
priority:
epic: a2a-conformance
design: docs/designs/a2a-conformance.md
note: "Tier-3 design-first: the whole task-management half of the spec depends on flux retaining an addressable async Task; produces its own design, then fans into impl stories"
---

# Stateful A2A task model (design)

## Goal
Decide, in a design doc, how flux moves from "one synchronous turn returning a `completed` Task" to a
**first-class, addressable, potentially-async A2A Task** — the single change that unblocks the entire
task-management half of the spec (`tasks/get` server-side, `tasks/cancel`, `tasks/resubscribe`,
non-blocking `message/send`, `input-required`/`auth-required` states, push notifications) — then fan
that design into implementation stories.

## Why (evidence)
- The server runs A2A requests synchronously and returns a `completed` `Task` with no id a later call
  can address (`crates/flux-server/src/a2a.rs:417-424`); `configuration.blocking` is ignored. So every
  method that operates on a *retained* task is structurally absent:
  - `tasks/get` is client-only (`flux-a2a/src/client.rs:191-206`); no server match → `-32601`.
  - `tasks/cancel`, `tasks/resubscribe`, `tasks/pushNotificationConfig/*` have zero server code.
  - non-terminal `TaskState`s (`submitted`/`input-required`/`auth-required`) are defined
    (`types.rs:155-162`) but never emitted.

## Acceptance
- [ ] A design doc `docs/designs/a2a-stateful-task-model.md` answering the Tier-3 questions in the epic:
      task retention & identity (projection over `events.db`, realm-scoped like A-48), async execution
      + polling, cancellation against an in-flight run (generalize the existing SSE-disconnect
      `CancellationToken`), suspension → `input-required`/`auth-required` on the engine's suspend/resume
      seam, and whether/when push notifications are in scope.
- [ ] The design must preserve the synchronous fast path clients use today (no regression) and call out
      any breaking handler-signature changes (likely a minor bump).
- [ ] Implementation stories filed from the design (not pre-split here): `tasks/get` server-side +
      non-blocking send, `tasks/cancel`, `tasks/resubscribe`, push notifications.

## Progress
- (not started — design-first; blocked on nothing, but larger than a quick-win so it waits behind
  Tier-1/2)

## Notes
- Reuses established substrate: `events.db` projections ([event-store-unification](../designs/event-store-unification.md)),
  the A-48 realm key, and the engine suspend/resume seam journeys already use.
- Epic: [a2a-conformance](../designs/a2a-conformance.md).
