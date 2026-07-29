---
id: A-133
title: "Startup does not strictly precede bus events under `App::run` — the run lease activates before `Start` completes"
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-app]
note: "filed from A-112's implementor report and still unaddressed — it has been living as a Note on A-129, which explicitly declined it as out of Acceptance"
---

# Startup does not strictly precede bus events under `App::run` — the run lease activates before `Start` completes

## Goal
`App::run`'s doc comment promises an ordering: "emit `startup` once, **then** route public bus events
forever" (`crates/flux-app/src/app.rs:524-527`). The supervisor does not honour the *then*.

In `Supervisor::run` (`crates/flux-app/src/supervisor.rs:138-178`) the startup path sends
`DeliveryMessage::Start` onto the queue and then calls:

```rust
submission.enqueued();
lease.run.activate();          // supervisor.rs:164
result
    .await                     // supervisor.rs:165-167 — the Start response
    .map_err(...)??;
```

`lease.run.activate()` runs **before** the `Start` response is awaited. Activation is precisely what
opens the run route: `Bus::emit`'s external-run branch admits an event only when the `RunContext`
passes `RunContext::accepts` (`crates/flux-app/src/bus.rs:180-181`). So in the window between
`activate()` and the `Start` oneshot resolving, a bus event can be accepted, enqueued behind `Start`,
and — since A-112 made the actor loop spawn each dequeued delivery rather than run it inline — begin
executing before the startup journey has finished. Any journey that assumes startup already ran (seed
state, a board sweep priming, a registry warm-up) can observe the pre-startup world.

Decide whether that ordering is part of `App::run`'s contract. If it is, enforce it; if it is not,
the doc comment's *then* has to go, because right now the code and the promise disagree.

## Acceptance
- [ ] The contract is stated explicitly: either `App::run` guarantees that no bus-routed delivery
      begins before the `startup` journey completes, or it documents that startup is merely *enqueued
      first* and journeys must not assume it has run. **Decide it here and say why** — the promise in
      `app.rs:524-527` currently reads as the former.
- [ ] Failing-first test: with an event emitted into the run route during the startup window, a
      bus-triggered journey observes state that only the completed `startup` journey establishes.
      Ordering tests are timing-shaped, so use the rendezvous pattern the A-112 tests already use
      (`crates/flux-app/tests/integration.rs:770-835`) rather than sleeps — a flaky proof of an
      ordering property is worse than none.
- [ ] The fix does not lose the event that arrives in the window. Deferring `activate()` until after
      `Start` resolves closes the ordering hole by *rejecting* those emits instead — the run route
      simply does not accept them — which trades a correctness bug for a silent-loss bug. Whatever is
      chosen (queue them, accept-then-hold, or reject with the rejection made visible) must be tested,
      not assumed.
- [ ] Startup remains exactly-once and the lease semantics are unchanged: a second concurrent
      `App::run` still fails promptly, a cancelled call still releases the lease without repeating
      startup, and `startup_sent` / `startup_observed` (`supervisor.rs:65-66`, `:152-154`) keep their
      meanings. The existing supervisor tests for those must not be relaxed to make this pass.
- [ ] Standard gate green in both workspaces (root + `plugins/`), `cargo fmt --check` included.

## Progress
- (not started)

## Notes
- Filed 2026-07-29 from the fleet-coordinator integration run. **First surfaced by A-112's
  implementor**, who reported it as a second risk alongside the unbounded-concurrency finding. Only
  the first became a story (A-129); this one has been living as a trailing Note on
  `docs/stories/A-129-bound-delivery-concurrency.md` ever since, and A-129's Progress records that it
  "was **not** touched — out of this story's Acceptance". This story exists so it stops being a
  footnote on a closed story.
- Evidence as given by that report and re-verified against `main` (base `9721daca`): the run lease
  activates before the `Start` response is awaited, so a bus event can be processed before startup
  completes — `supervisor.rs:164` versus `supervisor.rs:165-167`.
- ⚠ Interacts with A-132 (a run-routed emit is silently dropped when the supervisor queue is full).
  If this story's answer is "hold the events during startup", it lands squarely on A-132's lossy
  branch; coordinate or sequence them.
- Why it matters to this epic rather than in the abstract: the coordinator (A-117) starts with a
  `schedule` channel and a webhook channel live at once, which is the shape that actually races
  startup — a cron tick or an inbound hook arriving while startup is still running.
