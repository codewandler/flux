---
id: D-170
title: Project live datasources into list and get operations
pillar: Agent
status: done
epic: async-live-datasource-seam
design: docs/designs/async-live-datasource-seam.md
note: "D-62 phase 3; depends on D-169"
---

# Project live datasources into list and get operations

## Goal

Turn any registered live backend into the uniform `<domain>.list` and `<domain>.get` operation
surface so consumers do not hand-build adapters per integration.

## Acceptance

- [x] `try_register_live_datasource` atomically installs exactly two source-labelled tools and
      rejects collisions without partially mutating the registry.
- [x] Generated input schemas enumerate the backend's entities and their declared filter fields;
      outputs render compact list rows, full get rows, `next:` cursors, empty pages, and not-found
      results consistently.
- [x] Calls dispatch to the typed backend with the guarded `ToolContext`; no operation calls
      `execute` directly outside tests or introduces an IO side path.
- [x] Failing-first mock-backend tests cover list/get routing, rendering, collision atomicity, and
      backend error propagation.

## Progress

- Added the generated two-op projection, conservative datasource access declaration, atomic
  source-labelled registration, and consistent row/page rendering. The integration fixture failed
  first on the absent registration API, then passed the scoped capabilities tests, clippy, format,
  and architecture gate. Filter-contract enforcement and exact authority/surfacing remain isolated
  to D-171 and D-172 respectively.
