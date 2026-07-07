//! The SQLite backend — the default, embedded, single-file event store.
//!
//! One ordered `events` log (WAL) holds every fact; a small `streams` registry mints the
//! `s_<n>` session ids and serves the session-list read model (it is rebuildable from the
//! log). A *stream* is one session, so messages, run events, and turn telemetry interleave
//! in one causal order — the whole point of unifying the three old logs.
//!
//! Everything here is SQLite-specific (rusqlite, WAL, `PRAGMA table_info` migration probing); the
//! backend-neutral projections, wrappers, and serde decode live in the parent [`super`] module,
//! over the shared [`super::RawEvent`] row tuple. This impl is the [`super::EventBackend`] the
//! default [`EventStore`](super::EventStore) is built on.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};

use flux_core::{Error, Result};

use super::{decode_all, now_ms, parse_id, EventBackend, RawEvent, SessionInfo, SessionSummary};
use crate::context::EventContext;
use crate::kind::{EventKind, NewEvent, StoredEvent};

fn map_sql<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(format!("event store: {e}"))
}

/// Begin a write transaction that takes the WAL write lock up front (`BEGIN IMMEDIATE`) (C-25). A
/// deferred transaction (rusqlite's default `unchecked_transaction`) takes a read lock first and only
/// tries to promote to the write lock at its first write — and SQLite refuses to run the busy handler
/// on that read→write upgrade (it could deadlock), returning `SQLITE_BUSY` immediately. So a
/// cross-process contender would still abort despite the `busy_timeout` set in [`SqliteEvents::open`].
/// Acquiring the write lock at `BEGIN` instead lets the busy handler wait the other writer out.
fn begin_write(conn: &Connection) -> Result<rusqlite::Transaction<'_>> {
    rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(map_sql)
}

/// The `streams` columns a [`SessionSummary`] reads, in `row_to_summary` order. Shared by
/// `list` and `list_for_account` so the two never drift.
const SUMMARY_COLS: &str =
    "n, model, created_at, updated_at, msg_count, account, agent_id, agent_version, correlation_id";

/// Decode a `streams` row selected as [`SUMMARY_COLS`] into a [`SessionSummary`].
fn row_to_summary(r: &rusqlite::Row) -> rusqlite::Result<SessionSummary> {
    let n: i64 = r.get(0)?;
    Ok(SessionSummary {
        id: format!("s_{n}"),
        model: r.get(1)?,
        created_at_ms: r.get(2)?,
        updated_at_ms: r.get(3)?,
        messages: r.get::<_, i64>(4)? as usize,
        context: EventContext {
            account: r.get(5)?,
            agent_id: r.get(6)?,
            agent_version: r.get(7)?,
            correlation_id: r.get(8)?,
        },
    })
}

/// The run context tagged on a stream's registry row, or empty for ad-hoc / unknown streams.
/// All events in a stream share one context, so reads look it up once and stamp every event.
fn read_context(conn: &Connection, stream: &str) -> Result<EventContext> {
    let Ok(n) = parse_id(stream) else {
        return Ok(EventContext::default());
    };
    let ctx = conn
        .prepare_cached(
            "SELECT account, agent_id, agent_version, correlation_id FROM streams WHERE n = ?1",
        )
        .map_err(map_sql)?
        .query_row([n], |r| {
            Ok(EventContext {
                account: r.get(0)?,
                agent_id: r.get(1)?,
                agent_version: r.get(2)?,
                correlation_id: r.get(3)?,
            })
        })
        .optional()
        .map_err(map_sql)?;
    Ok(ctx.unwrap_or_default())
}

/// Add the optional run-context columns + account index to `streams` (idempotent). The
/// `CREATE TABLE` in [`SqliteEvents::init`] makes the base table; this fills in the additive
/// columns, so a fresh store and a pre-existing one converge on the same schema with no
/// destructive migration.
fn migrate_stream_context(conn: &Connection) -> Result<()> {
    for col in ["account", "agent_id", "agent_version", "correlation_id"] {
        add_column_if_missing(conn, "streams", col)?;
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_streams_account ON streams(account)",
        [],
    )
    .map_err(map_sql)?;
    Ok(())
}

/// `ALTER TABLE <table> ADD COLUMN <col> TEXT`, but only if the column is absent — SQLite has
/// no `ADD COLUMN IF NOT EXISTS`, so we consult `PRAGMA table_info`. `table`/`col` are internal
/// `&'static str` constants (never user input), so the formatted SQL is safe.
fn add_column_if_missing(conn: &Connection, table: &str, col: &str) -> Result<()> {
    let present = {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(map_sql)?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(map_sql)?;
        let mut found = false;
        for name in names {
            if name.map_err(map_sql)? == col {
                found = true;
                break;
            }
        }
        found
    };
    if !present {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} TEXT"), [])
            .map_err(map_sql)?;
    }
    Ok(())
}

fn collect_raw(
    stmt: &mut rusqlite::Statement,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<RawEvent>> {
    let rows = stmt
        .query_map(params, |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(map_sql)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_sql)
}

/// Insert one event row (no registry update — callers handle that). Mints a ULID id when
/// the event has none. `conn` is the active transaction (a `Transaction` derefs here). `ctx` is
/// the stream's run context, stamped onto the returned [`StoredEvent`] (it lives on the registry,
/// not the event row, so it is not persisted here — only surfaced).
fn insert_event(
    conn: &Connection,
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
    let kind_tag = ev.kind.kind_tag();
    let payload = serde_json::to_string(&ev.kind)?;
    conn.prepare_cached(
        "INSERT INTO events (stream, stream_seq, id, kind, schema_version, ts, payload, turn_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .map_err(map_sql)?
    .execute(rusqlite::params![
        stream,
        stream_seq,
        id,
        kind_tag,
        ev.schema_version,
        ts,
        payload,
        ev.turn_id
    ])
    .map_err(map_sql)?;
    let global_seq = conn.last_insert_rowid();
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
fn load_by_id(conn: &Connection, id: &str) -> Result<Option<StoredEvent>> {
    let row = conn
        .query_row(
            "SELECT global_seq, stream, stream_seq, schema_version, ts, payload, turn_id \
             FROM events WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, u32>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(map_sql)?;
    match row {
        Some((global_seq, stream, stream_seq, schema_version, ts, payload, turn_id)) => {
            let kind = serde_json::from_str(&payload)?;
            let context = read_context(conn, &stream)?;
            Ok(Some(StoredEvent {
                global_seq,
                stream,
                stream_seq,
                id: id.to_string(),
                turn_id,
                schema_version,
                ts_ms: ts,
                kind,
                context,
            }))
        }
        None => Ok(None),
    }
}

/// The append-only event store, backed by SQLite (WAL); serialized in-process by a `Mutex`,
/// with `UNIQUE(id)` and `UNIQUE(stream, stream_seq)` as durable backstops.
pub(crate) struct SqliteEvents {
    conn: Mutex<Connection>,
}

impl SqliteEvents {
    /// Open (creating if needed) a store at `path`, with WAL enabled for concurrent reads.
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_sql)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(map_sql)?;
        // C-25: coordinate cross-process writers on the shared `~/.flux/events.db`. WAL permits a
        // single writer at a time; without a busy handler a second process (a `flux app run --serve`
        // daemon + a CLI turn on the same file) gets `SQLITE_BUSY` immediately and the write is lost
        // — `record_message` `?`-propagates and aborts the turn. A ~5s busy_timeout makes a contended
        // writer WAIT for the lock instead of failing; the in-process `Mutex` still serializes one
        // process's own writers, so this only ever matters across processes.
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(map_sql)?;
        // NORMAL is the recommended durability level under WAL: durable against application crashes
        // (only a power loss can drop the last few committed transactions) while avoiding an fsync
        // per commit — so single-process throughput does not regress.
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(map_sql)?;
        Self::init(conn)
    }

    /// An in-memory store (for tests and the SDK's ephemeral sessions).
    pub(crate) fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(map_sql)?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                 global_seq     INTEGER PRIMARY KEY AUTOINCREMENT,
                 stream         TEXT    NOT NULL,
                 stream_seq     INTEGER NOT NULL,
                 id             TEXT    NOT NULL,
                 kind           TEXT    NOT NULL,
                 schema_version INTEGER NOT NULL DEFAULT 1,
                 ts             INTEGER NOT NULL,
                 payload        TEXT    NOT NULL,
                 turn_id        INTEGER,
                 UNIQUE(id),
                 UNIQUE(stream, stream_seq)
             );
             CREATE INDEX IF NOT EXISTS idx_events_stream_kind ON events(stream, kind, stream_seq);
             CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);
             CREATE INDEX IF NOT EXISTS idx_events_turn ON events(stream, turn_id) WHERE turn_id IS NOT NULL;
             CREATE TABLE IF NOT EXISTS streams (
                 n          INTEGER PRIMARY KEY AUTOINCREMENT,
                 model      TEXT    NOT NULL DEFAULT '',
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 last_seq   INTEGER NOT NULL DEFAULT -1,
                 msg_count  INTEGER NOT NULL DEFAULT 0
             );",
        )
        .map_err(map_sql)?;
        migrate_stream_context(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl EventBackend for SqliteEvents {
    fn create_session_with_context(&self, model: &str, ctx: &EventContext) -> Result<String> {
        let ts = now_ms();
        let conn = self.conn.lock().unwrap();
        let tx = begin_write(&conn)?;
        tx.execute(
            "INSERT INTO streams \
             (model, created_at, updated_at, last_seq, msg_count, \
              account, agent_id, agent_version, correlation_id) \
             VALUES (?1, ?2, ?2, 0, 0, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                model,
                ts,
                ctx.account,
                ctx.agent_id,
                ctx.agent_version,
                ctx.correlation_id
            ],
        )
        .map_err(map_sql)?;
        let n = tx.last_insert_rowid();
        let stream = format!("s_{n}");
        let ev = NewEvent::new(EventKind::SessionStarted {
            model: model.to_string(),
        });
        insert_event(&tx, &stream, &ev, 0, ctx)?;
        tx.commit().map_err(map_sql)?;
        Ok(stream)
    }

    fn latest_session(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        // Distinguish "no sessions yet" from a real DB error so `--continue` fails loudly
        // on corruption instead of silently starting fresh.
        let n: Option<i64> =
            match conn.query_row("SELECT n FROM streams ORDER BY n DESC LIMIT 1", [], |r| {
                r.get(0)
            }) {
                Ok(n) => Some(n),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(map_sql(e)),
            };
        Ok(n.map(|n| format!("s_{n}")))
    }

    fn info(&self, stream: &str) -> Result<SessionInfo> {
        let n = parse_id(stream)?;
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT model, created_at, updated_at, account, agent_id, agent_version, correlation_id \
             FROM streams WHERE n = ?1",
            [n],
            |r| {
                Ok(SessionInfo {
                    id: stream.to_string(),
                    model: r.get(0)?,
                    created_at_ms: r.get(1)?,
                    updated_at_ms: r.get(2)?,
                    context: EventContext {
                        account: r.get(3)?,
                        agent_id: r.get(4)?,
                        agent_version: r.get(5)?,
                        correlation_id: r.get(6)?,
                    },
                })
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::Other(format!("session {stream} not found"))
            }
            other => map_sql(other),
        })
    }

    fn list(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {SUMMARY_COLS} FROM streams \
                 ORDER BY updated_at DESC, n DESC LIMIT ?1"
            ))
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([limit as i64], row_to_summary)
            .map_err(map_sql)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sql)
    }

    fn list_for_account(&self, account: &str, limit: usize) -> Result<Vec<SessionSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {SUMMARY_COLS} FROM streams \
                 WHERE account = ?1 ORDER BY updated_at DESC, n DESC LIMIT ?2"
            ))
            .map_err(map_sql)?;
        let rows = stmt
            .query_map(rusqlite::params![account, limit as i64], row_to_summary)
            .map_err(map_sql)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sql)
    }

    fn account_streams(&self, account: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT n FROM streams WHERE account = ?1 ORDER BY updated_at DESC, n DESC")
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([account], |r| Ok(format!("s_{}", r.get::<_, i64>(0)?)))
            .map_err(map_sql)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sql)
    }

    fn find_correlated(&self, correlation_id: &str, agent_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let n: Option<i64> = conn
            .query_row(
                "SELECT n FROM streams WHERE correlation_id = ?1 AND agent_id = ?2 \
                 ORDER BY n DESC LIMIT 1",
                [correlation_id, agent_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_sql)?;
        Ok(n.map(|n| format!("s_{n}")))
    }

    fn find_correlated_in_realm(
        &self,
        correlation_id: &str,
        agent_id: &str,
        realm: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let n: Option<i64> = conn
            .query_row(
                "SELECT n FROM streams WHERE correlation_id = ?1 AND agent_id = ?2 \
                 AND account = ?3 ORDER BY n DESC LIMIT 1",
                [correlation_id, agent_id, realm],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_sql)?;
        Ok(n.map(|n| format!("s_{n}")))
    }

    fn children_of(&self, stream: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT n FROM streams WHERE correlation_id = ?1 ORDER BY n ASC")
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([stream], |r| Ok(format!("s_{}", r.get::<_, i64>(0)?)))
            .map_err(map_sql)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sql)
    }

    fn prune_empty(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let tx = begin_write(&conn)?;
        let empty: Vec<i64> = {
            let mut stmt = tx
                .prepare("SELECT n FROM streams WHERE msg_count = 0")
                .map_err(map_sql)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, i64>(0))
                .map_err(map_sql)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(map_sql)?
        };
        for n in &empty {
            let stream = format!("s_{n}");
            tx.execute("DELETE FROM events WHERE stream = ?1", [&stream])
                .map_err(map_sql)?;
            tx.execute("DELETE FROM streams WHERE n = ?1", [n])
                .map_err(map_sql)?;
        }
        tx.commit().map_err(map_sql)?;
        Ok(empty.len())
    }

    fn prune_inactive(&self, agent_id: &str, cutoff_ms: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let tx = begin_write(&conn)?;
        let expired: Vec<i64> = {
            let mut stmt = tx
                .prepare("SELECT n FROM streams WHERE agent_id = ?1 AND updated_at < ?2")
                .map_err(map_sql)?;
            let rows = stmt
                .query_map(rusqlite::params![agent_id, cutoff_ms], |r| {
                    r.get::<_, i64>(0)
                })
                .map_err(map_sql)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(map_sql)?
        };
        for n in &expired {
            let stream = format!("s_{n}");
            tx.execute("DELETE FROM events WHERE stream = ?1", [&stream])
                .map_err(map_sql)?;
            tx.execute("DELETE FROM streams WHERE n = ?1", [n])
                .map_err(map_sql)?;
        }
        tx.commit().map_err(map_sql)?;
        Ok(expired.len())
    }

    fn prune_older_than(&self, cutoff_ms: i64) -> Result<usize> {
        // The tag-agnostic sibling of `prune_inactive`: same loop-delete shape, WHERE `updated_at`
        // alone (no `agent_id` predicate) — D-75's whole-store retention.
        let conn = self.conn.lock().unwrap();
        let tx = begin_write(&conn)?;
        let expired: Vec<i64> = {
            let mut stmt = tx
                .prepare("SELECT n FROM streams WHERE updated_at < ?1")
                .map_err(map_sql)?;
            let rows = stmt
                .query_map([cutoff_ms], |r| r.get::<_, i64>(0))
                .map_err(map_sql)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(map_sql)?
        };
        for n in &expired {
            let stream = format!("s_{n}");
            tx.execute("DELETE FROM events WHERE stream = ?1", [&stream])
                .map_err(map_sql)?;
            tx.execute("DELETE FROM streams WHERE n = ?1", [n])
                .map_err(map_sql)?;
        }
        tx.commit().map_err(map_sql)?;
        Ok(expired.len())
    }

    fn append(&self, stream: &str, ev: NewEvent) -> Result<StoredEvent> {
        let conn = self.conn.lock().unwrap();
        if let Some(id) = &ev.id {
            if let Some(existing) = load_by_id(&conn, id)? {
                return Ok(existing);
            }
        }
        let tx = begin_write(&conn)?;
        // All events in a stream share its run context; read it once and stamp the stored event.
        let ctx = read_context(&tx, stream)?;
        let next_seq: i64 = tx
            .prepare_cached(
                "SELECT COALESCE(MAX(stream_seq), -1) + 1 FROM events WHERE stream = ?1",
            )
            .map_err(map_sql)?
            .query_row([stream], |r| r.get(0))
            .map_err(map_sql)?;
        let stored = insert_event(&tx, stream, &ev, next_seq, &ctx)?;
        // Maintain the session registry — but only for real `s_<n>` sessions. The log itself accepts
        // any stream string (the interpreter writes run events under ad-hoc ids like `"sess"`), so a
        // non-session stream simply has no registry row to update.
        if let Ok(n) = parse_id(stream) {
            let model_opt = match &ev.kind {
                EventKind::SessionStarted { model } | EventKind::ModelChanged { model } => {
                    Some(model.as_str())
                }
                _ => None,
            };
            tx.prepare_cached(
                "UPDATE streams SET updated_at = ?1, last_seq = ?2, model = COALESCE(?3, model) \
                 WHERE n = ?4",
            )
            .map_err(map_sql)?
            .execute(rusqlite::params![stored.ts_ms, next_seq, model_opt, n])
            .map_err(map_sql)?;
            // Keep msg_count equal to the live conversation length (so `list` matches a replay).
            match &ev.kind {
                EventKind::Message(_) => {
                    tx.prepare_cached("UPDATE streams SET msg_count = msg_count + 1 WHERE n = ?1")
                        .map_err(map_sql)?
                        .execute([n])
                        .map_err(map_sql)?;
                }
                EventKind::Compacted { messages } => {
                    tx.prepare_cached("UPDATE streams SET msg_count = ?1 WHERE n = ?2")
                        .map_err(map_sql)?
                        .execute(rusqlite::params![messages.len() as i64, n])
                        .map_err(map_sql)?;
                }
                _ => {}
            }
        }
        tx.commit().map_err(map_sql)?;
        Ok(stored)
    }

    fn load_stream(&self, stream: &str, after_seq: Option<i64>) -> Result<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();
        let ctx = read_context(&conn, stream)?;
        let after = after_seq.unwrap_or(-1);
        let mut stmt = conn
            .prepare(
                "SELECT global_seq, stream_seq, id, schema_version, ts, payload, turn_id \
                 FROM events WHERE stream = ?1 AND stream_seq > ?2 ORDER BY stream_seq",
            )
            .map_err(map_sql)?;
        let raw = collect_raw(&mut stmt, rusqlite::params![stream, after])?;
        decode_all(stream, &ctx, raw)
    }

    fn load_by_kind(&self, stream: &str, kind: &str) -> Result<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();
        let ctx = read_context(&conn, stream)?;
        let mut stmt = conn
            .prepare(
                "SELECT global_seq, stream_seq, id, schema_version, ts, payload, turn_id \
                 FROM events WHERE stream = ?1 AND kind = ?2 ORDER BY stream_seq",
            )
            .map_err(map_sql)?;
        let raw = collect_raw(&mut stmt, rusqlite::params![stream, kind])?;
        decode_all(stream, &ctx, raw)
    }

    fn conversation_delta(&self, stream: &str, after_seq: i64) -> Result<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();
        let ctx = read_context(&conn, stream)?;
        // `prepare_cached` — this is the one query that runs on EVERY planner round (the point of
        // the incremental cache is that it usually returns nothing), so don't re-compile it each time.
        let mut stmt = conn
            .prepare_cached(
                "SELECT global_seq, stream_seq, id, schema_version, ts, payload, turn_id \
                 FROM events WHERE stream = ?1 AND stream_seq > ?2 AND kind IN ('message', 'compacted') \
                 ORDER BY stream_seq",
            )
            .map_err(map_sql)?;
        let raw = collect_raw(&mut stmt, rusqlite::params![stream, after_seq])?;
        decode_all(stream, &ctx, raw)
    }

    fn load_turn(&self, stream: &str, turn_id: i64) -> Result<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();
        let ctx = read_context(&conn, stream)?;
        let mut stmt = conn
            .prepare(
                "SELECT global_seq, stream_seq, id, schema_version, ts, payload, turn_id \
                 FROM events WHERE stream = ?1 AND (global_seq = ?2 OR turn_id = ?2) \
                 ORDER BY stream_seq",
            )
            .map_err(map_sql)?;
        let raw = collect_raw(&mut stmt, rusqlite::params![stream, turn_id])?;
        decode_all(stream, &ctx, raw)
    }

    fn head_seq(&self, stream: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(MAX(stream_seq), -1) FROM events WHERE stream = ?1",
            [stream],
            |r| r.get(0),
        )
        .map_err(map_sql)
    }

    fn all_streams(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT n FROM streams ORDER BY n")
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([], |r| Ok(format!("s_{}", r.get::<_, i64>(0)?)))
            .map_err(map_sql)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sql)
    }

    fn streams_with_correlation(&self) -> Result<Vec<(i64, Option<String>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT n, correlation_id FROM streams ORDER BY n")
            .map_err(map_sql)?;
        let rows: Vec<(i64, Option<String>)> = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(map_sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sql)?;
        Ok(rows)
    }
}
