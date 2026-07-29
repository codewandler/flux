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
- [x] Failing-first test: two `App::deliver` calls issued concurrently on labels with distinct
      journeys each observe **only their own** cascade events — no journey run appears in both
      results, and no event is processed twice. This test fails on today's broadcast fan-out.
- [x] Failing-first test: a long-running delivery (a sweep-shaped journey) does not delay a second
      delivery submitted while it runs — asserted on ordering/completion, not wall-clock sleeps.
- [x] Cascade depth/bound semantics are unchanged per delivery: the existing bounded cascade-tree
      guarantees still hold within one delivery's scope.
- [x] `crates/flux-channels/src/lib.rs`'s `## Concurrency` module doc is updated to describe what is
      now true, rather than leaving the serialization note in place.
- [x] Full gate green in both workspaces (`cargo fmt --check`, clippy, `cargo test --workspace`).

## Progress
- **Done.** `DeliverySupervisor`'s actor loop no longer processes inline — it only dequeues, and
  spawns each `DeliveryMessage` into a `JoinSet` (`supervisor.rs`, new `process()` fn; the three
  match arms moved verbatim, `continue` → `return`, cancellation and run-lease semantics unchanged
  but now per task). `process_root` already gave every root its own cascade queue, so isolation
  followed once roots stopped queueing behind one another.
- The non-obvious half: `Engine::depth` (the `MAX_SPAWN_DEPTH` guard) was an engine-wide
  `AtomicU32` that was only *accidentally* per-delivery, because deliveries were serialized. Under
  concurrency 17 simultaneous deliveries would trip the recursion guard on work that never
  recursed. The budget moved into `DeliveryOrigin` (`depth: Arc<AtomicU32>`), with the engine
  counter kept as the fallback for journey runs outside any delivery scope.
- Tests (`crates/flux-app/tests/integration.rs`), all four new — a `hold`/`release` `Notify` gate
  makes a journey block deterministically, so overlap is proven by ordering, not by sleeping:
  `concurrent_deliveries_each_collect_only_their_own_cascade`,
  `a_long_running_delivery_does_not_delay_the_next_one`,
  `a_self_feeding_cascade_stays_bounded_within_one_delivery` (MAX_CASCADE regression guard, passed
  before and after), `a_wave_of_deliveries_does_not_share_one_nesting_budget` (24 concurrent
  deliveries on a shared `Barrier`, wider than `MAX_SPAWN_DEPTH`'s 16).
- **Nested self-delivery still fails fast.** `DeliverySupervisor::deliver` keeps its
  `origin.supervisor == self.id` guard. Per-message spawning removes the deadlock that motivated
  it, but lifting it would open an unbounded `deliver → journey → deliver` recursion that no
  `MAX_CASCADE` covers. Out of scope here; behaviour unchanged.
- **Deliberately not fixed, filed separately:** delivery concurrency is now unbounded (the `mpsc`
  `CAPACITY` bound was the only backpressure and the loop drains it instantly), and `App::run` no
  longer strictly orders `startup` before bus events (the run lease activates before the `Start`
  response is awaited). Both are consequences of removing serialization, not regressions inside it.
- Breaking for `flux-app` embedders who relied on `App::deliver` being globally serialized.

## Notes
- Likely a breaking change for `flux-app` embedders (delivery/bus surface) ⇒ pre-1.0 SemVer makes it
  a **MINOR**.
- Design: [fleet-coordinator.md §6](../designs/fleet-coordinator.md).
- Relevant: `crates/flux-app/src/supervisor.rs:42` (`DeliverySupervisor` — the sole trigger-routing
  owner; direct deliveries and the run lease both submit roots to it).
