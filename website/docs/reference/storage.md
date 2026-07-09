---
title: Storage & persistence
description: "How flux stores sessions, run traces, and resumability data across SQLite and Postgres backends."
---

# Storage & persistence

flux stores sessions, run traces, and usage telemetry as append-only events. Conversations, flow
execution, and per-turn metrics share one ordered log; every user-facing view is a projection over
that log.

Compaction appends a snapshot for faster projection. It does not rewrite earlier events.

The store lives in the `flux-events` crate behind a backend-independent API: embedded SQLite by
default, Postgres opt-in.

## Default backend: embedded SQLite

Zero configuration. The `flux` binary opens (creating on first use):

| File | Contents |
|---|---|
| `~/.flux/events.db` | the unified event log — conversations, run traces, turn telemetry |
| `~/.flux/flow.db` | flow-engine state — values, symbols, suspensions |
| `~/.flux/flows/` | reusable flows and composite ops (`.flux` files) — auto-loaded, discovered/run by `flow_list`/`flow_run` (legacy `~/.flux/ops/` still read) |

SQLite runs in WAL mode with a ~5 s busy timeout, so concurrent readers and multiple flux
processes sharing the same log coordinate safely.

## Postgres backend (opt-in)

For deployments that embed the flux crates and run **several processes or replicas against one
shared store**, `flux-events` ships a Postgres backend behind the `postgres` cargo feature. The
default build is entirely DB-free — no Postgres driver is compiled unless you enable the feature —
and the `flux` CLI binary always uses embedded SQLite.

Selection is programmatic: enable the feature and open the store over a connection handle instead
of a file path. There is no environment variable or config key.

```toml
# in an embedding application's Cargo.toml (dependency source elided)
flux-events = { path = "…", features = ["postgres"] }        # event log
flux-capabilities = { path = "…", features = ["postgres"] }  # datasource records (optional, see below)
```

```rust
let pg = flux_pg::PgHandle::connect("postgres://user:pw@db.example.com:5432/flux?pool_max=10")?;
let store = flux_events::EventStore::open_postgres(pg)?;
```

The public `EventStore` API is identical across backends, and event payloads round-trip
byte-exactly on both.

### DSN

A standard `postgres://` URL. flux strips three params of its own and hands the rest (host, user,
`sslmode`, …) to the driver unchanged:

| Param | Default | Meaning |
|---|---|---|
| `pool_max` | `5` | max pool connections |
| `acquire_timeout_ms` | `5000` | pool acquire timeout |
| `schema` | *(none → `public`)* | created if absent, then pinned as the `search_path` — an isolation knob |

Connecting is lazy: the first query opens the first connection, so no reachable database is needed
at construction time.

### What it gives you

- **Cross-process append serialization.** Appends to a stream take a transaction-scoped
  `pg_advisory_xact_lock` on that stream, so writes stay contiguous across processes and replicas —
  the thing an embedded file structurally cannot do.
- **Race-free first boot.** All bootstrap DDL runs in a transaction under one global advisory
  lock, so replicas cold-booting against a fresh database serialize instead of tripping over
  Postgres's non-atomic `IF NOT EXISTS`.
- **Credentials never logged.** `PgHandle::redacted_dsn()` is the one safe-to-print DSN form
  (userinfo masked, `password`-class query params masked — including sqlx's `?password=` form).
  The handle's `Debug` shows only this form, and a DSN parse error never echoes the raw string.

## Retention

`flux sessions --prune` deletes all zero-message (abandoned) sessions — see the
[CLI reference](../agent/cli.md). Time-based retention (`prune_older_than`,
`prune_adhoc_older_than`, `prune_inactive`) is available to embedders via the `EventStore` API on
both backends.

## Datasource records

The datasource layer (the agent's retrieval/RAG store) has the same split: the built-in backends
keep records in memory or in a per-scope SQLite file, and `flux-capabilities`'s `postgres` feature
adds `PostgresBackend` — one shared `ds_records` table where a namespace column, bound once at
construction, is part of the primary key. Full-text search uses a stored generated `tsvector`
column with a GIN index (`websearch_to_tsquery` + `ts_rank`), and search results are
shape-identical to the SQLite backend's.

## Related docs

- [Configuration](./config.md) — runtime settings that affect local sessions.
- [Time Machine](../agent/time-machine.md) — replay, fork, and diff recorded runs.
- [FlowClient](../sdk/flow-client.md) — deterministic flow execution over stored state.
