---
id: D-73
title: Postgres EventStore backend — open_postgres, advisory-lock append, conformance suite, CI pg job
pillar: Core
status: done
epic: pg-backend
design: docs/designs/pg-backend.md
note: "needs D-71 + D-72; BIGSERIAL preserves the s_<n>/turn-id contracts, payload stays TEXT (byte-exact serde), per-stream pg_advisory_xact_lock replaces Mutex+BEGIN IMMEDIATE and additionally serializes appends ACROSS replicas"
---

# Postgres EventStore backend

## Goal
The event log — conversations, run traces, turn telemetry, `EventKind::Custom` app facts —
becomes deployable on a shared Postgres: `EventStore::open_postgres(Arc<PgHandle>)` behind a
`postgres` feature, byte-compatible with the SQLite backend at the API and payload level, and
safe for concurrent appenders across processes/replicas.

## Acceptance
- [ ] `flux-events` gains `postgres = ["dep:flux-pg"]`; rusqlite stays unconditional; the
      default build/test is unchanged and DB-free.
- [ ] `store/postgres.rs` implements the full D-72 primitive set. Schema created inline
      (`CREATE TABLE IF NOT EXISTS` / `ADD COLUMN IF NOT EXISTS`, matching the house
      no-migration-framework style): `streams` (`n BIGSERIAL PRIMARY KEY`, same columns) +
      `events` (`global_seq BIGSERIAL PRIMARY KEY`, `payload TEXT`, `UNIQUE(id)`,
      `UNIQUE(stream, stream_seq)`, indexes on `(stream, kind, stream_seq)`, `(kind)`, and
      partial `(stream, turn_id) WHERE turn_id IS NOT NULL`). `INSERT … RETURNING` replaces
      `last_insert_rowid()`; `s_<n>` / turn-id contracts hold (monotone, never reused).
- [ ] `payload` is `TEXT`, not `jsonb`, and the module header documents why (byte-exact
      round-trip of the adjacently-tagged serde JSON; payload is opaque to SQL).
- [ ] Append transaction opens with
      `SELECT pg_advisory_xact_lock(hashtextextended($stream, 0))` — including for ad-hoc
      (non-registry) streams; `UNIQUE(stream, stream_seq)` remains the durable backstop.
      `prune_empty`/`prune_inactive` batch deletes with `WHERE stream = ANY($1)` in one tx.
- [ ] Conformance: existing store test bodies extracted into helpers taking `EventStore`,
      run as both `mod sqlite_tests` (as today) and env-gated `mod postgres_tests`
      (`TEST_POSTGRES_URL`, skip-with-notice; each test in a throwaway `?schema=t_<ulid>`,
      parallel-safe). Failing-first for the new backend: the suite red against a stub, green
      against the impl.
- [ ] PG-only test: N concurrent appends to one stream from a
      `#[tokio::test(flavor = "multi_thread")]` context — contiguous `stream_seq`, no gaps,
      no duplicates, no panic (proves the advisory lock and the D-71 bridge together).
- [ ] CI: new workflow job with `services: postgres:16` running
      `cargo test -p flux-events -p flux-capabilities -p flux-pg --features postgres` and
      `cargo clippy --features postgres -- -D warnings`; the existing job untouched.

## Progress
- (not started — needs D-71 and D-72)

## Notes
- Design: [pg-backend.md](../designs/pg-backend.md) §3 + §5.
- sqlx caches prepared statements per pooled connection — the `prepare_cached` hot paths
  (e.g. `conversation_delta`) need no special handling.
- D-75 (`prune_older_than`) may fold into this story if convenient.
