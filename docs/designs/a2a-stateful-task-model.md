# A2A stateful task model (Tier 3)

**Status:** implemented (A-54..A-57, 2026-07-08; suspension slice open) · **Pillar:** Agent · **Epic:** [a2a-conformance](a2a-conformance.md) ·
**Story:** [A-53](../stories/A-53-stateful-a2a-task-model.md)

This design answers the single architectural question that gates the whole task-management half of
the A2A spec, and fans it into implementation stories. A-54..A-57 are implemented (2026-07-08);
the `input-required`/`auth-required` suspension slice is the one part still open. One deviation
from the sketch below: the C-29 sweep protection generalized — every A2A run (blocking, streaming,
non-blocking) registers in the live-task registry, and the mint-time TTL sweep excludes live
tasks (`prune_inactive_excluding`), because non-blocking mints made mid-turn sweeps possible.

## The problem

flux runs an A2A request as **one synchronous turn and returns a `completed` `Task`** whose `id` is
the session id — a value no *later* call can meaningfully act on, because the run is already over and
nothing addressable survives it. `configuration.blocking` is ignored (every send blocks). That single
choice is why every method operating on a *retained* task is structurally absent:

- `tasks/get` — client-only; the server has no handler (falls through to `-32601`).
- `tasks/cancel` / `tasks/resubscribe` / `tasks/pushNotificationConfig/*` — no server code; answered
  `-32004 UnsupportedOperation` (A-50).
- non-terminal `TaskState`s (`submitted` / `working` mid-flight / `input-required` / `auth-required`)
  are modeled but never emitted.

To close it, an A2A `Task` must become a **first-class, addressable, potentially-async** object:
identity that outlives the turn, a lifecycle a background run advances, and out-of-band operations
(get / cancel / resubscribe / push) against that identity.

## Substrate we already have (so this is assembly, not invention)

- **An append-only event log with projections.** `events.db` already derives the conversation,
  run-trace, and turn-metrics as *folds* over stored events ([event-store
  unification](event-store-unification.md)). A **task-state projection** is the same pattern: fold the
  run/turn/message events of a stream into the current `TaskState` + `Task`. Retention is free — the
  events are already durable — and there is no second source of truth to keep in sync.
- **A realm key for tenancy.** A-48 + D-69 give every session an authenticated `realm` (`acct:`/`user:`
  disjoint), and lookups are realm-scoped (`find_correlated_in_realm`). Task identity reuses it, so a
  `task-id` from one tenant is structurally unreachable by another — the same isolation `contextId`
  continuity already has.
- **A cancellation primitive.** Streaming already cancels an in-flight turn when the SSE client
  disconnects, via a `CancellationToken` whose `drop_guard` fires on stream drop, propagating through
  `run_turn_cancellable`. `tasks/cancel` is *the same token*, fired by an out-of-band request instead
  of a socket close.
- **A single-turn gate + lazy TTL sweep.** The A2A surface already serializes turns behind
  `turn_gate` and prunes stale A2A sessions at mint time (C-18/C-29). Task retention/TTL rides the
  same mechanism (a task *is* a tagged stream).
- **A suspend/resume seam.** The engine can park a run awaiting input (the seam journeys use);
  `input-required` / `auth-required` are that seam surfaced as A2A task states.

## Identity & retention

- **`task-id` = the flux stream/session id**, exactly as today's synchronous `Task.id` already is —
  so the *blocking* fast path is unchanged and its returned id becomes retro-actively addressable.
- **`Task` is a projection** `task(events) -> Task`: fold a stream's events into `status.state`
  (from lifecycle/turn events), `history` (the A-52 conversation projection), and `artifacts` (A-52).
  No new stored row type; a completed task is reconstructable for as long as its stream is retained.
- **Retention/TTL** reuses the A2A session tag + lazy sweep (C-18). A terminal task survives until its
  stream is swept; `tasks/get` on a swept id is `-32001 TaskNotFound`.
- **Realm scoping** reuses the D-69 realm key: every task lookup is realm-scoped; cross-tenant access
  is a constant `-32001` (never a distinguishable "exists but forbidden").

## Async execution (the lifecycle change)

The spec default is **non-blocking**: `configuration.blocking` absent/`false` means "return now, run
in the background." Today flux ignores it. The change:

- **Blocking send (unchanged fast path).** `blocking: true` keeps today's behavior exactly — run to
  completion under the gate, return the `completed` Task (now with history/artifacts). *No regression;
  this is the compatibility guarantee.*
- **Non-blocking send.** Return `submitted` (or `working`) with the `task-id` immediately; spawn the
  turn on a background task (as streaming already spawns). The client advances via `tasks/get`
  (poll) or `tasks/resubscribe` (stream). The background run records its lifecycle to `events.db`, so
  the task-state projection reflects progress with no extra bookkeeping.
- **A small in-process registry** maps `task-id -> (CancellationToken, live-update broadcast)` for the
  *currently-running* tasks — the handle `cancel`/`resubscribe` need. Terminal/retained tasks are
  served purely from the projection (no registry entry required), so a server restart still answers
  `tasks/get` for finished tasks; only *live* cancel/resubscribe require the run to be in-process.

## Per-method plan

| Method | State | New code |
|---|---|---|
| `message/send` (blocking) | ✅ today | unchanged fast path |
| `message/send` (non-blocking) | new | return `submitted`+id, spawn run, project lifecycle |
| `tasks/get` | new | resolve id → task projection (realm-scoped); `-32001` if unknown |
| `tasks/cancel` | new | fire the registry `CancellationToken`; mark `canceled`; `-32002` if terminal |
| `tasks/resubscribe` | new | re-attach SSE: replay the task's status/artifact updates, then follow live |
| `input-required` / `auth-required` | new | surface the suspend/resume seam as task states + resume on next send |
| push notifications | new | per-task webhook config + delivery of status/artifact updates |

## Cancellation

Generalize the SSE drop-guard into a **realm-scoped registry of in-flight `CancellationToken`s**.
`tasks/cancel` looks up the token by `task-id` (within the caller's realm), fires it — the existing
`run_turn_cancellable` path already stops between plan rounds — and the projection moves to
`canceled`. A terminal task is `-32002 TaskNotCancelable`; an unknown/other-realm id is `-32001`.

## Suspension → `input-required` / `auth-required`

When a run parks on the engine's suspend/resume seam (needs more input, or an auth step), the task
projects as `input-required` / `auth-required` instead of `working`. The next `message/send` carrying
the same `taskId` **resumes** the parked run rather than starting a new turn — the first real use of
`Message.taskId` (today parsed but ignored). This is the richest slice and can trail the others.

## Push notifications (last, optional)

`tasks/pushNotificationConfig/{set,get,list,delete}` store a per-task webhook config (realm-scoped);
a delivery layer POSTs `TaskStatusUpdateEvent` / `TaskArtifactUpdateEvent` (A-52's shapers) to the URL
as the background run advances. Presupposes non-blocking execution + the task projection. Until built,
the methods stay `-32004`; when built, `capabilities.pushNotifications` flips to `true` and
`-32003 PushNotificationNotSupported` becomes the "configured off" answer.

## Compatibility & breaking surface

- The **blocking fast path is preserved bit-for-bit** — the whole point. Clients that send
  `blocking: true` (or that never poll) see no change beyond the already-shipped history/artifacts.
- **Likely breaking (minor bump):** `router`/`serve`/dispatch signatures gain a task-store/registry
  in their `State`, mirroring how D-69 threaded auth. Handler signatures change; the wire protocol for
  existing methods does not. Per the [SemVer rule](../../CHANGELOG.md) a breaking signature change is a
  **minor** (0.y) bump.
- New non-terminal `TaskState`s serialize only when a task actually reaches them, so existing
  decoders are unaffected.

## Implementation stories (filed from this design)

Sequenced; each is independently shippable and leaves the fast path intact.

- **[A-54](../stories/A-54-addressable-tasks-get-nonblocking.md)** — the foundation: task-state
  projection + retention, non-blocking `message/send`, and server-side `tasks/get` (`-32001` for
  unknown). Everything else builds on this.
- **[A-55](../stories/A-55-tasks-cancel.md)** — `tasks/cancel` via the realm-scoped in-flight
  `CancellationToken` registry (`-32002` for terminal tasks).
- **[A-56](../stories/A-56-tasks-resubscribe.md)** — `tasks/resubscribe`: re-attach an SSE stream to
  a live/retained task, replay then follow.
- **[A-57](../stories/A-57-a2a-push-notifications.md)** — push notifications: per-task webhook config
  + delivery; flips `capabilities.pushNotifications`.

`input-required`/`auth-required` (suspension) rides A-54's async model and is called out in that
story; it may split into its own story if the resume-on-`taskId` seam proves large.

## Non-goals (unchanged from the epic)

gRPC + A2A REST bindings; extensions negotiation; `tasks/list` (folds in here if ever pursued);
`FlowStore`/`ValueStore` Postgres impls. All artifacts stay protocol-focused — no downstream-consumer
names.
