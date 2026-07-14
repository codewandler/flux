---
id: D-171
title: Enforce live-datasource filters limits and cursors
pillar: Agent
status: done
epic: async-live-datasource-seam
design: docs/designs/async-live-datasource-seam.md
note: "D-62 phase 4; depends on D-170"
---

# Enforce live-datasource filters limits and cursors

## Goal

Make the generic projection enforce each backend's declared query contract before remote work is
attempted.

## Acceptance

- [x] Unknown entities/filter keys, missing required filters, wrong scalar types, and invalid enum
      values fail with path-aware messages before the backend is invoked.
- [x] Omitted limits use `default_page`, explicit limits clamp to `max_page`, and opaque cursors are
      passed through byte-for-byte without interpretation or logging as capabilities.
- [x] Normalized deterministic filters—not the caller's raw JSON—reach the backend.
- [x] Failing-first tests cover every rejection and boundary, including zero/oversized limits and
      cursor values containing punctuation and Unicode.

## Progress

- 2026-07-15 — The rejection matrix failed first because undeclared filters reached the backend,
  entity errors lacked an input path, zero was accepted, and oversized limits were not clamped.
- 2026-07-15 — Added path-aware entity/filter enforcement, required and scalar/enum validation,
  deterministic normalized filters, default/clamped limits, and byte-exact opaque cursor plumbing.
  The full capabilities tests, warnings-denied clippy, formatting, and architecture gate are green.
