---
id: A-112
title: Per-delivery bus isolation — concurrent deliveries without cascade double-processing
pillar: Agent
status: ready
priority: 2
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-app, flux-channels]
note: "blocks the whole fleet-coordinator epic; likely breaking for flux-app embedders ⇒ MINOR"
---

# Per-delivery bus isolation — concurrent deliveries without cascade double-processing

## Goal
Let `flux-app` process deliveries concurrently. Today `App::deliver`
(`crates/flux-app/src/app.rs:501`) subscribes to the broadcast bus and drains the cascade its
journeys emit, so `flux-channels` states plainly that deliveries are serialized and that
"cross-channel parallelism needs per-delivery bus isolation"
(`crates/flux-channels/src/lib.rs:20`). A coordinator whose nightly sweep blocks webhook intake is
single-threaded by construction, so nothing else in the epic matters until this lands.

The mechanism already exists — `DeliveryOrigin`, a task-local (`crates/flux-app/src/bus.rs:23`,
`:27`), and `scope_delivery` (`bus.rs:231`). The work is scoping cascade collection to the *causing*
delivery and making `deliver` re-entrant, not inventing a mechanism.

## Acceptance
- [ ] Failing-first test: two `App::deliver` calls issued concurrently on labels with distinct
      journeys each observe **only their own** cascade events — no journey run appears in both
      results, and no event is processed twice. This test fails on today's broadcast fan-out.
- [ ] Failing-first test: a long-running delivery (a sweep-shaped journey) does not delay a second
      delivery submitted while it runs — asserted on ordering/completion, not wall-clock sleeps.
- [ ] Cascade depth/bound semantics are unchanged per delivery: the existing bounded cascade-tree
      guarantees still hold within one delivery's scope.
- [ ] `crates/flux-channels/src/lib.rs`'s `## Concurrency` module doc is updated to describe what is
      now true, rather than leaving the serialization note in place.
- [ ] Full gate green in both workspaces (`cargo fmt --check`, clippy, `cargo test --workspace`).

## Progress
- (not started)

## Notes
- Likely a breaking change for `flux-app` embedders (delivery/bus surface) ⇒ pre-1.0 SemVer makes it
  a **MINOR**.
- Design: [fleet-coordinator.md §6](../designs/fleet-coordinator.md).
- Relevant: `crates/flux-app/src/supervisor.rs:42` (`DeliverySupervisor` — the sole trigger-routing
  owner; direct deliveries and the run lease both submit roots to it).
