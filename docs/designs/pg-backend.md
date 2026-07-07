# Postgres storage backend (epic)

**Status:** proposed (2026-07-07) · **Pillar:** Core · **Epic slug:** `pg-backend`

flux's durable persistence today is embedded SQLite — `rusqlite` with the bundled engine, one
local file per store, one process, one writer. That is the right default for the CLI and for
demos, and it stays the default. But server deployments of flux runtimes (multi-tenant
managed-agent services, anything running >1 replica behind a load balancer, anything on a
platform where local disk is ephemeral or network-mounted) need what an embedded file cannot
give: a shared, durable, multi-writer-safe backend with operational tooling (backups, failover,
managed offerings). This epic adds **Postgres** as a second backend for the two persistence
primitives consumers actually deploy against — the unified event log (`flux-events::EventStore`)
and the datasource records store (`flux-capabilities::DatasourceBackend`) — behind opt-in cargo
features, with the default build staying rusqlite-only and dependency-free of any network DB.

Postgres specifically because it covers every need with first-class primitives: transactional
advisory locks for cross-replica append serialization, `tsvector`/GIN for the FTS5-equivalent
keyword search, `BIGSERIAL` for the rowid-derived id contracts, and a future path to `pgvector`
for the vector store (explicitly out of scope here).

## Context — what exists, and the two gaps

- **`flux-events::EventStore`** (`crates/flux-events/src/store.rs`) is a **concrete struct**
  over `Mutex<rusqlite::Connection>` (WAL, `busy_timeout` 5s). It is the durable heart of a
  deployment: conversations, run traces, turn telemetry, and app-defined `EventKind::Custom`
  facts all ride one append-only log (`events` + `streams` tables). ~40 public methods, of
  which only **~20 touch SQL**; the rest are projections over the backend-neutral
  `RawEvent` row tuple (store.rs:973) and pure serde. There is **no trait over it** — a
  deliberate choice this epic preserves.
- **`flux-capabilities::DatasourceBackend`** (`datasource/mod.rs:51`) **is already a trait**
  (impls: `MemoryBackend`, `SqliteBackend`). The SQLite impl couples isolation to the
  filesystem: one DB file per scope. A Postgres impl is purely additive.
- **Out of scope, stated up front:** `FlowStore`/`ValueStore`/`DurableStore` (flow state is
  in-memory or CLI-local; the existing traits mean a Postgres impl can come later on demand),
  a `VectorStore` over pgvector (backlog note in the datasource story), MySQL or other
  engines, and any migration tooling for moving existing SQLite data (event-sourced data
  replays via `all_streams` → `load_stream` → `append` if ever needed — do not build it now).

## Architecture

### 1. One crate owns the driver: `flux-pg` (new, L1)

Everything Postgres-flavored funnels through one new crate so exactly one place owns the sqlx
dependency, the connection pool, and the sync↔async bridge. Registered at **L1** in the
`flux-codegate` layer map (precedent: `flux-a2a`, "a network client with no flux deps" — here:
deps on `flux-core` only). Dependencies: `sqlx` (features `runtime-tokio`, `postgres`,
`tls-rustls`; **no macros** — builds must never need a live database), `tokio`
(`rt-multi-thread`).

Core type:

```rust
pub struct PgHandle { rt: tokio::runtime::Runtime, pool: sqlx::PgPool }

impl PgHandle {
    /// Parse DSN, strip flux-owned params, build the pool on the handle's own runtime.
    pub fn connect(url: &str) -> Result<Arc<Self>>;
    /// Panic-safe sync bridge: run a future to completion from ANY calling context.
    pub fn block_on<T: Send>(&self, fut: impl Future<Output = T> + Send) -> T;
    pub fn pool(&self) -> &PgPool;
}
```

**The bridge is the load-bearing decision.** `EventStore`'s methods are sync and are called
from every context flux supports: plain threads (CLI), multi-thread tokio workers
(flux-server; consumers' async handlers), and current-thread runtimes (`#[tokio::test]`).
The naive bridges all panic somewhere in that matrix: the sync `postgres` crate does an
internal `Runtime::block_on` per call (panics "Cannot start a runtime from within a runtime"
on a tokio worker); `Handle::block_on` panics on worker threads; `block_in_place` panics on
current-thread runtimes. The one shape that works everywhere: **spawn the future onto the
handle's own dedicated runtime and block the caller on a plain `std::sync::mpsc` channel**
(`self.rt.spawn(async { tx.send(fut.await) })` + `rx.recv()`). A blocking `recv()` is legal
on any thread — the same "briefly blocks the worker" class as today's
`Mutex<Connection>` rusqlite calls, so this changes nothing about flux's blocking posture.
The module header must document why the naive bridges are wrong so nobody "simplifies" it.

**DSN contract.** `PgHandle::connect` accepts a full `postgres://user:pw@host:port/db?…` URL,
strips the flux-owned query params, and passes the remainder (e.g. `sslmode=require`) to
`sqlx::postgres::PgConnectOptions`. Flux-owned params:

| param | default | meaning |
|---|---|---|
| `pool_max` | 5 | max pool connections (serverless-friendly default) |
| `acquire_timeout_ms` | 5000 | pool acquire timeout |
| `schema` | *(none → `public`)* | `SET search_path` via `after_connect` hook; doubles as the test-isolation mechanism |

Userinfo must be percent-decoded; usernames/db names with hyphens must round-trip.

### 2. `EventStore`: internal backend enum, public API byte-identical

No public trait (23 consumer files hold `Arc<EventStore>` concretely; a trait churns every
signature for zero benefit). Instead:

```rust
pub struct EventStore { backend: Backend }
enum Backend {
    Sqlite(sqlite::SqliteEvents),                          // today's code, moved verbatim
    #[cfg(feature = "postgres")] Postgres(postgres::PgEvents),
}
```

`store.rs` becomes `store/mod.rs` + `store/sqlite.rs` (+ `store/postgres.rs`). The ~20 SQL
primitives (init/migrate, `create_session_with_context`, `latest_session`, `info`, `list`,
`list_for_account`, `account_streams`, `find_correlated`, `find_correlated_in_realm`,
`children_of`, `prune_empty`, `prune_inactive`, `append`, `load_stream`, `load_by_kind`,
`conversation_delta`, `load_turn`, `head_seq`, `load_by_id`, `all_streams`, plus a new
`streams_with_correlation()` splitting the SQL half out of `aggregate_streams`) delegate
per-backend; every projection/wrapper/serde path stays shared in `mod.rs` over `RawEvent`.
Constructors stay symmetric: `open(path)` / `in_memory()` /
`#[cfg(feature = "postgres")] open_postgres(Arc<PgHandle>)`.

### 3. Postgres schema for the event log

Created inline — `CREATE TABLE IF NOT EXISTS` + `ALTER TABLE … ADD COLUMN IF NOT EXISTS` —
matching the existing no-migration-framework convention (and Postgres's native `IF NOT
EXISTS` removes SQLite's `PRAGMA table_info` probing dance):

```sql
CREATE TABLE IF NOT EXISTS streams (
  n BIGSERIAL PRIMARY KEY, model TEXT NOT NULL DEFAULT '',
  created_at BIGINT NOT NULL, updated_at BIGINT NOT NULL,
  last_seq BIGINT NOT NULL DEFAULT -1, msg_count BIGINT NOT NULL DEFAULT 0,
  account TEXT, agent_id TEXT, agent_version TEXT, correlation_id TEXT);
CREATE INDEX IF NOT EXISTS idx_streams_account ON streams(account);
CREATE TABLE IF NOT EXISTS events (
  global_seq BIGSERIAL PRIMARY KEY, stream TEXT NOT NULL, stream_seq BIGINT NOT NULL,
  id TEXT NOT NULL, kind TEXT NOT NULL, schema_version INTEGER NOT NULL DEFAULT 1,
  ts BIGINT NOT NULL, payload TEXT NOT NULL, turn_id BIGINT,
  UNIQUE(id), UNIQUE(stream, stream_seq));
CREATE INDEX IF NOT EXISTS idx_events_stream_kind ON events(stream, kind, stream_seq);
CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);
CREATE INDEX IF NOT EXISTS idx_events_turn ON events(stream, turn_id) WHERE turn_id IS NOT NULL;
```

Three deliberate mappings:

- **`BIGSERIAL` preserves the id contracts.** Session ids are `s_<streams.n>` (`parse_id`)
  and `begin_turn` uses `global_seq` as the turn id — both only require monotone,
  never-reused i64s. `last_insert_rowid()` becomes `INSERT … RETURNING`.
- **`payload TEXT`, not `jsonb` — on purpose.** The payload is opaque to SQL (`kind` is its
  own column; no query reaches into the payload), and TEXT guarantees byte-exact round-trips
  of the adjacently-tagged serde JSON (`{"kind":…,"data":…}`). `jsonb` would re-serialize
  (key order, duplicate handling) for zero query benefit. Document in the module header so
  nobody "improves" it later.
- **Append serialization via advisory lock.** SQLite serializes appends with the in-process
  `Mutex` + `BEGIN IMMEDIATE`. Postgres replaces both with a per-stream transaction-scoped
  advisory lock — `SELECT pg_advisory_xact_lock(hashtextextended($stream, 0))` as the first
  statement of the append transaction. Strictly stronger: it also serializes appends **across
  processes/replicas** (the thing SQLite structurally cannot do), and it works for ad-hoc
  streams that never touch the registry. `UNIQUE(stream, stream_seq)` stays as the durable
  backstop. `prune_*` becomes the same two-statement delete inside one transaction, batched
  with `WHERE stream = ANY($1)`.

sqlx caches prepared statements per pooled connection, so the `prepare_cached` hot paths are
covered for free.

### 4. `PostgresBackend` for the datasource trait

Purely additive impl of the existing `DatasourceBackend`. The per-scope-file isolation
pattern maps to a **namespace column bound at construction**:

```rust
pub struct PostgresBackend { handle: Arc<PgHandle>, ns: String }
impl PostgresBackend {
    pub fn new(handle: Arc<PgHandle>, namespace: impl Into<String>) -> Result<Self>;
    /// The analog of scanning a directory of per-scope files.
    pub fn namespaces(handle: &Arc<PgHandle>, prefix: &str) -> Result<Vec<String>>;
}
```

```sql
CREATE TABLE IF NOT EXISTS ds_records (
  ns TEXT NOT NULL, source TEXT NOT NULL, entity TEXT NOT NULL, id TEXT NOT NULL,
  title TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '',
  links TEXT NOT NULL DEFAULT '[]', meta TEXT NOT NULL DEFAULT 'null',
  fts tsvector GENERATED ALWAYS AS (to_tsvector('simple', title || ' ' || body)) STORED,
  PRIMARY KEY (ns, source, entity, id));
CREATE INDEX IF NOT EXISTS idx_ds_records_fts ON ds_records USING GIN (fts);
```

`ns` is part of the PK and bound once at construction (never per-call) — isolation is
structural, exactly like `SqliteBackend::open(<scope>.db)`. Rejected alternatives:
schema-per-scope (DDL explosion, per-tenant search_path juggling) and prefixing the `source`
key (corrupts `row_to_record`'s `split_once('/')` plugin/instance parsing and leaks into
consumer-visible `Source` keys).

**Search parity with FTS5/bm25:** build the same OR-of-quoted-terms query string the SQLite
impl builds, feed it to `websearch_to_tsquery('simple', $1)` (supports OR + quoted phrases,
**never errors on malformed user input**), rank with `ts_rank(fts, query) DESC, id ASC`. The
`'simple'` config (no stemming/stopwords) is the closest match to FTS5's default unicode61
tokenizer. The generated column makes upsert a single `INSERT … ON CONFLICT (ns,source,
entity,id) DO UPDATE` — no FTS-mirror sync dance. `matched_fields` + `snippet` extract to a
shared private `datasource/text.rs` so both backends return identical `Match` shapes.
SQLite's `LIMIT -1` idiom becomes conditionally-built LIMIT/OFFSET clauses.

### 5. Feature gating & CI

`postgres = ["dep:flux-pg"]` in **flux-events** and **flux-capabilities**; rusqlite stays
unconditional. The default workspace build/test remains DB-free: Postgres tests are gated on
`TEST_POSTGRES_URL` (skip with an eprintln notice when unset), each test isolating itself in
a throwaway `?schema=t_<ulid>`. CI gains one additional job with `services: postgres:16`
running `cargo test -p flux-events -p flux-capabilities -p flux-pg --features postgres` +
`cargo clippy --features postgres -- -D warnings`; the main job is untouched.

The event-store conformance suite is the existing store tests with bodies extracted into
helpers taking `EventStore`, run twice (`mod sqlite_tests` as today; env-gated
`mod postgres_tests`), plus one PG-only test: concurrent appends to one stream from a
multi-thread tokio context — proving the advisory lock and the no-panic bridge in one shot.

## Stories & sequencing

| # | Story | Depends on | Est. |
|---|---|---|---|
| 1 | [D-71](../stories/D-71-flux-pg-bridge-crate.md) — `flux-pg` bridge crate | — | ~2d |
| 2 | [D-72](../stories/D-72-eventstore-backend-seam.md) — EventStore internal backend seam (pure refactor) | — (∥ D-71) | ~2–3d |
| 3 | [D-73](../stories/D-73-postgres-eventstore-backend.md) — Postgres EventStore backend + conformance + CI | D-71, D-72 | ~4–5d |
| 4 | [D-74](../stories/D-74-postgres-datasource-backend.md) — datasource `PostgresBackend` | D-71 (∥ D-72/73) | ~3–4d |
| 5 | [D-75](../stories/D-75-eventstore-prune-older-than.md) — `prune_older_than` retention primitive | D-72 (may fold into D-73) | ~0.5d |

Critical path: D-71 → D-72 → D-73. D-74 parallelizes after D-71. Every story boundary keeps
the default `cargo test --workspace` green with no database anywhere.
