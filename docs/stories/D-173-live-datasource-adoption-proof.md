---
id: D-173
title: Prove and document live-datasource adoption
pillar: Agent
status: done
epic: async-live-datasource-seam
design: docs/designs/async-live-datasource-seam.md
note: "D-62 phase 6; depends on D-172"
---

# Prove and document live-datasource adoption

## Goal

Ship a hermetic reference backend and end-to-end SDK recipe proving a consumer can replace its
custom paging/projection layer with the shared seam.

## Acceptance

- [x] A small in-memory backend exercises multiple entities, typed filters, cursor paging, get,
      not-found, and no-external-access authority through real `Executor::dispatch`.
- [x] An SDK integration test proves registration, evidence-gated catalog surfacing, list/get calls,
      and authorization denial without bypassing approval or guarded IO.
- [x] Public datasource documentation explains live-vs-indexed backends, the two-op surface,
      cursor/filter contracts, exact authority, and the weak-reference/no-secret rule with a
      runnable Rust example.
- [x] Root full gate and relevant documentation checks pass; user-visible behavior is recorded in
      both changelogs.

## Progress

- 2026-07-15 — The consumer integration test failed first against the then-missing SDK datasource
  re-export and builder seam. The completed proof now supplies a hermetic tickets/customers backend
  with string, integer, boolean, and enum filters; backend-owned cursors; get/not-found; weak
  references; and an entry counter that proves authorization denial happens before backend IO.
- 2026-07-15 — The runnable example dispatches the generated operations through the real executor.
  SDK tests prove the configured-domain catalog is hidden before evidence and visible afterward,
  exact datasource-only authority for the in-memory backend, paging across multiple entities, and
  structural policy denial. Public SDK/agent/architecture docs now explain the indexed/live split;
  the complete root and nested-plugin build/test/clippy/fmt gates, codegate, website build, and
  customer-changelog sync check are green on the exact staged tree.
