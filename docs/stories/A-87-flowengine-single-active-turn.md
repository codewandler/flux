---
id: A-87
title: Make FlowEngine own the single-active-turn invariant
pillar: Agent
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: shared mutable turn state is protected by optional outer-surface mutexes instead of the engine
---

# Make FlowEngine own the single-active-turn invariant

## Goal

Prevent concurrent calls on one `FlowEngine` from cross-wiring session state, sinks, identity,
usage, receipts, or audit records, without serializing independent engines.

## Acceptance

- [x] A mandatory engine-level turn gate or `AgentHandle` serializes every public text, authored,
      resumed, and flow-driven voice entry point on the same engine.
- [x] Failing-first tests run two direct concurrent calls through the raw public engine and prove
      their sinks, histories, usage, advertised tools, caller identities, and audit rows never mix.
- [x] A control test proves turns on distinct engines can overlap, and nested authored operations do
      not deadlock or recursively acquire the outer gate.
- [x] Mutable caller/turn context becomes lexical or turn-owned where possible; long-lived shared
      cells cannot be retargeted while an earlier turn is active.
- [x] Redundant SDK/server/App gates are removed or reduced to documented higher-level ordering once
      the engine owns the invariant; the advanced SDK engine escape hatch remains safe.
- [x] Cancellation and finalization semantics reuse A-86's lifecycle rather than creating another
      execution branch.

## Progress

- 2026-07-14 — Moved the single-active-turn gate into `FlowEngine` across text and authored entries
  and kept runtime turn context lexical. Raw-engine concurrency, distinct-engine overlap, nested-flow
  no-deadlock, cancellation, sink/history/usage/catalog/identity/audit isolation tests prove it.
  `TurnIdentity` now freezes each request after the engine gate; mutable `IdentityCell::set` /
  `Executor::set_identity` retargeting and the server's redundant identity mutex are removed. A
  two-principal raw-engine test proves policy dispatch, sinks, observations, and audit rows stay
  attributed to the correct turn, including across supervised child-task propagation.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Sequence after C-60 and alongside/after A-86.
- Primary evidence: `EngineLoopHost::set_turn`, `FlowEngine::run_turn*`, SDK `Client::engine`, and
  cached App agent engines.
