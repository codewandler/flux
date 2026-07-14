---
id: D-168
title: Add pure live-datasource contracts
pillar: Agent
status: ready
priority: 2
epic: async-live-datasource-seam
design: docs/designs/async-live-datasource-seam.md
note: "D-62 phase 1: deterministic L0 row, page, filter, reference, and schema types"
---

# Add pure live-datasource contracts

## Goal

Define the pure data vocabulary for async live backends without changing the existing synchronous
record-index contract or introducing IO into L0.

## Acceptance

- [ ] `flux_datasource::live` exposes documented `Row`, `Page<T>`, `PageRequest`, deterministic
      `Filters`/`FilterValue`, weak `Reference`, `LiveSchema`/`LiveEntity`, and
      `FilterKey`/`FilterType` contracts.
- [ ] Serde round-trip tests pin cursor, enum-filter, absent-reference, and deterministic filter-map
      representations; reference shapes cannot carry credentials or runtime handles.
- [ ] Existing `Record`/`DatasourceBackend` wire shapes remain unchanged and the L0 crate stays free
      of IO/runtime dependencies.
- [ ] `cargo test -p codewandler-flux-datasource` and `cargo test -p flux-codegate` pass.

## Progress

- Not started; depends on accepted design story D-62.

## Notes

- D-169 consumes these contracts; names stay under the `live` module to avoid `Row`/`Page` import
  collisions.
