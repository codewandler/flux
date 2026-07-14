---
id: D-173
title: Prove and document live-datasource adoption
pillar: Agent
status: backlog
epic: async-live-datasource-seam
design: docs/designs/async-live-datasource-seam.md
note: "D-62 phase 6; depends on D-172"
---

# Prove and document live-datasource adoption

## Goal

Ship a hermetic reference backend and end-to-end SDK recipe proving a consumer can replace its
custom paging/projection layer with the shared seam.

## Acceptance

- [ ] A small in-memory backend exercises multiple entities, typed filters, cursor paging, get,
      not-found, and no-external-access authority through real `Executor::dispatch`.
- [ ] An SDK integration test proves registration, evidence-gated catalog surfacing, list/get calls,
      and authorization denial without bypassing approval or guarded IO.
- [ ] Public datasource documentation explains live-vs-indexed backends, the two-op surface,
      cursor/filter contracts, exact authority, and the weak-reference/no-secret rule with a
      runnable Rust example.
- [ ] Root full gate and relevant documentation checks pass; user-visible behavior is recorded in
      both changelogs.

## Progress

- Not started; blocked on D-172.
