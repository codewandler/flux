---
id: D-74
title: PostgresBackend for the datasource trait — namespace-column isolation + tsvector search parity
pillar: Core
status: done
epic: pg-backend
design: docs/designs/pg-backend.md
note: "needs only D-71 (parallel to D-72/73); the trait already exists so this is purely additive — ns column bound at construction replaces one-file-per-scope, websearch_to_tsquery+ts_rank replaces FTS5/bm25"
---

# PostgresBackend for the datasource trait

## Goal
A `DatasourceBackend` impl over Postgres so record stores (agent registries, knowledge
corpora — anything built on the records + keyword-search trait) can live in a shared
database. Isolation stays structural: a namespace bound at construction is the exact
equivalent of today's one-SQLite-file-per-scope, and search results are shape-identical
across backends.

## Acceptance
- [ ] `flux-capabilities` gains `postgres = ["dep:flux-pg"]`;
      `datasource/postgres.rs` exports `PostgresBackend` beside `SqliteBackend`
      (`#[cfg(feature = "postgres")]`).
- [ ] `PostgresBackend::new(handle, namespace)` binds `ns` once (never per-call). One
      `ds_records` table, PK `(ns, source, entity, id)`, columns matching the SQLite
      `records` shape (`links`/`meta` as TEXT JSON), plus a stored generated column
      `fts tsvector` over `to_tsvector('simple', title || ' ' || body)` with a GIN index.
      Upsert is a single `INSERT … ON CONFLICT DO UPDATE` (no FTS-mirror sync).
- [ ] Search parity: the same OR-of-quoted-terms query construction as the SQLite impl,
      executed via `websearch_to_tsquery('simple', $1)` (never errors on malformed user
      input) ranked by `ts_rank(fts, query) DESC, id ASC LIMIT $n`. `matched_fields` +
      `snippet` extracted into a shared private `datasource/text.rs` used by both backends —
      `Match` output is shape-identical.
- [ ] `list`'s `LIMIT -1` idiom replaced by conditionally-built LIMIT/OFFSET; `len()` =
      `SELECT COUNT(*) WHERE ns = $1`.
- [ ] `PostgresBackend::namespaces(handle, prefix) -> Result<Vec<String>>` — the analog of
      scanning a directory of per-scope DB files (`SELECT DISTINCT ns WHERE ns LIKE $1||'%'`).
- [ ] Env-gated conformance tests mirroring the three SQLite tests (search/persistence,
      upsert-replaces-and-fts-in-sync, delete-source/by-id) + an ns-isolation test: two
      backends on one pool with different `ns` — records invisible across.

## Progress
- (not started — needs D-71)

## Notes
- Design: [pg-backend.md](../designs/pg-backend.md) §4.
- Rejected: schema-per-scope (DDL explosion, search_path juggling) and source-key prefixing
  (corrupts `row_to_record`'s `split_once('/')` parsing and leaks into consumer-visible
  `Source` keys).
- Out of scope, backlog note: a `VectorStore` impl over pgvector — the trait exists, the
  in-memory impl covers today's consumers; file separately when a consumer needs durable
  vectors.
