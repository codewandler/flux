---
id: A-129
title: Bound delivery concurrency — the mpsc capacity was the only backpressure
pillar: Agent
status: in-progress
priority: 32
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-app]
note: "filed from A-112's implementor report — concurrency was the point of A-112, but it removed the one thing that bounded it"
---

# Bound delivery concurrency — the mpsc capacity was the only backpressure

## Goal
A-112 made `App::deliver` concurrent by spawning each dequeued `DeliveryMessage` into a `JoinSet`
instead of processing it inline. That was the story's whole point — but as its implementor reported,
the supervisor's `mpsc` `CAPACITY` bound was the **only** backpressure in the system, and a loop that
dequeues instantly no longer applies it. A webhook storm can now spawn unboundedly.

This matters most for exactly the workload A-112 exists to enable: a fleet coordinator taking
inbound Jira webhooks while a sweep runs.

## Acceptance
- [x] A bound on in-flight deliveries, configurable, with a documented default.
- [x] Failing-first test: N deliveries submitted at once with a limit of K run at most K
      concurrently, and the remainder still complete — bounded, not dropped.
- [x] The bound does not reintroduce head-of-line blocking between unrelated channels: a slow sweep
      must not starve webhook intake, which is A-112's Acceptance and must keep passing.
- [x] Backpressure is observable — a delivery waiting on the bound is distinguishable from a
      delivery that is running slowly.

## Progress
- **Done.** `crates/flux-app/src/admission.rs` is the new module: a `Semaphore` of `limit` slots
  acquired in the actor loop *before* the spawn (`supervisor.rs:291`), so a saturated App stops
  dequeuing, its channel fills, and submitters block in `send`. Backpressure therefore reaches the
  channel adapter producing the storm rather than piling up parked tasks.
- At the bound a delivery **waits** — never dropped, never rejected. Dropping loses work a webhook
  already acknowledged; rejecting suits an HTTP request that can be told 503, not a bus whose
  submitters have nowhere to put the event. The tradeoff is written up in `admission.rs`'s module
  docs, including the deadlock shape it accepts and why the default sits above flux's own fan-out.
- Configuration: `App::with_max_inflight_deliveries` (`app.rs:543`) and
  `FLUX_MAX_INFLIGHT_DELIVERIES`; default `DEFAULT_MAX_INFLIGHT_DELIVERIES = 64`, documented at
  `website/docs/reference/config.md:453`.
- Observability: `DeliveryLoad { in_flight, waiting, limit }` + `is_backpressured()` via
  `App::delivery_load` (`app.rs:554`) — `waiting` is held by the bound, `in_flight` is merely slow.
- Failing-first: `a_delivery_storm_does_not_spawn_a_journey_for_every_event_at_once`
  (`tests/integration.rs:1139`) names no admission API, so it compiles against A-112's tree and
  fails there behaviourally: *"all 80 deliveries were running at once"*.
- A submission's `waiting` count is carried by a `#[must_use]` `Submission` guard rather than a
  `submit`/`abandon` pair, because the send it spans is an `await`: a `deliver` future cancelled
  while blocked on a full queue would otherwise leak a count, and since `waiting > 0` *is*
  `is_backpressured()`, that would pin the App to "backpressured" forever.
- **Not addressed here** (needs its own story): run-routed `Bus::emit` still uses `try_send` and
  returns `0` when the supervisor queue is full, silently dropping that event. Pre-existing, but the
  bound makes a full queue far more reachable, so the lossy path now matters more. See Notes.
- The second risk in Notes (startup vs. bus-event ordering under `App::run`) was **not** touched —
  out of this story's Acceptance.

## Notes
- Filed 2026-07-29 from A-112's handoff. The implementor was explicit that fixing it inside A-112
  would have widened that story past its Acceptance, and asked for it to be its own.
- A second risk from the same report, worth a look while here: under `App::run`, startup no longer
  strictly precedes bus events — the run lease activates before the `Start` response is awaited.
