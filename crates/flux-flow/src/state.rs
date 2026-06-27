//! flux-flow's own durable store: the immutable value store, the session symbol table, and the
//! run-event trace. It is deliberately separate from `flux-session` (which owns the provider message
//! log) — flux-flow keeps its execution facts in its own SQLite database, so the message-log
//! invariants are never entangled with flow state.
//!
//! Values are append-only and versioned: a revision creates a new [`ValueId`] and the old version
//! stays addressable. A symbol points at its *current* value; the symbol table is the model-facing
//! projection mechanism, and only visible/pinned symbols appear in [`FlowStore::view`].

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use flux_core::{Error, Result};

use crate::ast::{Node, NodeId, RunEvent, SymbolName, Value, ValueId, Visibility};

fn map_sql<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(format!("flow store: {e}"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn value_rowid(id: &ValueId) -> Result<i64> {
    id.0.strip_prefix("v_")
        .and_then(|n| n.parse::<i64>().ok())
        .ok_or_else(|| Error::Other(format!("invalid value id: {:?}", id.0)))
}

/// The model-facing session-projection types live in the language crate ([`flux_lang::store`]);
/// re-exported so `flux_flow::state::{SessionView, SymbolView}` paths are unchanged.
pub use flux_lang::store::{SessionView, SymbolView};

/// flux-flow's durable [`FlowStore`] is the engine's [`ValueStore`](flux_lang::store::ValueStore):
/// the interpreter (in `flux-lang`) reads and writes session state through this trait, with the SQLite
/// implementation staying here. Methods forward to the inherent ones (inherent methods win in
/// `self.method()` resolution, so there is no recursion).
impl flux_lang::store::ValueStore for FlowStore {
    fn put_value(&self, session_id: &str, value: &Value) -> Result<ValueId> {
        self.put_value(session_id, value)
    }
    fn get_value(&self, id: &ValueId) -> Result<Option<Value>> {
        self.get_value(id)
    }
    fn bind(
        &self,
        session_id: &str,
        name: &SymbolName,
        vid: &ValueId,
        ty: Option<&str>,
        summary: &str,
        visibility: Visibility,
    ) -> Result<()> {
        self.bind(session_id, name, vid, ty, summary, visibility)
    }
    fn resolve(&self, session_id: &str, name: &SymbolName) -> Result<Option<ValueId>> {
        self.resolve(session_id, name)
    }
    fn append_event(&self, session_id: &str, event: &RunEvent) -> Result<()> {
        self.append_event(session_id, event)
    }
    fn view(&self, session_id: &str) -> Result<SessionView> {
        self.view(session_id)
    }
}

/// flux-flow's own SQLite store for values, symbols, and the run-event trace.
pub struct FlowStore {
    conn: Mutex<Connection>,
}

impl FlowStore {
    /// Open (creating if needed) a store at `path`, with WAL enabled.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_sql)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(map_sql)?;
        Self::init(conn)
    }

    /// An in-memory store (for tests).
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(map_sql)?)
    }

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
             CREATE TABLE IF NOT EXISTS run_events (
                 session_id TEXT NOT NULL,
                 seq        INTEGER NOT NULL,
                 data       TEXT NOT NULL,
                 PRIMARY KEY (session_id, seq)
             );
             CREATE TABLE IF NOT EXISTS suspensions (
                 session_id TEXT PRIMARY KEY,
                 body       TEXT NOT NULL,
                 node       INTEGER NOT NULL,
                 source     TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );",
        )
        .map_err(map_sql)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Store an immutable value and return its id. Values are append-only — a revision creates a new
    /// id; old versions remain addressable for audit and re-run.
    pub fn put_value(&self, session_id: &str, value: &Value) -> Result<ValueId> {
        let data = serde_json::to_string(value)?;
        let bytes = data.len() as i64;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO values_store (session_id, data, bytes, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, data, bytes, now_ms()],
        )
        .map_err(map_sql)?;
        Ok(ValueId(format!("v_{}", conn.last_insert_rowid())))
    }

    /// Fetch a stored value by id.
    pub fn get_value(&self, id: &ValueId) -> Result<Option<Value>> {
        let n = value_rowid(id)?;
        let conn = self.conn.lock().unwrap();
        let data: Option<String> =
            match conn.query_row("SELECT data FROM values_store WHERE n = ?1", [n], |r| {
                r.get(0)
            }) {
                Ok(d) => Some(d),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(map_sql(e)),
            };
        match data {
            Some(d) => Ok(Some(serde_json::from_str(&d)?)),
            None => Ok(None),
        }
    }

    /// Bind a symbol to a value (creating it or moving the pointer). The previous value stays stored.
    pub fn bind(
        &self,
        session_id: &str,
        name: &SymbolName,
        value_id: &ValueId,
        ty: Option<&str>,
        summary: &str,
        visibility: Visibility,
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
                name.0,
                value_id.0,
                ty,
                summary,
                visibility.as_str(),
                now_ms()
            ],
        )
        .map_err(map_sql)?;
        Ok(())
    }

    /// Resolve a symbol to its current value id.
    pub fn resolve(&self, session_id: &str, name: &SymbolName) -> Result<Option<ValueId>> {
        let conn = self.conn.lock().unwrap();
        match conn.query_row(
            "SELECT value_id FROM symbols WHERE session_id = ?1 AND name = ?2",
            rusqlite::params![session_id, name.0],
            |r| r.get::<_, String>(0),
        ) {
            Ok(v) => Ok(Some(ValueId(v))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(map_sql(e)),
        }
    }

    /// Append a run-event to the session's trace.
    pub fn append_event(&self, session_id: &str, event: &RunEvent) -> Result<()> {
        let data = serde_json::to_string(event)?;
        let conn = self.conn.lock().unwrap();
        let seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(seq), -1) + 1 FROM run_events WHERE session_id = ?1",
                [session_id],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        conn.execute(
            "INSERT INTO run_events (session_id, seq, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![session_id, seq, data],
        )
        .map_err(map_sql)?;
        Ok(())
    }

    /// Load the run-event trace for a session.
    pub fn events(&self, session_id: &str) -> Result<Vec<RunEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT data FROM run_events WHERE session_id = ?1 ORDER BY seq")
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([session_id], |r| r.get::<_, String>(0))
            .map_err(map_sql)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row.map_err(map_sql)?)?);
        }
        Ok(out)
    }

    /// Persist a flow suspended on a top-level `await`: its body, the suspended node index, and the
    /// awaited input `source`. One pending suspension per session — a new one replaces any prior.
    /// Resumed (and cleared) by [`take_suspension`] when the awaited input arrives next turn.
    pub fn save_suspension(
        &self,
        session_id: &str,
        body: &[Node],
        node: NodeId,
        source: &str,
    ) -> Result<()> {
        let body_json = serde_json::to_string(body)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO suspensions (session_id, body, node, source, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![session_id, body_json, node.0, source, now_ms()],
        )
        .map_err(map_sql)?;
        Ok(())
    }

    /// Take (load **and** remove) a session's pending suspension, if any — a one-shot resume point.
    /// Returns the persisted flow body, the suspended `await` node, and the awaited source.
    pub fn take_suspension(&self, session_id: &str) -> Result<Option<(Vec<Node>, NodeId, String)>> {
        let conn = self.conn.lock().unwrap();
        let row = conn.query_row(
            "SELECT body, node, source FROM suspensions WHERE session_id = ?1",
            [session_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        );
        match row {
            Ok((body_json, node, source)) => {
                // One-shot: clear the row regardless. A body that no longer deserializes (e.g. AST
                // schema drift across an upgrade) is discarded and reported as "no suspension" so the
                // turn recovers with a fresh compile rather than hard-erroring on every future turn.
                conn.execute(
                    "DELETE FROM suspensions WHERE session_id = ?1",
                    [session_id],
                )
                .map_err(map_sql)?;
                match serde_json::from_str::<Vec<Node>>(&body_json) {
                    Ok(body) => Ok(Some((body, NodeId(node as u32), source))),
                    Err(_) => Ok(None),
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(map_sql(e)),
        }
    }

    /// Total stored value bytes for a session (the budget-accounting surface; eviction lands later).
    pub fn total_value_bytes(&self, session_id: &str) -> Result<u64> {
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

    /// Project the model-facing view: visible + pinned symbols, newest-updated first, summaries only.
    pub fn view(&self, session_id: &str) -> Result<SessionView> {
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
        let mut symbols = Vec::new();
        for row in rows {
            let (name, ty, summary, vis) = row.map_err(map_sql)?;
            let visibility = Visibility::from_tag(&vis).unwrap_or(Visibility::Hidden);
            if visibility.is_shown() {
                symbols.push(SymbolView {
                    name: SymbolName(name),
                    ty,
                    summary,
                    visibility,
                });
            }
        }
        Ok(SessionView { symbols })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspensions_round_trip_take_once_and_replace() {
        let s = FlowStore::in_memory().unwrap();
        let body = vec![Node::Await {
            binding: Some(SymbolName("x".into())),
            source: "user_input".into(),
            as_type: None,
        }];
        assert!(
            s.take_suspension("sess").unwrap().is_none(),
            "none initially"
        );

        s.save_suspension("sess", &body, NodeId(3), "user_input")
            .unwrap();
        // A second save replaces the first (one pending suspension per session).
        s.save_suspension("sess", &body, NodeId(5), "other")
            .unwrap();

        let (got_body, node, source) = s.take_suspension("sess").unwrap().expect("a suspension");
        assert_eq!(node, NodeId(5), "latest save wins");
        assert_eq!(source, "other");
        assert_eq!(got_body, body);
        // Taking is one-shot — it's cleared.
        assert!(s.take_suspension("sess").unwrap().is_none(), "consumed");
    }

    #[test]
    fn values_are_versioned_and_old_versions_stay_addressable() {
        let s = FlowStore::in_memory().unwrap();
        let v1 = s
            .put_value("sess", &Value::String("draft one".into()))
            .unwrap();
        let v2 = s
            .put_value("sess", &Value::String("draft two".into()))
            .unwrap();
        assert!(v1.0.starts_with("v_"));
        assert_ne!(v1, v2);
        assert_eq!(
            s.get_value(&v1).unwrap(),
            Some(Value::String("draft one".into()))
        );
        assert_eq!(
            s.get_value(&v2).unwrap(),
            Some(Value::String("draft two".into()))
        );
        assert!(s.get_value(&ValueId("v_9999".into())).unwrap().is_none());
    }

    #[test]
    fn bind_moves_the_pointer_but_keeps_the_old_value() {
        let s = FlowStore::in_memory().unwrap();
        let draft = SymbolName("draft".into());
        let v1 = s.put_value("sess", &Value::String("v1".into())).unwrap();
        let v2 = s.put_value("sess", &Value::String("v2".into())).unwrap();

        s.bind(
            "sess",
            &draft,
            &v1,
            Some("Draft"),
            "first",
            Visibility::Visible,
        )
        .unwrap();
        assert_eq!(s.resolve("sess", &draft).unwrap(), Some(v1.clone()));

        s.bind(
            "sess",
            &draft,
            &v2,
            Some("Draft"),
            "second",
            Visibility::Visible,
        )
        .unwrap();
        assert_eq!(s.resolve("sess", &draft).unwrap(), Some(v2));
        // the superseded value is still retrievable
        assert_eq!(s.get_value(&v1).unwrap(), Some(Value::String("v1".into())));
    }

    #[test]
    fn view_shows_only_visible_and_pinned_symbols() {
        let s = FlowStore::in_memory().unwrap();
        let v = s.put_value("sess", &Value::String("x".into())).unwrap();
        s.bind(
            "sess",
            &SymbolName("a".into()),
            &v,
            Some("Draft"),
            "shown",
            Visibility::Visible,
        )
        .unwrap();
        s.bind(
            "sess",
            &SymbolName("b".into()),
            &v,
            None,
            "hidden one",
            Visibility::Hidden,
        )
        .unwrap();
        s.bind(
            "sess",
            &SymbolName("c".into()),
            &v,
            None,
            "pinned one",
            Visibility::Pinned,
        )
        .unwrap();

        let view = s.view("sess").unwrap();
        let names: Vec<String> = view.symbols.iter().map(|s| s.name.0.clone()).collect();
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"c".to_string()));
        assert!(
            !names.contains(&"b".to_string()),
            "a hidden symbol must not appear in the model-facing view"
        );
        assert!(view.render().contains("$a: Draft = shown"));
    }

    #[test]
    fn run_events_append_and_load_in_order() {
        let s = FlowStore::in_memory().unwrap();
        s.append_event(
            "sess",
            &RunEvent::StepSucceeded {
                step: "s1".into(),
                output: "v_1".into(),
            },
        )
        .unwrap();
        s.append_event(
            "sess",
            &RunEvent::FlowReturned {
                value: "v_1".into(),
            },
        )
        .unwrap();
        let events = s.events("sess").unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], RunEvent::StepSucceeded { .. }));
        assert!(matches!(events[1], RunEvent::FlowReturned { .. }));
    }

    #[test]
    fn byte_budget_accounts_for_stored_values() {
        let s = FlowStore::in_memory().unwrap();
        assert_eq!(s.total_value_bytes("sess").unwrap(), 0);
        s.put_value("sess", &Value::String("some content".into()))
            .unwrap();
        assert!(s.total_value_bytes("sess").unwrap() > 0);
    }
}
