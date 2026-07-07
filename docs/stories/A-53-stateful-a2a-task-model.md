---
id: A-53
title: Stateful A2A task model (design) — addressable async tasks for get/cancel/resubscribe/non-blocking/push
pillar: Agent
status: done
epic: a2a-conformance
design: docs/designs/a2a-stateful-task-model.md
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
- **Done (design-first): the deliverable is the design + the filed impl stories, not code.**
- Wrote [`docs/designs/a2a-stateful-task-model.md`](../designs/a2a-stateful-task-model.md), which
  answers the Tier-3 questions: **identity** (`task-id` = the flux stream id, already what today's
  synchronous `Task.id` is, so the blocking id becomes retro-actively addressable); **retention** (a
  `task(events) -> Task` projection over `events.db`, reusing the A-52 history/artifacts folds and the
  C-18 session tag + lazy TTL sweep); **realm scoping** (the D-69 realm key — cross-tenant access is a
  constant `-32001`); **async execution** (non-blocking send returns `submitted`+id and runs on a
  background task, poll via `tasks/get` / stream via `tasks/resubscribe`); **cancellation** (generalize
  the SSE-disconnect `CancellationToken` into a realm-scoped in-flight registry); **suspension**
  (`input-required`/`auth-required` = the engine suspend/resume seam surfaced as task states, resumed
  on the next send carrying the same `taskId`); and **push** (per-task webhook config + delivery, last
  and optional).
- **Fast-path guarantee:** the design preserves `blocking: true` bit-for-bit (no regression) and calls
  out the likely breaking surface — `router`/`serve`/dispatch `State` gains a task store (a **minor**
  bump per the SemVer rule); the wire protocol for existing methods is unchanged.
- **Impl stories filed** (backlog, sequenced, each independently shippable):
  [A-54](A-54-addressable-tasks-get-nonblocking.md) (foundation: projection + non-blocking send +
  server-side `tasks/get`), [A-55](A-55-tasks-cancel.md) (`tasks/cancel`),
  [A-56](A-56-tasks-resubscribe.md) (`tasks/resubscribe`), [A-57](A-57-a2a-push-notifications.md)
  (push notifications). `input-required`/`auth-required` rides A-54.
- No implementation in this story — Tier-3 execution is the A-54..A-57 fan-out, greenlit separately.

## Notes
- Reuses established substrate: `events.db` projections ([event-store-unification](../designs/event-store-unification.md)),
  the A-48 realm key, and the engine suspend/resume seam journeys already use.
- Epic: [a2a-conformance](../designs/a2a-conformance.md).
