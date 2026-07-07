---
id: D-78
title: Cross-namespace entity scan for the Postgres datasource backend — one query instead of N per-scope round trips
pillar: Core
status: done
epic: pg-backend
design: docs/designs/pg-backend.md
note: "global lookups over per-scope namespaces (token→scope resolution, registry sweeps) currently cost namespaces() + a per-scope backend + list() each — 1+N..1+3N serial round trips; a prefix+entity scan is one"
---

# Cross-namespace entity scan for the Postgres datasource backend

## Goal
`PostgresBackend` gains an associated cross-scope read:

```rust
pub fn scan(handle: &Arc<PgHandle>, ns_prefix: &str, entity: &str) -> Result<Vec<(String, Record)>>
```

— one query (`WHERE ns LIKE $prefix || '%' AND entity = $entity`, ordered by `(ns, id)`) returning
`(namespace, record)` pairs. Today a consumer resolving a **global** lookup across per-scope
namespaces (e.g. a public-token → scope resolution over per-scope `head` records) must call
`namespaces()` (1 round trip), then construct a backend and `list()` **per scope** — 1+N serial
round trips warm, 1+3N cold — for something the shared `ds_records` table answers in one.
`namespaces()` itself is the precedent: cross-scope reads on the shared table are already part of
this backend's contract.

## Acceptance
- [ ] `scan` as above; namespace round-trips intact (`ns` returned verbatim, record shape identical
      to `list`); no LIMIT (callers filter) or an optional limit param — designer's call.
- [ ] Env-gated tests: records across three namespaces under two prefixes — `scan("a:", "head")`
      returns exactly the `a:*` heads with correct pairing; empty result on a prefix with no
      records; missing-table tolerance consistent with `namespaces()` (see D-76).
- [ ] Doc note on `DatasourceBackend` (the trait stays per-scope by design; `scan` is deliberately
      an associated fn on the Postgres impl, like `namespaces`).

## Progress
- 2026-07-08 — done. `PostgresBackend::scan(handle, ns_prefix, entity) -> Result<Vec<(String,
  Record)>>` — one query (`WHERE ns LIKE $1 || '%' AND entity = $2 ORDER BY ns, id`), records via
  the existing `row_to_record` (source `plugin/instance` round-trip intact), `ns` verbatim from its
  own column, no limit param (callers filter), prefix semantics mirroring `namespaces()`.
  Missing-table tolerance shared with `namespaces()` via one `is_undefined_table` helper
  (`42P01 → Ok(vec![])`) so the two can't drift (see D-76). Deliberately an associated fn on the
  Postgres impl, not a trait method — doc note added on `DatasourceBackend` stating the trait stays
  per-scope by design. Env-gated tests: exact (ns, record) pairing/ordering across `a:1`/`a:2`/`b:1`
  with noise entities, record equality vs `list()`, empty prefix result, never-bootstrapped schema →
  `Ok(vec![])`. Full package gate green (fmt, default + `--features postgres` tests vs live PG,
  clippy `-D warnings` both ways).

## Notes
- Found in a post-ship review: the per-scope-loop shape was tolerable over local SQLite files but
  becomes serial network amplification on Postgres — worst on unauthenticated lookup paths.
- Design: [pg-backend.md](../designs/pg-backend.md) §4.
