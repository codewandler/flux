---
id: D-168
title: Add pure live-datasource contracts
pillar: Agent
status: done
epic: async-live-datasource-seam
design: docs/designs/async-live-datasource-seam.md
note: "D-62 phase 1: deterministic L0 row, page, filter, reference, and schema types"
---

# Add pure live-datasource contracts

## Goal

Define the pure data vocabulary for async live backends without changing the existing synchronous
record-index contract or introducing IO into L0.

## Acceptance

- [x] `flux_datasource::live` exposes documented `Row`, `Page<T>`, `PageRequest`, deterministic
      `Filters`/`FilterValue`, weak `Reference`, `LiveSchema`/`LiveEntity`, and
      `FilterKey`/`FilterType` contracts.
- [x] Serde round-trip tests pin cursor, enum-filter, absent-reference, and deterministic filter-map
      representations; reference shapes cannot carry credentials or runtime handles.
- [x] Existing `Record`/`DatasourceBackend` wire shapes remain unchanged and the L0 crate stays free
      of IO/runtime dependencies.
- [x] `cargo test -p codewandler-flux-datasource` and `cargo test -p flux-codegate` pass.

## Progress

- Added the namespaced pure-data module, deterministic scalar filters, tagged weak references, and
  compact serde shapes. The contract test failed first on the absent module, then passed with the
  scoped datasource test/clippy gate and flux-codegate.

## Notes

- D-169 consumes these contracts; names stay under the `live` module to avoid `Row`/`Page` import
  collisions.
