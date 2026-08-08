---
id: C-576
title: "Attribute resource usage to requests, results and Board work"
pillar: Core
status: backlog
epic: resource-accounting
design: docs/designs/resource-accounting.md
areas: [flux-events, flux-orchestrate, flux-board]
depends_on: [C-575]
note: "explicit causal BoardRef/assignment links; exclusive/inclusive totals; shared overhead stays visible or uses a versioned allocation policy"
---

# Say which result consumed the resources

## Goal

Attach causal resource spans to exact requests/results and Fleet Board assignments, with aggregation
semantics that stay truthful under nested agents and shared coordinator/integration work.

## Acceptance

- [ ] Failing first, two concurrent workers plus a shared integration gate can be grouped only by
      overlapping time/session and cannot produce a trustworthy per-story total. The fixed path binds
      root result, BoardRef, assignment revision, worker, wave and repository at admission/dispatch.
- [ ] Writer, nested task, reviewer, rework, targeted-check and handoff descendants inherit explicit
      causal identity. A worker cannot attribute usage to another assignment or remove its own spans.
- [ ] Projections expose exclusive/direct and inclusive descendant totals with each receipt counted
      once. They never sum a parent's already-inclusive token rollup together with the same child.
- [ ] Coordinator planning, supervision and integration gates are shared/unallocated by default. An
      optional revisioned policy may allocate by a stated basis; allocated overhead and policy remain
      separate from direct/inclusive totals.
- [ ] Board epic membership is resolved at an explicit Board revision. Later story/epic moves do not
      rewrite old bills; corrections append an adjustment and preserve original evidence.
- [ ] Missing BoardRef or ambiguous parentage remains unattributed with an actionable gap. Timestamp
      overlap, model names and prose are never used to guess ownership.
- [ ] Contract tests cover one story, five concurrent stories across three repositories, nested task,
      shared gate, retry/idempotency, Board backend mapping and epic membership change.

## Progress

- (not started)

## Notes

- Board stores evidence references, not the canonical metrics ledger, and does not become a
  datasource.
