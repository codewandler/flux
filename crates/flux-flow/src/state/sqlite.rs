//! The native [`FlowStateBackend`]: SQLite/WAL, and the default everywhere flux runs as a process.
//!
//! This is the implementation that used to *be* `state.rs` — every statement here is the one that
//! ran before C-270 put a port in front of it, so native behaviour is unchanged. It is also the only
//! file in `flux-flow` that names `rusqlite`, which is the point: the crate's portable core reaches
//! storage exclusively through [`FlowStateBackend`].
//!
//! `rusqlite::Error::QueryReturnedNoRows` is consumed **here**, at the five reads that can miss, and
//! converted to [`Lookup::NoSuchRow`]. It never crosses the port.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use flux_core::{Error, Result};

use crate::ast::{SymbolName, ValueId, Visibility};
use crate::state::port::{FlowStateBackend, Lookup, StoredSymbol, Suspension, SymbolBinding};

/// Every SQLite failure reaches the caller as one opaque store error. The driver's error *type* has
/// never crossed this boundary; C-270 removed the remaining structural leak (the not-found variant),
/// which is handled as a [`Lookup`] instead.
fn map_sql<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(format!("flow store: {e}"))
}

/// A [`ValueId`]'s SQLite rowid. The `v_<rowid>` id format is this backend's own — a portable
/// backend is free to mint ids any way it likes — so decoding it belongs here. A malformed id is a
/// caller error, not a missing row, and stays an error rather than becoming
/// [`Lookup::NoSuchRow`].
fn value_rowid(id: &ValueId) -> Result<i64> {
    id.0.strip_prefix("v_")
        .and_then(|n| n.parse::<i64>().ok())
        .ok_or_else(|| Error::Other(format!("invalid value id: {:?}", id.0)))
}

/// flux-flow's SQLite store for values, symbols, the suspension latch, and session composites.
pub struct SqliteState {
    conn: Mutex<Connection>,
}

impl SqliteState {
    /// Open (creating if needed) a store at `path`, with WAL enabled.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        // flux-allow-direct-io: SqliteState is the owner of this host-selected SQLite state backend;
        // operation implementations receive the store, never a model-supplied database path.
        let conn = Connection::open(path).map_err(map_sql)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(map_sql)?;
        Self::init(conn)
    }

    /// An in-memory SQLite store (for tests and ephemeral sessions).
    pub fn in_memory() -> Result<Self> {
        // flux-allow-direct-io: in-memory SQLite backend owns no filesystem or external resource.
        Self::init(Connection::open_in_memory().map_err(map_sql)?)
    }

    /// Create the schema if it is absent, then run the one column migration. Unchanged from the
    /// pre-port `FlowStore::init`: cold-boot behaviour here is load-bearing (C-230 was a real bug in
    /// exactly this area for the event log), so the statements and their ordering are preserved
    /// verbatim rather than tidied.
    fn init(conn: Connection) -> Result<Self> {
        // `values` is a SQL keyword, so the value store table is `values_store`.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS values_store (
                 n          INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id TEXT NOT NULL,
                 data       TEXT NOT NULL,
                 bytes      INTEGER NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS symbols (
                 session_id TEXT NOT NULL,
                 name       TEXT NOT NULL,
                 value_id   TEXT NOT NULL,
                 ty         TEXT,
                 summary    TEXT NOT NULL,
                 visibility TEXT NOT NULL,
                 updated_at INTEGER NOT NULL,
                 PRIMARY KEY (session_id, name)
             );
             CREATE TABLE IF NOT EXISTS suspensions (
                 session_id TEXT PRIMARY KEY,
                 flow_name  TEXT,
                 body       TEXT NOT NULL,
                 node       INTEGER NOT NULL,
                 source     TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS session_composites (
                 session_id TEXT NOT NULL,
                 name       TEXT NOT NULL,
                 source     TEXT NOT NULL,
                 updated_at INTEGER NOT NULL,
                 PRIMARY KEY (session_id, name)
             );",
        )
        .map_err(map_sql)?;
        // Migration (L-21): pre-existing stores created the `suspensions` table without the
        // `flow_name` column (a named flow's resume then derived its checkpoint key hash-only).
        // `ALTER TABLE ADD COLUMN` errors when the column already exists — that error is the
        // "already migrated" signal, so it is deliberately ignored.
        let _ = conn.execute("ALTER TABLE suspensions ADD COLUMN flow_name TEXT", []);
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// The suspension row projection shared by the peek and the take, so the two cannot drift.
    fn read_suspension(conn: &Connection, session_id: &str) -> Result<Lookup<Suspension>> {
        let row = conn.query_row(
            "SELECT flow_name, body, node, source FROM suspensions WHERE session_id = ?1",
            [session_id],
            |r| {
                Ok(Suspension {
                    flow_name: r.get::<_, Option<String>>(0)?,
                    body: r.get::<_, String>(1)?,
                    node: r.get::<_, i64>(2)?,
                    source: r.get::<_, String>(3)?,
                })
            },
        );
        match row {
            Ok(suspension) => Ok(Lookup::Found(suspension)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Lookup::NoSuchRow),
            Err(error) => Err(map_sql(error)),
        }
    }
}

impl FlowStateBackend for SqliteState {
    fn put_value(
        &self,
        session_id: &str,
        data: &str,
        bytes: i64,
        created_at_ms: i64,
    ) -> Result<ValueId> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO values_store (session_id, data, bytes, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, data, bytes, created_at_ms],
        )
        .map_err(map_sql)?;
        Ok(ValueId(format!("v_{}", conn.last_insert_rowid())))
    }

    fn get_value(&self, id: &ValueId) -> Result<Lookup<String>> {
        let n = value_rowid(id)?;
        let conn = self.conn.lock().unwrap();
        match conn.query_row("SELECT data FROM values_store WHERE n = ?1", [n], |r| {
            r.get::<_, String>(0)
        }) {
            Ok(data) => Ok(Lookup::Found(data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Lookup::NoSuchRow),
            Err(e) => Err(map_sql(e)),
        }
    }

    fn total_value_bytes(&self, session_id: &str) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let sum: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(bytes), 0) FROM values_store WHERE session_id = ?1",
                [session_id],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        Ok(sum.max(0) as u64)
    }

    fn bind(
        &self,
        session_id: &str,
        name: &str,
        binding: SymbolBinding<'_>,
        updated_at_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO symbols (session_id, name, value_id, ty, summary, visibility, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(session_id, name) DO UPDATE SET
                 value_id   = excluded.value_id,
                 ty         = excluded.ty,
                 summary    = excluded.summary,
                 visibility = excluded.visibility,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                session_id,
                name,
                binding.value_id,
                binding.ty,
                binding.summary,
                binding.visibility.as_str(),
                updated_at_ms
            ],
        )
        .map_err(map_sql)?;
        Ok(())
    }

    fn resolve(&self, session_id: &str, name: &str) -> Result<Lookup<String>> {
        let conn = self.conn.lock().unwrap();
        match conn.query_row(
            "SELECT value_id FROM symbols WHERE session_id = ?1 AND name = ?2",
            rusqlite::params![session_id, name],
            |r| r.get::<_, String>(0),
        ) {
            Ok(v) => Ok(Lookup::Found(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Lookup::NoSuchRow),
            Err(e) => Err(map_sql(e)),
        }
    }

    fn symbols(&self, session_id: &str) -> Result<Vec<StoredSymbol>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT name, ty, summary, visibility FROM symbols
                 WHERE session_id = ?1 ORDER BY updated_at DESC, name ASC",
            )
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([session_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(map_sql)?;
        let mut out = Vec::new();
        for row in rows {
            let (name, ty, summary, vis) = row.map_err(map_sql)?;
            out.push(StoredSymbol {
                name: SymbolName(name),
                ty,
                summary,
                // An undecodable tag is conservatively Hidden, so an unreadable row never leaks
                // into the model-facing view.
                visibility: Visibility::from_tag(&vis).unwrap_or(Visibility::Hidden),
            });
        }
        Ok(out)
    }

    fn save_suspension(
        &self,
        session_id: &str,
        suspension: &Suspension,
        created_at_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO suspensions (session_id, flow_name, body, node, source, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                session_id,
                suspension.flow_name,
                suspension.body,
                suspension.node,
                suspension.source,
                created_at_ms
            ],
        )
        .map_err(map_sql)?;
        Ok(())
    }

    fn load_suspension(&self, session_id: &str) -> Result<Lookup<Suspension>> {
        let conn = self.conn.lock().unwrap();
        Self::read_suspension(&conn, session_id)
    }

    fn take_suspension(&self, session_id: &str) -> Result<Lookup<Suspension>> {
        // One-shot: the read and the delete happen under the one connection lock, so no concurrent
        // caller can observe the same latch twice.
        let conn = self.conn.lock().unwrap();
        let found = Self::read_suspension(&conn, session_id)?;
        if found.is_found() {
            conn.execute(
                "DELETE FROM suspensions WHERE session_id = ?1",
                [session_id],
            )
            .map_err(map_sql)?;
        }
        Ok(found)
    }

    fn has_suspension(&self, session_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        match conn.query_row(
            "SELECT 1 FROM suspensions WHERE session_id = ?1 LIMIT 1",
            rusqlite::params![session_id],
            |_| Ok(()),
        ) {
            Ok(()) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(map_sql(e)),
        }
    }

    fn clear_suspension(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM suspensions WHERE session_id = ?1",
            [session_id],
        )
        .map_err(map_sql)?;
        Ok(())
    }

    fn save_session_composite(
        &self,
        session_id: &str,
        name: &str,
        source: &str,
        updated_at_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO session_composites (session_id, name, source, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id, name) DO UPDATE SET
                 source     = excluded.source,
                 updated_at = excluded.updated_at",
            rusqlite::params![session_id, name, source, updated_at_ms],
        )
        .map_err(map_sql)?;
        Ok(())
    }

    fn session_composites(&self, session_id: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT name, source FROM session_composites
                 WHERE session_id = ?1 ORDER BY updated_at ASC, name ASC",
            )
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([session_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(map_sql)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(map_sql)?);
        }
        Ok(out)
    }
}
