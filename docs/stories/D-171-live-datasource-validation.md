---
id: D-171
title: Enforce live-datasource filters limits and cursors
pillar: Agent
status: backlog
epic: async-live-datasource-seam
design: docs/designs/async-live-datasource-seam.md
note: "D-62 phase 4; depends on D-170"
---

# Enforce live-datasource filters limits and cursors

## Goal

Make the generic projection enforce each backend's declared query contract before remote work is
attempted.

## Acceptance

- [ ] Unknown entities/filter keys, missing required filters, wrong scalar types, and invalid enum
      values fail with path-aware messages before the backend is invoked.
- [ ] Omitted limits use `default_page`, explicit limits clamp to `max_page`, and opaque cursors are
      passed through byte-for-byte without interpretation or logging as capabilities.
- [ ] Normalized deterministic filters—not the caller's raw JSON—reach the backend.
- [ ] Failing-first tests cover every rejection and boundary, including zero/oversized limits and
      cursor values containing punctuation and Unicode.

## Progress

- Not started; blocked on D-170.
