//! The Postgres backend — a shared, durable, multi-writer-safe event store for server deployments.
//!
//! One ordered `events` log holds every fact; a small `streams` registry mints the `s_<n>` session
//! ids and serves the session-list read model (it is rebuildable from the log). A *stream* is one
//! session, so messages, run events, and turn telemetry interleave in one causal order — the whole
//! point of unifying the three old logs. The default backend is embedded SQLite ([`super::sqlite`]);
//! this arm plugs into the same [`super::EventBackend`] seam behind the `postgres` feature and keeps
//! the public [`EventStore`](super::EventStore) API byte-identical.
//!
//! ## Three deliberate mappings (do not "improve" these away)
//!
//! - **`BIGSERIAL` preserves the id contracts.** Session ids are `s_<streams.n>` ([`super::parse_id`])
//!   and a turn id is its `TurnStarted` event's `global_seq` — both only require a monotone,
//!   never-reused i64. `BIGSERIAL` gives exactly that, so `INSERT … RETURNING n` / `RETURNING
//!   global_seq` replaces SQLite's `last_insert_rowid()` with no change to the contracts.
//!
//! - **`payload` is `TEXT`, not `jsonb` — on purpose.** The payload is opaque to SQL (`kind` is its
//!   own column; no query ever reaches into the payload), and `TEXT` guarantees a *byte-exact*
//!   round-trip of the adjacently-tagged serde JSON (`{"kind":…,"data":…}`) written by
//!   `serde_json::to_string` and read back by [`super::decode_all`]. `jsonb` would re-serialize
//!   (normalize key order, collapse duplicates) for zero query benefit — breaking the byte-identical
//!   guarantee the store promises across backends. Reads produce [`super::RawEvent`] tuples and hand
//!   them to the shared [`super::decode_all`], so the serde contract can never drift from SQLite.
//!
//! - **Append serialization via a transaction-scoped advisory lock.** SQLite serializes appends with
//!   an in-process `Mutex` + `BEGIN IMMEDIATE`. Postgres replaces both with a per-stream
//!   `pg_advisory_xact_lock(hashtextextended($stream, 0))` taken as the first statement of the append
//!   transaction — strictly stronger, because it also serializes appends **across processes and
//!   replicas** (the thing an embedded file structurally cannot do), and it applies to ad-hoc streams
//!   that never touch the registry too. `UNIQUE(stream, stream_seq)` stays as the durable backstop.
//!
//! sqlx caches prepared statements per pooled connection, so the hot read paths are covered for free.
//! Every method wraps its async body in [`flux_pg::PgHandle::block_on`], the panic-safe sync↔async
//! bridge, so these `&self` primitives stay callable from every context flux runs in.

use std::sync::Arc;

use flux_core::{Error, Result};
use flux_pg::{sqlx, PgHandle};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};

use super::{decode_all, now_ms, parse_id, EventBackend, RawEvent, SessionInfo, SessionSummary};
use crate::context::EventContext;
use crate::kind::{EventKind, NewEvent, StoredEvent};

/// Map any sqlx error into the shared flux error type (mirrors `sqlite.rs`'s `map_sql`).
fn map_sql<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(format!("event store: {e}"))
}

/// The schema, created inline on `connect` (matching the house no-migration-framework style —
/// Postgres's native `IF NOT EXISTS` removes SQLite's `PRAGMA table_info` probing). `payload` is
/// `TEXT` on purpose (see the module header); `BIGSERIAL` preserves the `s_<n>` / turn-id contracts.
const SCHEMA_DDL: &str = "\
CREATE TABLE IF NOT EXISTS streams (
    n              BIGSERIAL PRIMARY KEY,
    model          TEXT   NOT NULL DEFAULT '',
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL,
    last_seq       BIGINT NOT NULL DEFAULT -1,
    msg_count      BIGINT NOT NULL DEFAULT 0,
    account        TEXT,
    agent_id       TEXT,
    agent_version  TEXT,
    correlation_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_streams_account ON streams(account);
CREATE TABLE IF NOT EXISTS events (
    global_seq     BIGSERIAL PRIMARY KEY,
    stream         TEXT    NOT NULL,
    stream_seq     BIGINT  NOT NULL,
    id             TEXT    NOT NULL,
    kind           TEXT    NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1,
    ts             BIGINT  NOT NULL,
    payload        TEXT    NOT NULL,
    turn_id        BIGINT,
    UNIQUE(id),
    UNIQUE(stream, stream_seq)
);
CREATE INDEX IF NOT EXISTS idx_events_stream_kind ON events(stream, kind, stream_seq);
CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);
CREATE INDEX IF NOT EXISTS idx_events_turn ON events(stream, turn_id) WHERE turn_id IS NOT NULL;
";

/// The `streams` columns a [`SessionSummary`] reads, in [`row_to_summary`] order. Shared by
/// `list` and `list_for_account` so the two never drift.
const SUMMARY_COLS: &str =
    "n, model, created_at, updated_at, msg_count, account, agent_id, agent_version, correlation_id";

/// The append-only event store, backed by a shared Postgres. Appends serialize per-stream via a
/// transaction-scoped advisory lock; `UNIQUE(id)` and `UNIQUE(stream, stream_seq)` are the durable
/// backstops. Holds an [`Arc<PgHandle>`] (pool + the sync↔async bridge).
pub(crate) struct PgEvents {
    handle: Arc<PgHandle>,
}

impl PgEvents {
    /// Reuse an already-built handle, create the schema inline once, and return the backend.
    pub(crate) fn connect(handle: Arc<PgHandle>) -> Result<Self> {
        let pool = handle.pool().clone();
        // `raw_sql` runs the multi-statement DDL batch via the simple-query protocol.
        handle.block_on(async move {
            sqlx::raw_sql(SCHEMA_DDL)
                .execute(&pool)
                .await
                .map_err(map_sql)
        })?;
        Ok(Self { handle })
    }
}

/// Decode a `streams` row selected as [`SUMMARY_COLS`] into a [`SessionSummary`].
fn row_to_summary(row: &PgRow) -> Result<SessionSummary> {
    let n: i64 = row.try_get(0).map_err(map_sql)?;
    Ok(SessionSummary {
        id: format!("s_{n}"),
        model: row.try_get(1).map_err(map_sql)?,
        created_at_ms: row.try_get(2).map_err(map_sql)?,
        updated_at_ms: row.try_get(3).map_err(map_sql)?,
        // msg_count is BIGINT (i64) → usize.
        messages: row.try_get::<i64, _>(4).map_err(map_sql)? as usize,
        context: EventContext {
            account: row.try_get(5).map_err(map_sql)?,
            agent_id: row.try_get(6).map_err(map_sql)?,
            agent_version: row.try_get(7).map_err(map_sql)?,
            correlation_id: row.try_get(8).map_err(map_sql)?,
        },
    })
}

/// Turn a batch of event rows (selected as `global_seq, stream_seq, id, schema_version, ts, payload,
/// turn_id`) into [`RawEvent`] tuples for the shared [`decode_all`]. `schema_version` is `INTEGER`
/// (i32) in SQL but `u32` in [`RawEvent`], so it is decoded as `i32` and widened.
fn rows_to_raw(rows: Vec<PgRow>) -> Result<Vec<RawEvent>> {
    rows.iter()
        .map(|r| {
            Ok((
                r.try_get::<i64, _>(0).map_err(map_sql)?,
                r.try_get::<i64, _>(1).map_err(map_sql)?,
                r.try_get::<String, _>(2).map_err(map_sql)?,
                r.try_get::<i32, _>(3).map_err(map_sql)? as u32,
                r.try_get::<i64, _>(4).map_err(map_sql)?,
                r.try_get::<String, _>(5).map_err(map_sql)?,
                r.try_get::<Option<i64>, _>(6).map_err(map_sql)?,
            ))
        })
        .collect()
}

/// The run context tagged on a stream's registry row, or empty for ad-hoc / unknown streams. All
/// events in a stream share one context, so reads look it up once and stamp every event. The
/// context is immutable session metadata (set once at creation), so a plain pooled read is correct.
async fn read_context(pool: &PgPool, stream: &str) -> Result<EventContext> {
    let Ok(n) = parse_id(stream) else {
        return Ok(EventContext::default());
    };
    let row = sqlx::query(
        "SELECT account, agent_id, agent_version, correlation_id FROM streams WHERE n = $1",
    )
    .bind(n)
    .fetch_optional(pool)
    .await
    .map_err(map_sql)?;
    let Some(row) = row else {
        return Ok(EventContext::default());
    };
    Ok(EventContext {
        account: row.try_get(0).map_err(map_sql)?,
        agent_id: row.try_get(1).map_err(map_sql)?,
        agent_version: row.try_get(2).map_err(map_sql)?,
        correlation_id: row.try_get(3).map_err(map_sql)?,
    })
}

/// Insert one event row on the active transaction (no registry update — callers handle that). Mints
/// a ULID id when the event has none. `ctx` is the stream's run context, stamped onto the returned
/// [`StoredEvent`] (it lives on the registry, not the event row, so it is surfaced, not persisted
/// here). `INSERT … RETURNING global_seq` replaces SQLite's `last_insert_rowid()`.
async fn insert_event(
    conn: &mut PgConnection,
    stream: &str,
    ev: &NewEvent,
    stream_seq: i64,
    ctx: &EventContext,
) -> Result<StoredEvent> {
    let id = ev
        .id
        .clone()
        .unwrap_or_else(|| ulid::Ulid::new().to_string());
    let ts = now_ms();
    let kind_tag = ev.kind.kind_tag().to_string();
    let payload = serde_json::to_string(&ev.kind)?;
    let global_seq: i64 = sqlx::query_scalar(
        "INSERT INTO events \
         (stream, stream_seq, id, kind, schema_version, ts, payload, turn_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING global_seq",
    )
    .bind(stream.to_string())
    .bind(stream_seq)
    .bind(id.clone())
    .bind(kind_tag)
    // schema_version is INTEGER (int4) in SQL; RawEvent/StoredEvent carry it as u32.
    .bind(ev.schema_version as i32)
    .bind(ts)
    .bind(payload)
    .bind(ev.turn_id)
    .fetch_one(conn)
    .await
    .map_err(map_sql)?;
    Ok(StoredEvent {
        global_seq,
        stream: stream.to_string(),
        stream_seq,
        id,
        turn_id: ev.turn_id,
        schema_version: ev.schema_version,
        ts_ms: ts,
        kind: ev.kind.clone(),
        context: ctx.clone(),
    })
}

/// Fetch a single event by its stable id (for idempotent retries).
async fn load_by_id(pool: &PgPool, id: &str) -> Result<Option<StoredEvent>> {
    let row = sqlx::query(
        "SELECT global_seq, stream, stream_seq, schema_version, ts, payload, turn_id \
         FROM events WHERE id = $1",
    )
    .bind(id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(map_sql)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let global_seq: i64 = row.try_get(0).map_err(map_sql)?;
    let stream: String = row.try_get(1).map_err(map_sql)?;
    let stream_seq: i64 = row.try_get(2).map_err(map_sql)?;
    let schema_version: i32 = row.try_get(3).map_err(map_sql)?;
    let ts: i64 = row.try_get(4).map_err(map_sql)?;
    let payload: String = row.try_get(5).map_err(map_sql)?;
    let turn_id: Option<i64> = row.try_get(6).map_err(map_sql)?;
    let kind = serde_json::from_str(&payload)?;
    let context = read_context(pool, &stream).await?;
    Ok(Some(StoredEvent {
        global_seq,
        stream,
        stream_seq,
        id: id.to_string(),
        turn_id,
        schema_version: schema_version as u32,
        ts_ms: ts,
        kind,
        context,
    }))
}

/// Collect a whole-stream delete (registry rows + their events) for a set of registry `n`s inside
/// one transaction, batched with `WHERE … = ANY($1)`. Shared by `prune_empty`, `prune_inactive`,
/// and `prune_older_than`, which differ only in which `n`s they select. Returns the count removed.
async fn delete_streams(pool: &PgPool, ns: Vec<i64>) -> Result<usize> {
    if ns.is_empty() {
        return Ok(0);
    }
    let count = ns.len();
    let streams: Vec<String> = ns.iter().map(|n| format!("s_{n}")).collect();
    let mut tx = pool.begin().await.map_err(map_sql)?;
    sqlx::query("DELETE FROM events WHERE stream = ANY($1)")
        .bind(streams)
        .execute(&mut *tx)
        .await
        .map_err(map_sql)?;
    sqlx::query("DELETE FROM streams WHERE n = ANY($1)")
        .bind(ns)
        .execute(&mut *tx)
        .await
        .map_err(map_sql)?;
    tx.commit().await.map_err(map_sql)?;
    Ok(count)
}

impl EventBackend for PgEvents {
    fn create_session_with_context(&self, model: &str, ctx: &EventContext) -> Result<String> {
        let pool = self.handle.pool().clone();
        let model = model.to_string();
        let ctx = ctx.clone();
        self.handle.block_on(async move {
            let ts = now_ms();
            let mut tx = pool.begin().await.map_err(map_sql)?;
            let n: i64 = sqlx::query_scalar(
                "INSERT INTO streams \
                 (model, created_at, updated_at, last_seq, msg_count, \
                  account, agent_id, agent_version, correlation_id) \
                 VALUES ($1, $2, $2, 0, 0, $3, $4, $5, $6) RETURNING n",
            )
            .bind(model.clone())
            .bind(ts)
            .bind(ctx.account.clone())
            .bind(ctx.agent_id.clone())
            .bind(ctx.agent_version.clone())
            .bind(ctx.correlation_id.clone())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sql)?;
            let stream = format!("s_{n}");
            let ev = NewEvent::new(EventKind::SessionStarted {
                model: model.clone(),
            });
            insert_event(&mut tx, &stream, &ev, 0, &ctx).await?;
            tx.commit().await.map_err(map_sql)?;
            Ok(stream)
        })
    }

    fn latest_session(&self) -> Result<Option<String>> {
        let pool = self.handle.pool().clone();
        self.handle.block_on(async move {
            let n: Option<i64> =
                sqlx::query_scalar("SELECT n FROM streams ORDER BY n DESC LIMIT 1")
                    .fetch_optional(&pool)
                    .await
                    .map_err(map_sql)?;
            Ok(n.map(|n| format!("s_{n}")))
        })
    }

    fn info(&self, stream: &str) -> Result<SessionInfo> {
        let pool = self.handle.pool().clone();
        let stream = stream.to_string();
        self.handle.block_on(async move {
            let n = parse_id(&stream)?;
            let row = sqlx::query(
                "SELECT model, created_at, updated_at, account, agent_id, agent_version, \
                 correlation_id FROM streams WHERE n = $1",
            )
            .bind(n)
            .fetch_optional(&pool)
            .await
            .map_err(map_sql)?;
            let Some(row) = row else {
                return Err(Error::Other(format!("session {stream} not found")));
            };
            Ok(SessionInfo {
                id: stream.clone(),
                model: row.try_get(0).map_err(map_sql)?,
                created_at_ms: row.try_get(1).map_err(map_sql)?,
                updated_at_ms: row.try_get(2).map_err(map_sql)?,
                context: EventContext {
                    account: row.try_get(3).map_err(map_sql)?,
                    agent_id: row.try_get(4).map_err(map_sql)?,
                    agent_version: row.try_get(5).map_err(map_sql)?,
                    correlation_id: row.try_get(6).map_err(map_sql)?,
                },
            })
        })
    }

    fn list(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let pool = self.handle.pool().clone();
        self.handle.block_on(async move {
            let rows = sqlx::query(&format!(
                "SELECT {SUMMARY_COLS} FROM streams ORDER BY updated_at DESC, n DESC LIMIT $1"
            ))
            .bind(limit as i64)
            .fetch_all(&pool)
            .await
            .map_err(map_sql)?;
            rows.iter().map(row_to_summary).collect()
        })
    }

    fn list_for_account(&self, account: &str, limit: usize) -> Result<Vec<SessionSummary>> {
        let pool = self.handle.pool().clone();
        let account = account.to_string();
        self.handle.block_on(async move {
            let rows = sqlx::query(&format!(
                "SELECT {SUMMARY_COLS} FROM streams \
                 WHERE account = $1 ORDER BY updated_at DESC, n DESC LIMIT $2"
            ))
            .bind(account)
            .bind(limit as i64)
            .fetch_all(&pool)
            .await
            .map_err(map_sql)?;
            rows.iter().map(row_to_summary).collect()
        })
    }

    fn account_streams(&self, account: &str) -> Result<Vec<String>> {
        let pool = self.handle.pool().clone();
        let account = account.to_string();
        self.handle.block_on(async move {
            let ns: Vec<i64> = sqlx::query_scalar(
                "SELECT n FROM streams WHERE account = $1 ORDER BY updated_at DESC, n DESC",
            )
            .bind(account)
            .fetch_all(&pool)
            .await
            .map_err(map_sql)?;
            Ok(ns.into_iter().map(|n| format!("s_{n}")).collect())
        })
    }

    fn find_correlated(&self, correlation_id: &str, agent_id: &str) -> Result<Option<String>> {
        let pool = self.handle.pool().clone();
        let correlation_id = correlation_id.to_string();
        let agent_id = agent_id.to_string();
        self.handle.block_on(async move {
            let n: Option<i64> = sqlx::query_scalar(
                "SELECT n FROM streams WHERE correlation_id = $1 AND agent_id = $2 \
                 ORDER BY n DESC LIMIT 1",
            )
            .bind(correlation_id)
            .bind(agent_id)
            .fetch_optional(&pool)
            .await
            .map_err(map_sql)?;
            Ok(n.map(|n| format!("s_{n}")))
        })
    }

    fn find_correlated_in_realm(
        &self,
        correlation_id: &str,
        agent_id: &str,
        realm: &str,
    ) -> Result<Option<String>> {
        let pool = self.handle.pool().clone();
        let correlation_id = correlation_id.to_string();
        let agent_id = agent_id.to_string();
        let realm = realm.to_string();
        self.handle.block_on(async move {
            let n: Option<i64> = sqlx::query_scalar(
                "SELECT n FROM streams WHERE correlation_id = $1 AND agent_id = $2 \
                 AND account = $3 ORDER BY n DESC LIMIT 1",
            )
            .bind(correlation_id)
            .bind(agent_id)
            .bind(realm)
            .fetch_optional(&pool)
            .await
            .map_err(map_sql)?;
            Ok(n.map(|n| format!("s_{n}")))
        })
    }

    fn children_of(&self, stream: &str) -> Result<Vec<String>> {
        let pool = self.handle.pool().clone();
        let stream = stream.to_string();
        self.handle.block_on(async move {
            let ns: Vec<i64> = sqlx::query_scalar(
                "SELECT n FROM streams WHERE correlation_id = $1 ORDER BY n ASC",
            )
            .bind(stream)
            .fetch_all(&pool)
            .await
            .map_err(map_sql)?;
            Ok(ns.into_iter().map(|n| format!("s_{n}")).collect())
        })
    }

    fn prune_empty(&self) -> Result<usize> {
        let pool = self.handle.pool().clone();
        self.handle.block_on(async move {
            let ns: Vec<i64> = sqlx::query_scalar("SELECT n FROM streams WHERE msg_count = 0")
                .fetch_all(&pool)
                .await
                .map_err(map_sql)?;
            delete_streams(&pool, ns).await
        })
    }

    fn prune_inactive(&self, agent_id: &str, cutoff_ms: i64) -> Result<usize> {
        let pool = self.handle.pool().clone();
        let agent_id = agent_id.to_string();
        self.handle.block_on(async move {
            let ns: Vec<i64> =
                sqlx::query_scalar("SELECT n FROM streams WHERE agent_id = $1 AND updated_at < $2")
                    .bind(agent_id)
                    .bind(cutoff_ms)
                    .fetch_all(&pool)
                    .await
                    .map_err(map_sql)?;
            delete_streams(&pool, ns).await
        })
    }

    fn prune_older_than(&self, cutoff_ms: i64) -> Result<usize> {
        // D-75: the tag-agnostic sibling of `prune_inactive` — WHERE `updated_at` alone.
        let pool = self.handle.pool().clone();
        self.handle.block_on(async move {
            let ns: Vec<i64> = sqlx::query_scalar("SELECT n FROM streams WHERE updated_at < $1")
                .bind(cutoff_ms)
                .fetch_all(&pool)
                .await
                .map_err(map_sql)?;
            delete_streams(&pool, ns).await
        })
    }

    fn append(&self, stream: &str, ev: NewEvent) -> Result<StoredEvent> {
        let pool = self.handle.pool().clone();
        let stream = stream.to_string();
        self.handle.block_on(async move {
            // Idempotent retry: a caller-supplied id that already exists is a no-op returning the
            // prior event. `UNIQUE(id)` is the durable backstop if two appenders race this window.
            if let Some(id) = &ev.id {
                if let Some(existing) = load_by_id(&pool, id).await? {
                    return Ok(existing);
                }
            }
            // The run context is immutable session metadata; read it once (a committed, separate
            // read) and stamp the stored event.
            let ctx = read_context(&pool, &stream).await?;
            let mut tx = pool.begin().await.map_err(map_sql)?;
            // Serialize appends to this stream — across processes/replicas, and for ad-hoc streams
            // that never touch the registry too. Transaction-scoped: released at commit/rollback.
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(stream.clone())
                .execute(&mut *tx)
                .await
                .map_err(map_sql)?;
            let next_seq: i64 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(stream_seq), -1) + 1 FROM events WHERE stream = $1",
            )
            .bind(stream.clone())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sql)?;
            let stored = insert_event(&mut tx, &stream, &ev, next_seq, &ctx).await?;
            // Maintain the session registry — but only for real `s_<n>` sessions. The log accepts
            // any stream string (the interpreter writes run events under ad-hoc ids), so a
            // non-session stream simply has no registry row to update.
            if let Ok(n) = parse_id(&stream) {
                let model_opt = match &ev.kind {
                    EventKind::SessionStarted { model } | EventKind::ModelChanged { model } => {
                        Some(model.clone())
                    }
                    _ => None,
                };
                sqlx::query(
                    "UPDATE streams SET updated_at = $1, last_seq = $2, \
                     model = COALESCE($3, model) WHERE n = $4",
                )
                .bind(stored.ts_ms)
                .bind(next_seq)
                .bind(model_opt)
                .bind(n)
                .execute(&mut *tx)
                .await
                .map_err(map_sql)?;
                // Keep msg_count equal to the live conversation length (so `list` matches a replay).
                match &ev.kind {
                    EventKind::Message(_) => {
                        sqlx::query("UPDATE streams SET msg_count = msg_count + 1 WHERE n = $1")
                            .bind(n)
                            .execute(&mut *tx)
                            .await
                            .map_err(map_sql)?;
                    }
                    EventKind::Compacted { messages } => {
                        sqlx::query("UPDATE streams SET msg_count = $1 WHERE n = $2")
                            .bind(messages.len() as i64)
                            .bind(n)
                            .execute(&mut *tx)
                            .await
                            .map_err(map_sql)?;
                    }
                    _ => {}
                }
            }
            tx.commit().await.map_err(map_sql)?;
            Ok(stored)
        })
    }

    fn load_stream(&self, stream: &str, after_seq: Option<i64>) -> Result<Vec<StoredEvent>> {
        let pool = self.handle.pool().clone();
        let stream = stream.to_string();
        self.handle.block_on(async move {
            let ctx = read_context(&pool, &stream).await?;
            let after = after_seq.unwrap_or(-1);
            let rows = sqlx::query(
                "SELECT global_seq, stream_seq, id, schema_version, ts, payload, turn_id \
                 FROM events WHERE stream = $1 AND stream_seq > $2 ORDER BY stream_seq",
            )
            .bind(stream.clone())
            .bind(after)
            .fetch_all(&pool)
            .await
            .map_err(map_sql)?;
            decode_all(&stream, &ctx, rows_to_raw(rows)?)
        })
    }

    fn load_by_kind(&self, stream: &str, kind: &str) -> Result<Vec<StoredEvent>> {
        let pool = self.handle.pool().clone();
        let stream = stream.to_string();
        let kind = kind.to_string();
        self.handle.block_on(async move {
            let ctx = read_context(&pool, &stream).await?;
            let rows = sqlx::query(
                "SELECT global_seq, stream_seq, id, schema_version, ts, payload, turn_id \
                 FROM events WHERE stream = $1 AND kind = $2 ORDER BY stream_seq",
            )
            .bind(stream.clone())
            .bind(kind)
            .fetch_all(&pool)
            .await
            .map_err(map_sql)?;
            decode_all(&stream, &ctx, rows_to_raw(rows)?)
        })
    }

    fn conversation_delta(&self, stream: &str, after_seq: i64) -> Result<Vec<StoredEvent>> {
        let pool = self.handle.pool().clone();
        let stream = stream.to_string();
        self.handle.block_on(async move {
            let ctx = read_context(&pool, &stream).await?;
            // sqlx caches prepared statements per pooled connection, so this per-round query needs
            // no special handling (the incremental cache means it usually returns nothing).
            let rows = sqlx::query(
                "SELECT global_seq, stream_seq, id, schema_version, ts, payload, turn_id \
                 FROM events WHERE stream = $1 AND stream_seq > $2 \
                 AND kind IN ('message', 'compacted') ORDER BY stream_seq",
            )
            .bind(stream.clone())
            .bind(after_seq)
            .fetch_all(&pool)
            .await
            .map_err(map_sql)?;
            decode_all(&stream, &ctx, rows_to_raw(rows)?)
        })
    }

    fn load_turn(&self, stream: &str, turn_id: i64) -> Result<Vec<StoredEvent>> {
        let pool = self.handle.pool().clone();
        let stream = stream.to_string();
        self.handle.block_on(async move {
            let ctx = read_context(&pool, &stream).await?;
            let rows = sqlx::query(
                "SELECT global_seq, stream_seq, id, schema_version, ts, payload, turn_id \
                 FROM events WHERE stream = $1 AND (global_seq = $2 OR turn_id = $2) \
                 ORDER BY stream_seq",
            )
            .bind(stream.clone())
            .bind(turn_id)
            .fetch_all(&pool)
            .await
            .map_err(map_sql)?;
            decode_all(&stream, &ctx, rows_to_raw(rows)?)
        })
    }

    fn head_seq(&self, stream: &str) -> Result<i64> {
        let pool = self.handle.pool().clone();
        let stream = stream.to_string();
        self.handle.block_on(async move {
            sqlx::query_scalar("SELECT COALESCE(MAX(stream_seq), -1) FROM events WHERE stream = $1")
                .bind(stream)
                .fetch_one(&pool)
                .await
                .map_err(map_sql)
        })
    }

    fn all_streams(&self) -> Result<Vec<String>> {
        let pool = self.handle.pool().clone();
        self.handle.block_on(async move {
            let ns: Vec<i64> = sqlx::query_scalar("SELECT n FROM streams ORDER BY n")
                .fetch_all(&pool)
                .await
                .map_err(map_sql)?;
            Ok(ns.into_iter().map(|n| format!("s_{n}")).collect())
        })
    }

    fn streams_with_correlation(&self) -> Result<Vec<(i64, Option<String>)>> {
        let pool = self.handle.pool().clone();
        self.handle.block_on(async move {
            let rows = sqlx::query("SELECT n, correlation_id FROM streams ORDER BY n")
                .fetch_all(&pool)
                .await
                .map_err(map_sql)?;
            rows.iter()
                .map(|r| {
                    Ok((
                        r.try_get::<i64, _>(0).map_err(map_sql)?,
                        r.try_get::<Option<String>, _>(1).map_err(map_sql)?,
                    ))
                })
                .collect()
        })
    }
}
