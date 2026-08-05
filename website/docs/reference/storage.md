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

### Relocating the store

| Override | Effect |
|---|---|
| `--store <dir>` | Store location for this invocation. flux exports it as `FLUX_STORE_DIR`, so subprocesses inherit the same store. |
| `FLUX_STORE_DIR` | The same thing set directly — useful for a shell session or a service unit. |

With neither set, the store is `$HOME/.flux`. Because `--store` works *by* setting
`FLUX_STORE_DIR`, the two are one mechanism rather than two layers.

Pointing `--store` at a scenario fixture written by `flux record` makes the session tools
(`replay`, `fork`, `diff`, `sessions`) work against a committed fixture with no fixture-specific
code path.

:::note `flux usage` does not follow `--store`
`flux usage` reads the **global** events store, resolved from `FLUX_HOME` (falling back to
`~/.flux`), and deliberately ignores `--store`. Cost roll-ups therefore stay account-wide even while
you are working against a relocated or fixture store — so a `--store` session's spend will not
appear where you might expect it.
:::

## Board and fleet state

Board persistence follows the selected scope and backend; there is no hidden universal board
database:

| Surface | Durable state |
|---|---|
| session board | board events in the selected `events.db`; `--store` and `FLUX_STORE_DIR` relocate them with the owning session |
| Track repository board | `docs/stories/`, its generated marker region, and authored vision/roadmap/decision/design documents in the repository |
| Markdown execution board | one item file per record under the declaration's configured `root` |
| federated workspace board | member `BoardRef`s and optional workspace planning documents; member stories remain in their owning repositories |
| memory board | process memory only; never a recovery source |

Fleet keeps its workspace authority beside the repositories it coordinates:

| Path | Contents |
|---|---|
| `.flux/fleet.toml` | closed fleet configuration: repositories, boards, gates, templates, limits, fences and worktree root |
| `.flux/fleet/state.json` | folded coordinator, worker, wave, handoff, review, gate and apply state |
| `.flux/fleet/events.ndjson` | append-only redacted fleet events and coordinator notes |
| `.flux/fleet/worktrees/` | the default integration/story worktree root; the configured `worktree_root` may point elsewhere |

`--store` does not relocate repository/workspace boards or the `.flux/fleet*` ledger. Back up the
authored board files, fleet manifest/state/events, and every Git commit or ref named by an active
wave together. Do not delete a fleet worktree merely because the ledger is durable: an unfinished
worker may still have uncommitted source there. Use `flux fleet status`, `worktrees`, and `inspect`
before recovery or cleanup.

## Postgres backend (opt-in)

For deployments that embed the flux crates and run **several processes or replicas against one
shared store**, `flux-events` ships a Postgres backend behind the `postgres` cargo feature. The
default build is **Postgres-driver-free**: it includes embedded SQLite, but no Postgres driver unless
you enable the feature. The `flux` CLI binary always uses embedded SQLite.

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

**Cross-session memory is exempt from time-based retention.** Memory entries live on their own
`memory:<scope-key>` streams, which carry no session-registry row and would otherwise look like
ordinary ad-hoc streams to `prune_adhoc_older_than`. They are skipped at any age: for a memory,
"no activity in months" means the knowledge settled, not that it is disposable, and the store is
its only copy. Removing a memory entry is a deliberate act — `EventStore::forget_memory`, which appends a
tombstone and keeps the history — never a side effect of a retention sweep.

## Datasource records

The datasource layer (the agent's retrieval/RAG store) has the same split: the built-in backends
keep records in memory or in a per-scope SQLite file, and `flux-capabilities`'s `postgres` feature
adds `PostgresBackend` — one shared `ds_records` table where a namespace column, bound once at
construction, is part of the primary key. Full-text search uses a stored generated `tsvector`
column with a GIN index (`websearch_to_tsquery` + `ts_rank`), and search results are
shape-identical to the SQLite backend's.

## Related docs

- [Configuration](./config.md) — runtime settings that affect local sessions.
- [Datasources](../agent/datasources.md) — the knowledge layer these records serve.
- [Boards](../coding/boards.md) — board scopes, profiles, backends, import/export and recovery authority.
- [Fleet](../coding/fleet.md) — durable worker, wave, handoff, gate and apply state.
- [Time Machine](../agent/time-machine.md) — replay, fork, and diff recorded runs.
- [FlowClient](../sdk/flow-client.md) — deterministic flow execution over stored state.
