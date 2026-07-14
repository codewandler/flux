---
id: A-89
title: Make App own delivery serialization
pillar: Agent
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: verification-first residual — direct or independently wrapped App delivery may bypass adapter-local serialization
---

# Make App own delivery serialization

## Goal

Place delivery/cascade serialization at the shared `App` owner so correctness does not depend on
every adapter instance remembering an outer mutex.

## Acceptance

- [x] First, a failing-first reproducer exercises direct concurrent `App::deliver` calls and two
      independently constructed `AppDeliverer`s around one App. It must demonstrate cross-consumed
      cascades or duplicate journey effects before behavior is changed.
- [x] If the reproducer refutes the finding, record the withdrawal and evidence in the architecture
      review, close this story without speculative synchronization, and retain the regression test.
- [x] If confirmed, one per-App coordinator covers every delivery entry point; concurrent deliveries
      cannot consume each other's broadcast cascades or execute a mutating journey twice.
- [x] Adapter-local redundant locking is removed after the App-owned invariant lands, with no mutex
      held across unrelated work and no reentrant cascade deadlock.
- [x] Delivery result ordering, channel correlation, and existing direct/channel routing behavior
      remain deterministic and covered.

## Progress

- 2026-07-14 — The reproducer confirmed the ownership gap. `App` now owns one lazy delivery actor;
  `deliver`, the long-running `run` lease, and direct bus events all enter that sole trigger router,
  while public broadcast subscriptions are observation-only. Per-App causal root tags keep cascades
  atomic without cross-App contamination, startup is emitted once, and the adapter-local gate is
  gone. Direct, independently wrapped, run-overlap, interleaving, and cross-App tests prove result
  correlation, deterministic ordering, and exactly-once effects.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- A prior review refuted a similar race for the known adapter because it serializes through one
  guard. This story tests the narrower ownership claim rather than assuming a defect.
