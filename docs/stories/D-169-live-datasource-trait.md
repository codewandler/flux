---
id: D-169
title: Add the async LiveDatasource trait
pillar: Agent
status: ready
priority: 3
epic: async-live-datasource-seam
design: docs/designs/async-live-datasource-seam.md
note: "D-62 phase 2; depends on D-168"
---

# Add the async LiveDatasource trait

## Goal

Give native hosts one object-safe async contract for live system-of-record reads while preserving
the separate synchronous index backend.

## Acceptance

- [ ] `flux-capabilities::datasource::LiveDatasource` exposes `schema`, cancellable async `list`,
      and async `get` over the D-168 types and receives the guarded `ToolContext`.
- [ ] A closed `LiveAccess` declaration describes exact network or connection resources without
      mixing authority metadata into model-facing entity schemas; a pure backend declares none.
- [ ] Registration rejects malformed domains, duplicate entities/filter keys, invalid page limits,
      blank authority subjects, and other impossible schemas before any tool is advertised.
- [ ] A compile/behavior fixture proves the existing `DatasourceBackend` implementations require no
      changes; scoped capability tests pass.

## Progress

- Not started; blocked on D-168.
