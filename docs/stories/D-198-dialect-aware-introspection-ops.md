---
id: D-198
title: Dialect-aware introspection for all six sql ops
pillar: Core
status: done
priority:
epic: mariadb-support
design: docs/designs/mariadb-support.md
areas: [plugins, docs]
note: deletes require_postgres; per-dialect SQL per op, including the MySQL schema==database semantic collapse
---

# Dialect-aware introspection for all six `sql` ops

## Goal
Make `test`, `query`, `database.list`, `table.list`, `table.show`, and `index.list` work on
MariaDB/MySQL by giving each op per-dialect SQL, and delete `require_postgres()`.

## Why
Wire-protocol support is necessary but not sufficient. The introspection SQL is Postgres-specific,
and in one case Postgres-*semantic* — the ops would silently mean different things across dialects if
this were treated as a syntax port. See [mariadb-support.md](../designs/mariadb-support.md) —
*D-198 — dialect-aware introspection*.

## Acceptance
- [x] **`require_postgres()` deleted**, replaced by per-op dialect dispatch. No op returns the
      "not yet supported" residual for `mysql`/`mariadb` any more. Failing-first test: the existing
      `mysql_and_sqlite_route_to_clear_errors` test is split — sqlite keeps its error, mysql must now
      succeed.
- [x] **`table.list` and `index.list` rewritten off `pg_catalog`** (`plugins/sql/src/main.rs:1087,1268`
      read `pg_class`/`pg_index`/`pg_namespace`, which have no MySQL equivalent) onto
      `information_schema.tables` and `information_schema.statistics` / `SHOW INDEX`.
- [x] **`table.show` foreign keys use the MySQL shape** — MySQL exposes
      `referenced_table_name`/`referenced_column_name` directly on `key_column_usage`, where Postgres
      needs the three-way join through `constraint_column_usage` (`:1218`). A different query, not a
      tweaked one.
- [x] **`database.list`'s divergent meaning is documented, not papered over** — the query parses on
      both engines but Postgres models database > schema > table while MySQL treats schema and
      database as the same object, so the op returns *schemas within the connected database* on
      Postgres and *actual databases* on MySQL. The op description states which, per dialect.
      Failing-first test asserts the documented result on each.
- [x] **System-object filters per dialect** — Postgres excludes `pg_catalog`/`information_schema`;
      MySQL excludes `information_schema`/`mysql`/`performance_schema`/`sys`.
- [x] **Read-only guard unchanged** — `SELECT`/`SHOW`/`DESCRIBE`/`EXPLAIN`/`WITH` (`:799,895`) is a
      valid allowlist on both engines and stays dialect-independent. A regression test keeps writes
      rejected on the MySQL path.
- [x] **Docs corrected** — the plugin module doc's *Dialects* section
      (`plugins/sql/src/main.rs:19-26`) no longer calls MySQL an unsupported residual.
      *(The story also called for regenerating "the website ops mirror". There is none to regenerate:
      `website/docs/language/ops.md` mirrors flux-lang **built-in** ops, not plugin ops, and no
      website page documents the `sql` plugin. The acceptance item was written on a false premise.)*
- [x] Gate green: both workspaces — build, test, clippy `-D warnings`, fmt, `flux-codegate`.

## Progress
- **Done (2026-07-28).** `require_postgres()` deleted; each op branches on `target.dialect` for its
  SQL. `mysql_and_sqlite_route_to_clear_errors` split — sqlite keeps its error, mariadb now succeeds.
- **Beyond the written acceptance,** two things the story did not anticipate:
  - **A SQL-injection gap.** `pg_lit()` escapes only `'`. MySQL treats `\` as an escape character
    inside string literals (Postgres, with `standard_conforming_strings`, does not), so a schema or
    table name containing `\'` would have terminated the literal. New `my_lit()` escapes the
    backslash first, then the quote; a test pins the difference between the two escapers.
  - **A silent cross-table bug.** MySQL names *every* primary key `PRIMARY`, so constraint names are
    unique per table, not per schema. Porting the Postgres PK join (constraint_name +
    constraint_schema) would have matched every table's PK in the schema at once. The MySQL path
    reads `key_column_usage` scoped by table instead.
- **Documented divergences** (not oversights): `database.list` returns only `kind: "database"` on
  MySQL, since schema and database are one object there; `index.list` omits `definition` rather than
  synthesizing a `CREATE INDEX` string MySQL never reported (no `pg_get_indexdef`), keeping
  `columns`/`unique`/`primary`/`method`.
- **Tests (7):** `sql.test` on mariadb, table.list asserting `information_schema` and *no*
  `pg_catalog` on the wire, database.list's database-only shape, table.show's MySQL FK/PK queries,
  index.list grouping three column-rows into two indexes with `seq_in_index` order preserved, and the
  escaper test. Plugin module docs updated; SQLite's message is now a shared constant.
- **Post-review fixes (`/code-review`):**
  - **Unaliased projections were a silent-empty-output risk.** The Postgres arms all alias
    (`AS table_schema`); the new MySQL arms did not. `cell()` matches labels **case-sensitively** and
    returns `""` on a miss, and MySQL declares its `information_schema` columns uppercase — so a
    server labelling them `TABLE_NAME` would have made `table.list` return `{count: 0, tables: []}`,
    indistinguishable from an empty database. The hand-crafted-frame tests build matching labels by
    construction and *could not* have caught it. Every MySQL projection is now explicitly aliased,
    and `cell()` documents why.
  - **The model-facing op description was never updated** — the one surface this story's acceptance
    named ("the op description states which, per dialect"). The module doc pointed readers at it
    while it still promised `kind: "schema"` entries the MySQL arm never emits. Fixed.
  - **`my_lit` now documents its two invisible dependencies:** an ASCII-compatible connection charset
    (fixed host-side as `utf8mb4` and never reported back — change one, check the other) and
    `NO_BACKSLASH_ESCAPES` being off.
  - **Known limitation recorded, not silently fixed:** MySQL 8.0.13+ functional indexes report
    `COLUMN_NAME` NULL with the expression in a separate `EXPRESSION` column, so such parts are
    dropped from `index.list`. Reading `EXPRESSION` is *not* a safe fix — MariaDB, this epic's
    primary target, has no such column, so selecting it would hard-fail every `index.list` on the
    main supported engine. Needs per-engine version detection; documented in `group_mysql_indexes`.
- Both workspace gates green: build, tests (128 root + 24 plugin binaries), clippy `-D warnings`,
  fmt, `flux-codegate`.
- **Live interop VERIFIED 2026-07-28** against MySQL 5.7.44, which retired two of the risks the
  review raised:
  - The **aliasing fix works on a real server** — `table.list` returned all 173 tables with populated
    `name`/`schema`/`row_estimate`, the failure mode (silent empty output on uppercase labels) that
    hand-crafted frames structurally could not test.
  - The **table-scoped PK read is correct** — `table.show` on `vicidial_users` returned exactly
    `['user_id']`, not every table's `PRIMARY`.
  - Multi-column index grouping held on real data, including two indexes over the *same* pair of
    columns in opposite order (`list_phone` → `[list_id, phone_number]`, `phone_list` →
    `[phone_number, list_id]`), each preserving its own `seq_in_index` order.
  - `database.list` returned only `kind: "database"` entries with the four MySQL system schemas
    filtered, as documented.
- **Residual:** the verified server was MySQL 5.7, not literally MariaDB.

## Notes
- SQLite is untouched and stays unsupported by design: a local file, and plugins have no filesystem
  capability (`conn.*` is sockets only, `:527`).
- The plugin stays **read-only** on every dialect. This story does not add writes.
- Design: [mariadb-support.md](../designs/mariadb-support.md).
