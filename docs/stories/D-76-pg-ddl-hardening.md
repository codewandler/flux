---
id: D-76
title: Postgres DDL hardening — advisory-lock the bootstrap, split ensure_schema from construction, tolerate a missing table in namespaces()
pillar: Core
status: ready
priority: 3
epic: pg-backend
design: docs/designs/pg-backend.md
note: "post-ship review of D-71/73/74: concurrent first-boot DDL races Postgres's non-atomic IF NOT EXISTS (pg_type_typname_nsp_index errors); PostgresBackend::new re-runs shared-table DDL per construction; namespaces() errors on a fresh database"
---

# Postgres DDL hardening

## Goal
Make the Postgres backends' schema bootstrap safe under concurrency and free of per-construction
I/O. Three defects found reviewing the shipped epic:

1. **The bootstrap DDL races across processes.** `CREATE TABLE IF NOT EXISTS` is not atomic in
   Postgres: two connections that both pass the catalog check race the insert, and the loser errors
   (`duplicate key value violates unique constraint "pg_type_typname_nsp_index"`; CREATE INDEX /
   CREATE SCHEMA IF NOT EXISTS race analogously). Two replicas cold-booting against a fresh
   database can fail `PgEvents::connect`, `PostgresBackend::new`, or flux-pg's `CREATE SCHEMA`
   after_connect hook for a transient, non-config reason. The append path already takes
   `pg_advisory_xact_lock` — the DDL paths take nothing.
2. **`PostgresBackend::new` runs the shared-table DDL on every construction.** `ds_records` is one
   table for all namespaces, yet every per-scope constructor pays two blocking round trips
   (CREATE TABLE + CREATE INDEX) — pure waste after the first, and it forces callers that build
   backends lazily (e.g. under a cache lock) to do network I/O where construction should be free.
3. **`namespaces()` errors on a fresh database.** `SELECT DISTINCT ns FROM ds_records` fails with
   `undefined_table` when no backend has ever been constructed — an enumeration over "no records
   yet" should be `Ok(vec![])`, matching what a scan over zero per-scope SQLite files returns.

## Acceptance
- [ ] All bootstrap DDL (flux-pg `CREATE SCHEMA` hook, `PgEvents::connect` schema, the new
      `ensure_schema` below) runs under a transaction-scoped advisory lock (e.g.
      `pg_advisory_xact_lock(hashtextextended('flux:ddl', 0))`) so concurrent first-boots
      serialize instead of erroring. Failing-first: a test spawning N concurrent connects against
      a throwaway schema — red (flaky duplicate-key) before, green after.
- [ ] `PostgresBackend::ensure_schema(handle) -> Result<()>` (associated fn) owns the `ds_records`
      DDL; `PostgresBackend::new` becomes I/O-free (binds the namespace only). Call it once from
      wherever a deployment opens its stores. `PgEvents::connect` keeps its eager DDL (it already
      runs once per store) but under the lock.
- [ ] `namespaces()` treats `undefined_table` as `Ok(vec![])` (tolerant read), with a test against
      a schema where `ensure_schema` was never called.
- [ ] Conformance suites still green on both backends; default build untouched.

## Progress
- (not started)

## Notes
- Found in a post-ship adversarial review of the pg-backend epic; the constructor-I/O half also
  fixes a consumer-side hazard where lazily-constructed backends do DDL while holding a cache lock.
- Design: [pg-backend.md](../designs/pg-backend.md) §3–§5.
