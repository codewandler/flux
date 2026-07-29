---
id: A-129
title: Bound delivery concurrency — the mpsc capacity was the only backpressure
pillar: Agent
status: ready
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
- [ ] A bound on in-flight deliveries, configurable, with a documented default.
- [ ] Failing-first test: N deliveries submitted at once with a limit of K run at most K
      concurrently, and the remainder still complete — bounded, not dropped.
- [ ] The bound does not reintroduce head-of-line blocking between unrelated channels: a slow sweep
      must not starve webhook intake, which is A-112's Acceptance and must keep passing.
- [ ] Backpressure is observable — a delivery waiting on the bound is distinguishable from a
      delivery that is running slowly.

## Progress
- (not started)

## Notes
- Filed 2026-07-29 from A-112's handoff. The implementor was explicit that fixing it inside A-112
  would have widened that story past its Acceptance, and asked for it to be its own.
- A second risk from the same report, worth a look while here: under `App::run`, startup no longer
  strictly precedes bus events — the run lease activates before the `Start` response is awaited.
