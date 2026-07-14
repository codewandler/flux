---
id: D-169
title: Add the async LiveDatasource trait
pillar: Agent
status: done
epic: async-live-datasource-seam
design: docs/designs/async-live-datasource-seam.md
note: "D-62 phase 2; depends on D-168"
---

# Add the async LiveDatasource trait

## Goal

Give native hosts one object-safe async contract for live system-of-record reads while preserving
the separate synchronous index backend.

## Acceptance

- [x] `flux-capabilities::datasource::LiveDatasource` exposes `schema`, cancellable async `list`,
      and async `get` over the D-168 types and receives the guarded `ToolContext`.
- [x] A closed `LiveAccess` declaration describes exact network or connection resources without
      mixing authority metadata into model-facing entity schemas; a pure backend declares none.
- [x] The shared pre-registration validator rejects malformed domains, duplicate entities/filter
      keys, invalid page limits, blank authority subjects, and other impossible schemas before any
      tool can be advertised.
- [x] A compile/behavior fixture proves the existing `DatasourceBackend` implementations require no
      changes; scoped capability tests pass.

## Progress

- Added the object-safe async trait, guarded context threading, closed network/connection access
  declarations, and fail-fast static contract validation. The integration test failed first on the
  absent exports, then the full capabilities tests, scoped clippy, and codegate passed.
