---
id: D-76
title: Postgres DDL hardening — advisory-lock the bootstrap, split ensure_schema from construction, tolerate a missing table in namespaces()
pillar: Core
status: done
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
- 2026-07-08 — done, across all three crates:
  - **flux-pg** owns the lock: new `flux_pg::ddl_lock(&mut tx)` + `DDL_LOCK_SQL`
    (`pg_advisory_xact_lock(hashtextextended('flux:ddl', 0))`, one global key — bootstrap is rare,
    contention irrelevant), documented as the FIRST statement of every bootstrap-DDL transaction.
    The `after_connect` `CREATE SCHEMA IF NOT EXISTS` now runs in a locked transaction (then `SET
    search_path` on the session as before).
  - **flux-events**: `PgEvents::connect` bootstrap = begin tx → `ddl_lock` → DDL batch → commit
    (still eager, once per store). A `run_schema_ddl(conn)` helper works around sqlx `raw_sql`'s
    HRTB "implementation of `Executor` is not general enough" inside the `Send + 'static` bridge
    future — same simple-query multi-statement protocol, documented on the helper.
  - **flux-capabilities**: `PostgresBackend::ensure_schema(handle)` (associated fn) owns the
    `ds_records` DDL under the lock; **`PostgresBackend::new(handle, ns) -> Self` is now I/O-free
    and infallible — BREAKING signature change** (deliberate: adopters get a compile error pointing
    at `ensure_schema`, not a runtime `undefined_table`) → next release is a minor bump.
    `namespaces()` treats `42P01 undefined_table` as `Ok(vec![])` via a shared
    `is_undefined_table` helper (also used by D-78's `scan`).
  - Concurrency tests (8 own-handle threads, `Barrier`-released, one fresh schema, cleanup before
    assert) in all three crates: `concurrent_first_boot_bootstrap_is_serialized` (flux-pg),
    `concurrent_cold_boots_serialize_bootstrap_ddl` (flux-events),
    `pg_concurrent_ensure_schema_bootstrap` (flux-capabilities). Honest caveat: the pre-fix red
    state is probabilistic and did NOT reproduce on localhost in 8 barrier-tightened storm runs —
    the race window (catalog check → insert) is sub-microsecond locally; the tests stand as
    regression tripwires and the lock is correct by construction. Fresh-db tolerance test:
    `pg_namespaces_tolerates_missing_table`. All package gates green vs live PG.

## Notes
- Found in a post-ship adversarial review of the pg-backend epic; the constructor-I/O half also
  fixes a consumer-side hazard where lazily-constructed backends do DDL while holding a cache lock.
- Design: [pg-backend.md](../designs/pg-backend.md) §3–§5.
