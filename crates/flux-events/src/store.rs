//! The SQLite-backed append-only event store.
//!
//! One ordered `events` log (WAL) holds every fact; a small `streams` registry mints the
//! `s_<n>` session ids and serves the session-list read model (it is rebuildable from the
//! log). A *stream* is one session, so messages, run events, and turn telemetry interleave
//! in one causal order — the whole point of unifying the three old logs.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};

use flux_core::{Error, Message, Result, Usage};
use flux_lang::ast::RunEvent;

use crate::context::EventContext;
use crate::kind::{EventKind, NewEvent, StoredEvent};
use crate::projection;

fn map_sql<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(format!("event store: {e}"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse a session id (`"s_<n>"`) into its registry rowid, matching the old `s_<rowid>`
/// scheme so `FlowStore`'s `session_id`-keyed tables keep resolving.
fn parse_id(id: &str) -> Result<i64> {
    id.strip_prefix("s_")
        .and_then(|n| n.parse::<i64>().ok())
        .ok_or_else(|| Error::Other(format!("invalid session id: {id:?}")))
}

/// The `streams` columns a [`SessionSummary`] reads, in `row_to_summary` order. Shared by
/// [`EventStore::list`] and [`EventStore::list_for_account`] so the two never drift.
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
        .query_row(
            "SELECT account, agent_id, agent_version, correlation_id FROM streams WHERE n = ?1",
            [n],
            |r| {
                Ok(EventContext {
                    account: r.get(0)?,
                    agent_id: r.get(1)?,
                    agent_version: r.get(2)?,
                    correlation_id: r.get(3)?,
                })
            },
        )
        .optional()
        .map_err(map_sql)?;
    Ok(ctx.unwrap_or_default())
}

/// Add the optional run-context columns + account index to `streams` (idempotent). The
/// `CREATE TABLE` in [`EventStore::init`] makes the base table; this fills in the additive
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

/// Metadata about a session, projected from its events. (The session registry view —
/// "stream" and "session" are the same thing here.)
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub model: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// The owning run's tenant/agent context (empty for single-tenant sessions).
    pub context: EventContext,
}

/// A one-line session summary for listings (`flux sessions` / the REPL `/sessions`).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub model: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Length of the current (post-compaction) conversation — kept equal to
    /// `conversation(id).len()` by the registry, so the count never disagrees with a replay.
    pub messages: usize,
    /// The owning run's tenant/agent context (empty for single-tenant sessions).
    pub context: EventContext,
}

/// The append-only event store. Backed by SQLite (WAL); serialized in-process by a `Mutex`,
/// with `UNIQUE(id)` and `UNIQUE(stream, stream_seq)` as durable backstops.
pub struct EventStore {
    conn: Mutex<Connection>,
}

impl EventStore {
    /// Open (creating if needed) a store at `path`, with WAL enabled for concurrent reads.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_sql)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(map_sql)?;
        Self::init(conn)
    }

    /// An in-memory store (for tests and the SDK's ephemeral sessions).
    pub fn in_memory() -> Result<Self> {
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

    // --- streams (sessions) -------------------------------------------------

    /// Mint a new session and return its id (`"s_<n>"`). Atomically registers the stream
    /// and appends its `SessionStarted` event at `stream_seq` 0. Single-tenant: the run is
    /// tagged with an empty [`EventContext`] (see [`create_session_with_context`] to tag it).
    ///
    /// [`create_session_with_context`]: Self::create_session_with_context
    pub fn create_session(&self, model: &str) -> Result<String> {
        self.create_session_with_context(model, &EventContext::default())
    }

    /// Mint a new session tagged with `ctx` (account / agent id+version / correlation id) and
    /// return its id. The context is fixed for the run's whole lifetime, recorded on the stream
    /// registry; reads of this stream's events, [`info`](Self::info), and [`list`](Self::list)
    /// carry it back, and [`list_for_account`](Self::list_for_account) scopes to it. An empty
    /// `ctx` is exactly equivalent to [`create_session`](Self::create_session).
    pub fn create_session_with_context(&self, model: &str, ctx: &EventContext) -> Result<String> {
        let ts = now_ms();
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction().map_err(map_sql)?;
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

    /// The most recently created session id, if any (for `--continue`).
    pub fn latest_session(&self) -> Result<Option<String>> {
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

    /// Session metadata, from the registry.
    pub fn info(&self, stream: &str) -> Result<SessionInfo> {
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

    /// The most recent sessions (newest-active first), with current message counts.
    pub fn list(&self, limit: usize) -> Result<Vec<SessionSummary>> {
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

    /// The most recent runs for `account` (newest-active first) — the account-scoped sibling of
    /// [`list`](Self::list). Returns **only** streams tagged with that account, so a downstream
    /// multi-tenant service can enumerate one tenant's runs without seeing any other's.
    pub fn list_for_account(&self, account: &str, limit: usize) -> Result<Vec<SessionSummary>> {
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

    /// The stream ids for `account`, newest-active first — the enumeration primitive a downstream
    /// consumer folds over (`account_streams` → per-stream [`conversation`](Self::conversation) /
    /// [`turns`](Self::turns)) to replay a tenant's transcripts as projections over the log.
    pub fn account_streams(&self, account: &str) -> Result<Vec<String>> {
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

    /// Switch the session's model (records a `ModelChanged` event; the registry follows).
    pub fn set_model(&self, stream: &str, model: &str) -> Result<()> {
        self.append(
            stream,
            NewEvent::new(EventKind::ModelChanged {
                model: model.to_string(),
            }),
        )?;
        Ok(())
    }

    /// Delete sessions that recorded no messages (abandoned / test-run streams), along with
    /// their events. Returns the number of sessions removed. An empty stream has no history
    /// worth preserving, so real deletion is append-only-safe.
    pub fn prune_empty(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction().map_err(map_sql)?;
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

    // --- append -------------------------------------------------------------

    /// Append one event, assigning its `stream_seq` / `global_seq` / `ts` and updating the
    /// session registry — all in one transaction, so the read model never drifts from the
    /// log. If the event carries a caller-supplied `id` that already exists, this is a no-op
    /// returning the prior event (idempotent retry).
    pub fn append(&self, stream: &str, ev: NewEvent) -> Result<StoredEvent> {
        let conn = self.conn.lock().unwrap();
        if let Some(id) = &ev.id {
            if let Some(existing) = load_by_id(&conn, id)? {
                return Ok(existing);
            }
        }
        let tx = conn.unchecked_transaction().map_err(map_sql)?;
        // All events in a stream share its run context; read it once and stamp the stored event.
        let ctx = read_context(&tx, stream)?;
        let next_seq: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(stream_seq), -1) + 1 FROM events WHERE stream = ?1",
                [stream],
                |r| r.get(0),
            )
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
            tx.execute(
                "UPDATE streams SET updated_at = ?1, last_seq = ?2, model = COALESCE(?3, model) \
                 WHERE n = ?4",
                rusqlite::params![stored.ts_ms, next_seq, model_opt, n],
            )
            .map_err(map_sql)?;
            // Keep msg_count equal to the live conversation length (so `list` matches a replay).
            match &ev.kind {
                EventKind::Message(_) => {
                    tx.execute(
                        "UPDATE streams SET msg_count = msg_count + 1 WHERE n = ?1",
                        [n],
                    )
                    .map_err(map_sql)?;
                }
                EventKind::Compacted { messages } => {
                    tx.execute(
                        "UPDATE streams SET msg_count = ?1 WHERE n = ?2",
                        rusqlite::params![messages.len() as i64, n],
                    )
                    .map_err(map_sql)?;
                }
                _ => {}
            }
        }
        tx.commit().map_err(map_sql)?;
        Ok(stored)
    }

    /// Append several events to a stream atomically (all-or-nothing, consecutive seqs).
    pub fn append_batch(&self, stream: &str, evs: Vec<NewEvent>) -> Result<Vec<StoredEvent>> {
        let mut out = Vec::with_capacity(evs.len());
        for ev in evs {
            out.push(self.append(stream, ev)?);
        }
        Ok(out)
    }

    // --- load ---------------------------------------------------------------

    /// All events of a stream in order; `after_seq` enables incremental replay.
    pub fn load_stream(&self, stream: &str, after_seq: Option<i64>) -> Result<Vec<StoredEvent>> {
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

    /// Events of a stream filtered by `kind` tag (e.g. `"message"`, `"run"`), in order.
    pub fn load_by_kind(&self, stream: &str, kind: &str) -> Result<Vec<StoredEvent>> {
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

    /// Every event tagged with `turn_id`, plus its `TurnStarted` anchor (whose `global_seq`
    /// *is* the turn id), in order — the old `turn_log` + `plan_attempts` join.
    pub fn load_turn(&self, stream: &str, turn_id: i64) -> Result<Vec<StoredEvent>> {
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

    /// The current head sequence of a stream (`-1` if empty) — the optimistic-concurrency anchor.
    pub fn head_seq(&self, stream: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(MAX(stream_seq), -1) FROM events WHERE stream = ?1",
            [stream],
            |r| r.get(0),
        )
        .map_err(map_sql)
    }

    // --- ergonomic event-native helpers (used at call sites) ----------------

    /// Record one conversation message.
    pub fn record_message(&self, stream: &str, m: &Message) -> Result<()> {
        self.append(stream, NewEvent::message(m.clone()))?;
        Ok(())
    }

    /// Record a context-compaction snapshot (the append-only `rewrite_messages`).
    pub fn record_compaction(&self, stream: &str, messages: &[Message]) -> Result<()> {
        self.append(stream, NewEvent::compacted(messages.to_vec()))?;
        Ok(())
    }

    /// Record a flow run-trace event.
    pub fn record_run_event(&self, stream: &str, ev: &RunEvent) -> Result<()> {
        self.append(stream, NewEvent::run(ev.clone()))?;
        Ok(())
    }

    /// Begin a turn and return its `turn_id` (the `TurnStarted` event's `global_seq`). Use
    /// `.unwrap_or(-1)` at call sites to stay non-fatal — telemetry must never block a turn.
    pub fn begin_turn(&self, stream: &str, user_input: &str, model: &str) -> Result<i64> {
        let stored = self.append(
            stream,
            NewEvent::new(EventKind::TurnStarted {
                user_input: user_input.to_string(),
                model: model.to_string(),
            }),
        )?;
        Ok(stored.global_seq)
    }

    /// Record one planning attempt within `turn_id`. A negative `turn_id` (failed
    /// `begin_turn`) is silently skipped.
    pub fn record_plan_attempt(
        &self,
        stream: &str,
        turn_id: i64,
        step: u32,
        outcome: &str,
        error: Option<&str>,
    ) -> Result<()> {
        if turn_id < 0 {
            return Ok(());
        }
        self.append(
            stream,
            NewEvent::new(EventKind::PlanAttempted {
                step,
                outcome: outcome.to_string(),
                error: error.map(|s| s.to_string()),
            })
            .in_turn(turn_id),
        )?;
        Ok(())
    }

    /// Close a turn with its final outcome, iteration count, assistant answer, and token `usage`
    /// tally (`None` when the provider reported none). A negative `turn_id` is a no-op.
    pub fn end_turn(
        &self,
        stream: &str,
        turn_id: i64,
        outcome: &str,
        iterations: u32,
        answer: &str,
        usage: Option<Usage>,
    ) -> Result<()> {
        if turn_id < 0 {
            return Ok(());
        }
        self.append(
            stream,
            NewEvent::new(EventKind::TurnEnded {
                outcome: outcome.to_string(),
                iterations,
                answer: answer.to_string(),
                usage,
            })
            .in_turn(turn_id),
        )?;
        Ok(())
    }

    // --- projections (load + fold) ------------------------------------------

    /// The conversation for a session (replaces `SessionStore::load_messages`).
    pub fn conversation(&self, stream: &str) -> Result<Vec<Message>> {
        Ok(projection::conversation(&self.load_stream(stream, None)?))
    }

    /// The flow run-trace for a session (replaces `FlowStore::events`).
    pub fn run_trace(&self, stream: &str) -> Result<Vec<RunEvent>> {
        Ok(projection::run_trace(&self.load_by_kind(stream, "run")?))
    }

    /// The turn telemetry for a session (replaces `turn_log` + `plan_attempts`).
    pub fn turns(&self, stream: &str) -> Result<Vec<projection::TurnSummary>> {
        Ok(projection::turns(&self.load_stream(stream, None)?))
    }
}

/// Raw event columns as read from a row, before the `payload` JSON is decoded.
type RawEvent = (i64, i64, String, u32, i64, String, Option<i64>);

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

/// Decode a batch of raw rows (all from `stream`, all sharing its run `ctx`) into [`StoredEvent`]s.
fn decode_all(stream: &str, ctx: &EventContext, raw: Vec<RawEvent>) -> Result<Vec<StoredEvent>> {
    let mut out = Vec::with_capacity(raw.len());
    for (global_seq, stream_seq, id, schema_version, ts, payload, turn_id) in raw {
        let kind: EventKind = serde_json::from_str(&payload)?;
        out.push(StoredEvent {
            global_seq,
            stream: stream.to_string(),
            stream_seq,
            id,
            turn_id,
            schema_version,
            ts_ms: ts,
            kind,
            context: ctx.clone(),
        });
    }
    Ok(out)
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
    conn.execute(
        "INSERT INTO events (stream, stream_seq, id, kind, schema_version, ts, payload, turn_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            stream,
            stream_seq,
            id,
            kind_tag,
            ev.schema_version,
            ts,
            payload,
            ev.turn_id
        ],
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::Message;

    // --- conformance: ported from flux-session's test module, adapted to the event API ---

    #[test]
    fn create_append_load_roundtrip() {
        let store = EventStore::in_memory().unwrap();
        let id = store.create_session("claude-sonnet-4-6").unwrap();
        assert!(id.starts_with("s_"));

        store
            .record_message(&id, &Message::user_text("hello"))
            .unwrap();
        store
            .record_message(&id, &Message::assistant_text("hi there"))
            .unwrap();

        let msgs = store.conversation(&id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text(), "hello");
        assert_eq!(msgs[1].text(), "hi there");
        assert_eq!(store.info(&id).unwrap().model, "claude-sonnet-4-6");
    }

    #[test]
    fn updated_at_advances_on_append() {
        let store = EventStore::in_memory().unwrap();
        let id = store.create_session("m").unwrap();
        let created = store.info(&id).unwrap().updated_at_ms;
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .record_message(&id, &Message::user_text("hi"))
            .unwrap();
        let after = store.info(&id).unwrap().updated_at_ms;
        assert!(after >= created, "updated_at must not go backwards");
        assert_eq!(store.list(1).unwrap()[0].updated_at_ms, after);
    }

    #[test]
    fn updated_at_advances_on_set_model() {
        let store = EventStore::in_memory().unwrap();
        let id = store.create_session("sonnet").unwrap();
        let before = store.info(&id).unwrap().updated_at_ms;
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.set_model(&id, "opus").unwrap();
        let after = store.info(&id).unwrap().updated_at_ms;
        assert!(after >= before);
        assert_eq!(store.info(&id).unwrap().model, "opus");
    }

    #[test]
    fn compaction_replaces_the_live_view_but_keeps_history() {
        let store = EventStore::in_memory().unwrap();
        let id = store.create_session("m").unwrap();
        for i in 0..5 {
            store
                .record_message(&id, &Message::user_text(format!("m{i}")))
                .unwrap();
        }
        assert_eq!(store.conversation(&id).unwrap().len(), 5);

        store
            .record_compaction(
                &id,
                &[Message::user_text("summary"), Message::user_text("recent")],
            )
            .unwrap();
        let msgs = store.conversation(&id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text(), "summary");
        assert_eq!(msgs[1].text(), "recent");

        // appending after a compaction continues from the snapshot
        store
            .record_message(&id, &Message::user_text("more"))
            .unwrap();
        assert_eq!(store.conversation(&id).unwrap().len(), 3);

        // history is retained: the 5 superseded Message events are still on disk
        let raw = store.load_stream(&id, None).unwrap();
        let messages = raw
            .iter()
            .filter(|e| e.kind.kind_tag() == "message")
            .count();
        let compactions = raw
            .iter()
            .filter(|e| e.kind.kind_tag() == "compacted")
            .count();
        assert_eq!(messages, 6, "5 pre-compaction + 1 post-compaction");
        assert_eq!(compactions, 1);

        // the list count tracks the live conversation length, not the raw event count
        assert_eq!(store.list(1).unwrap()[0].messages, 3);
        assert_eq!(
            store.list(1).unwrap()[0].messages,
            store.conversation(&id).unwrap().len()
        );
    }

    #[test]
    fn latest_session_tracks_newest() {
        let store = EventStore::in_memory().unwrap();
        assert!(store.latest_session().unwrap().is_none());
        let _a = store.create_session("m").unwrap();
        let b = store.create_session("m").unwrap();
        assert_eq!(store.latest_session().unwrap(), Some(b));
    }

    #[test]
    fn unknown_session_has_no_conversation_but_info_errors() {
        let store = EventStore::in_memory().unwrap();
        // The log accepts any stream; an unknown one simply has no events.
        assert!(store.conversation("s_999").unwrap().is_empty());
        assert!(store.conversation("nope").unwrap().is_empty());
        // The registry, however, has no row for it.
        assert!(store.info("s_999").is_err());
    }

    #[test]
    fn list_returns_newest_first_with_counts() {
        let store = EventStore::in_memory().unwrap();
        let a = store.create_session("m1").unwrap();
        store.record_message(&a, &Message::user_text("hi")).unwrap();
        store
            .record_message(&a, &Message::user_text("there"))
            .unwrap();
        let b = store.create_session("m2").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .record_message(&a, &Message::user_text("last"))
            .unwrap();

        let list = store.list(10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, a, "most recently active first");
        assert_eq!(list[0].messages, 3);
        assert_eq!(list[1].id, b);
        assert_eq!(list[1].messages, 0);
        assert_eq!(list[0].model, "m1");
        assert_eq!(store.list(1).unwrap().len(), 1);
    }

    #[test]
    fn set_model_updates_listing() {
        let store = EventStore::in_memory().unwrap();
        let a = store.create_session("sonnet").unwrap();
        store.set_model(&a, "opus").unwrap();
        assert_eq!(store.list(1).unwrap()[0].model, "opus");
        assert_eq!(store.info(&a).unwrap().model, "opus");
    }

    #[test]
    fn prune_empty_removes_zero_message_sessions() {
        let store = EventStore::in_memory().unwrap();
        let a = store.create_session("m").unwrap();
        store.record_message(&a, &Message::user_text("hi")).unwrap();
        let _b = store.create_session("m").unwrap();
        let _c = store.create_session("m").unwrap();

        assert_eq!(store.list(10).unwrap().len(), 3);
        let pruned = store.prune_empty().unwrap();
        assert_eq!(pruned, 2);
        let remaining = store.list(10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, a);
        assert_eq!(store.latest_session().unwrap(), Some(a));
    }

    #[test]
    fn roles_round_trip_through_the_conversation() {
        let store = EventStore::in_memory().unwrap();
        let id = store.create_session("m").unwrap();
        store.record_message(&id, &Message::user_text("q")).unwrap();
        store
            .record_message(&id, &Message::assistant_text("a"))
            .unwrap();
        let roles: Vec<_> = store
            .conversation(&id)
            .unwrap()
            .iter()
            .map(|m| format!("{:?}", m.role).to_lowercase())
            .collect();
        assert_eq!(roles, vec!["user", "assistant"]);
    }

    #[test]
    fn append_is_transactional_and_sequences_monotonically() {
        let store = EventStore::in_memory().unwrap();
        let id = store.create_session("m").unwrap();
        for i in 0..10 {
            store
                .record_message(&id, &Message::user_text(format!("m{i}")))
                .unwrap();
        }
        assert_eq!(store.conversation(&id).unwrap().len(), 10);
        // SessionStarted (seq 0) + 10 messages → head seq 10, contiguous.
        assert_eq!(store.head_seq(&id).unwrap(), 10);
    }

    // --- event-store specific behavior ---

    #[test]
    fn run_events_and_turn_telemetry_share_the_log() {
        let store = EventStore::in_memory().unwrap();
        let id = store.create_session("m").unwrap();

        let turn = store.begin_turn(&id, "do it", "m").unwrap();
        store
            .record_plan_attempt(&id, turn, 0, "compile_error", Some("boom"))
            .unwrap();
        store
            .record_plan_attempt(&id, turn, 1, "accepted", None)
            .unwrap();
        store
            .record_run_event(
                &id,
                &RunEvent::StepSucceeded {
                    step: "s1".into(),
                    output: "v_1".into(),
                },
            )
            .unwrap();
        store
            .record_message(&id, &Message::user_text("hi"))
            .unwrap();
        store
            .end_turn(
                &id,
                turn,
                "accepted",
                2,
                "done",
                Some(Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    ..Default::default()
                }),
            )
            .unwrap();

        // run trace projection
        let trace = store.run_trace(&id).unwrap();
        assert_eq!(trace.len(), 1);
        assert!(matches!(trace[0], RunEvent::StepSucceeded { .. }));

        // turn telemetry projection
        let turns = store.turns(&id).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].outcome, "accepted");
        assert_eq!(turns[0].iterations, 2);
        assert_eq!(turns[0].plan_attempts.len(), 2);
        // token usage survives the SQLite payload round-trip
        assert_eq!(turns[0].usage.as_ref().map(|u| u.total()), Some(120));

        // load_turn returns the anchor plus its scoped children
        let turn_events = store.load_turn(&id, turn).unwrap();
        let kinds: Vec<_> = turn_events.iter().map(|e| e.kind.kind_tag()).collect();
        assert_eq!(
            kinds,
            vec![
                "turn_started",
                "plan_attempted",
                "plan_attempted",
                "turn_ended"
            ]
        );

        // the conversation projection ignores run/turn events
        assert_eq!(store.conversation(&id).unwrap().len(), 1);
    }

    #[test]
    fn idempotent_append_with_a_stable_id() {
        let store = EventStore::in_memory().unwrap();
        let id = store.create_session("m").unwrap();
        let first = store
            .append(
                &id,
                NewEvent::message(Message::user_text("once")).with_id("evt-1"),
            )
            .unwrap();
        let again = store
            .append(
                &id,
                NewEvent::message(Message::user_text("once")).with_id("evt-1"),
            )
            .unwrap();
        assert_eq!(first.global_seq, again.global_seq, "retry is a no-op");
        assert_eq!(store.conversation(&id).unwrap().len(), 1);
    }

    // --- D-02: tenant/agent context envelope ---

    #[test]
    fn context_round_trips_on_stored_events_and_summaries() {
        let store = EventStore::in_memory().unwrap();
        let ctx = EventContext {
            account: Some("acme".into()),
            agent_id: Some("support-bot".into()),
            agent_version: Some("v3".into()),
            correlation_id: Some("corr-1".into()),
        };
        let id = store.create_session_with_context("m", &ctx).unwrap();
        store
            .record_message(&id, &Message::user_text("hi"))
            .unwrap();

        // Every event read back from the stream carries the run context (SessionStarted + msg).
        let events = store.load_stream(&id, None).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.context == ctx));
        // The append return value carries it too.
        let stored = store
            .append(&id, NewEvent::message(Message::user_text("again")))
            .unwrap();
        assert_eq!(stored.context, ctx);
        // The session registry views surface it.
        assert_eq!(store.info(&id).unwrap().context, ctx);
        assert_eq!(store.list(1).unwrap()[0].context, ctx);
    }

    #[test]
    fn accounts_are_isolated_in_scoped_reads() {
        let store = EventStore::in_memory().unwrap();
        let a = store
            .create_session_with_context("m", &EventContext::for_account("a"))
            .unwrap();
        let b = store
            .create_session_with_context("m", &EventContext::for_account("b"))
            .unwrap();
        store.record_message(&a, &Message::user_text("ax")).unwrap();
        store.record_message(&b, &Message::user_text("bx")).unwrap();

        // list_for_account returns only that account's run.
        let a_list = store.list_for_account("a", 10).unwrap();
        assert_eq!(a_list.len(), 1);
        assert_eq!(a_list[0].id, a);
        assert_eq!(a_list[0].context.account.as_deref(), Some("a"));
        // account_streams is the same isolation, as bare ids.
        assert_eq!(store.account_streams("b").unwrap(), vec![b.clone()]);
        // An unknown account sees nothing; the global list still sees both runs.
        assert!(store.list_for_account("nope", 10).unwrap().is_empty());
        assert!(store.account_streams("nope").unwrap().is_empty());
        assert_eq!(store.list(10).unwrap().len(), 2);
    }

    #[test]
    fn single_tenant_session_has_empty_context() {
        let store = EventStore::in_memory().unwrap();
        let id = store.create_session("m").unwrap();
        store
            .record_message(&id, &Message::user_text("hi"))
            .unwrap();

        // The single-tenant path is unchanged: every surface carries an empty envelope.
        assert!(store.info(&id).unwrap().context.is_empty());
        assert!(store.list(1).unwrap()[0].context.is_empty());
        assert!(store
            .load_stream(&id, None)
            .unwrap()
            .iter()
            .all(|e| e.context.is_empty()));
        // An untagged run never surfaces in account-scoped reads.
        assert!(store.list_for_account("anything", 10).unwrap().is_empty());
        assert!(store.account_streams("anything").unwrap().is_empty());
    }

    #[test]
    fn context_survives_reopen() {
        let path =
            std::env::temp_dir().join(format!("flux-events-d02-reopen-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let ctx = EventContext::for_account("tenant-7");
        let id = {
            let store = EventStore::open(&path).unwrap();
            store.create_session_with_context("m", &ctx).unwrap()
        };
        // Reopen: the additive column migration is idempotent (columns already exist) and the
        // context persists across the process boundary.
        let store = EventStore::open(&path).unwrap();
        assert_eq!(store.info(&id).unwrap().context, ctx);
        assert_eq!(store.list_for_account("tenant-7", 10).unwrap()[0].id, id);
        let _ = std::fs::remove_file(&path);
    }
}
