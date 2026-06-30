---
id: D-18
title: Datasource record deletion — delete_source + delete-by-id on DatasourceBackend
pillar: Core
status: done
theme: downstream-managed-services
---

# Datasource record deletion

## Goal
Give `DatasourceBackend` (D-07) a way to remove records at sub-backend granularity so a consumer can manage a
single source's lifecycle (replace/remove) without wiping the whole index. Until now the only removal verb was
the global `clear()`, which makes per-source CRUD impossible on a shared backend.

## Acceptance
- [x] `DatasourceBackend` gains `delete_source(source) -> usize` (drop every record under one source key) and
      `delete(source, entity, ids) -> usize` (drop addressed records), both returning the count removed.
- [x] Implemented in all backends: `SqliteBackend` (FTS mirror kept in sync, in a transaction),
      `MemoryBackend`, and the `SemanticIndex` decorator (delegates to inner + prunes its vector map).
- [x] Failing-first tests: `sqlite::tests::delete_source_and_by_id_remove_records_and_persist` (deletions
      survive a reopen, are FTS-synced, and never touch a second source) and
      `memory::tests::delete_source_and_by_id_are_scoped`.

## Progress
- Done. Trait + three impls + tests; `cargo test -p flux-capabilities` green.

## Notes
- Downstream driver: customer-managed knowledge sources need per-source replace/delete to
  CRUD an account's knowledge in one persistent `SqliteBackend`. See `flux-capabilities/src/datasource/`.
- Account/tenant scoping stays a consumer concern (as in D-07); this only adds the removal primitive.
