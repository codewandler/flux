//! `flux-session` — durable, resumable sessions backed by SQLite (WAL).
//!
//! A session is an append-only log of [`Message`]s. The store reconstructs the conversation by
//! replaying the log in order, so resuming (`--continue` / `--session <id>`) is just a reload.
//! Session ids are `s_<rowid>` from the `sessions` table.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use flux_core::{Error, Message, Result};

fn map_sql<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(format!("session store: {e}"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn parse_id(id: &str) -> Result<i64> {
    id.strip_prefix("s_")
        .and_then(|n| n.parse::<i64>().ok())
        .ok_or_else(|| Error::Other(format!("invalid session id: {id:?}")))
}

/// Metadata about a session.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub model: String,
    pub created_at_ms: i64,
}

/// A one-line session summary for listings (`flux sessions` / the REPL `/sessions`).
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub model: String,
    pub created_at_ms: i64,
    pub messages: usize,
}

/// A SQLite-backed session store.
pub struct SessionStore {
    conn: Mutex<Connection>,
}

impl SessionStore {
    /// Open (creating if needed) a store at `path`, with WAL enabled for concurrent access.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_sql)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(map_sql)?;
        Self::init(conn)
    }

    /// An in-memory store (for tests).
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(map_sql)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 model      TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS messages (
                 session_id INTEGER NOT NULL,
                 seq        INTEGER NOT NULL,
                 data       TEXT NOT NULL,
                 PRIMARY KEY (session_id, seq)
             );",
        )
        .map_err(map_sql)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create a new session and return its id.
    pub fn create_session(&self, model: &str) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (model, created_at) VALUES (?1, ?2)",
            rusqlite::params![model, now_ms()],
        )
        .map_err(map_sql)?;
        Ok(format!("s_{}", conn.last_insert_rowid()))
    }

    /// The most recently created session id, if any (for `--continue`).
    pub fn latest_session_id(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        // Distinguish "no sessions yet" from a real DB error — `.ok()` used to swallow lock/disk/
        // corruption errors, silently starting a fresh session on `--continue` instead of failing.
        let id: Option<i64> = match conn.query_row(
            "SELECT id FROM sessions ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        ) {
            Ok(n) => Some(n),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(map_sql(e)),
        };
        Ok(id.map(|n| format!("s_{n}")))
    }

    /// Fetch session metadata.
    pub fn info(&self, id: &str) -> Result<SessionInfo> {
        let rowid = parse_id(id)?;
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT model, created_at FROM sessions WHERE id = ?1",
            [rowid],
            |r| {
                Ok(SessionInfo {
                    id: id.to_string(),
                    model: r.get(0)?,
                    created_at_ms: r.get(1)?,
                })
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::Other(format!("session {id} not found")),
            other => map_sql(other), // a real DB error must not masquerade as "not found"
        })
    }

    /// The most recent sessions (newest first), with their message counts, for listing/resuming.
    pub fn list(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.model, s.created_at, \
                 (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) \
                 FROM sessions s ORDER BY s.id DESC LIMIT ?1",
            )
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([limit as i64], |r| {
                let rowid: i64 = r.get(0)?;
                let n: i64 = r.get(3)?;
                Ok(SessionSummary {
                    id: format!("s_{rowid}"),
                    model: r.get(1)?,
                    created_at_ms: r.get(2)?,
                    messages: n as usize,
                })
            })
            .map_err(map_sql)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sql)
    }

    /// Update the model recorded for a session (so listings reflect a mid-session `/model` switch).
    pub fn set_model(&self, id: &str, model: &str) -> Result<()> {
        let rowid = parse_id(id)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET model = ?1 WHERE id = ?2",
            rusqlite::params![model, rowid],
        )
        .map_err(map_sql)?;
        Ok(())
    }

    /// Append a message to the session log.
    pub fn append_message(&self, id: &str, msg: &Message) -> Result<()> {
        let rowid = parse_id(id)?;
        let data = serde_json::to_string(msg)?;
        let conn = self.conn.lock().unwrap();
        let next_seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(seq), -1) + 1 FROM messages WHERE session_id = ?1",
                [rowid],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        conn.execute(
            "INSERT INTO messages (session_id, seq, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![rowid, next_seq, data],
        )
        .map_err(map_sql)?;
        Ok(())
    }

    /// Replace a session's entire message log with `messages` (re-sequenced from 0), atomically.
    /// Used by context compaction to swap old turns for a summary.
    pub fn rewrite_messages(&self, id: &str, messages: &[Message]) -> Result<()> {
        let rowid = parse_id(id)?;
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction().map_err(map_sql)?;
        tx.execute("DELETE FROM messages WHERE session_id = ?1", [rowid])
            .map_err(map_sql)?;
        for (seq, msg) in messages.iter().enumerate() {
            let data = serde_json::to_string(msg)?;
            tx.execute(
                "INSERT INTO messages (session_id, seq, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![rowid, seq as i64, data],
            )
            .map_err(map_sql)?;
        }
        tx.commit().map_err(map_sql)?;
        Ok(())
    }

    /// Replay the full conversation for a session.
    pub fn load_messages(&self, id: &str) -> Result<Vec<Message>> {
        let rowid = parse_id(id)?;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT data FROM messages WHERE session_id = ?1 ORDER BY seq")
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([rowid], |r| r.get::<_, String>(0))
            .map_err(map_sql)?;
        let mut out = Vec::new();
        for row in rows {
            let data = row.map_err(map_sql)?;
            out.push(serde_json::from_str(&data)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::Message;

    #[test]
    fn create_append_load_roundtrip() {
        let store = SessionStore::in_memory().unwrap();
        let id = store.create_session("claude-sonnet-4-6").unwrap();
        assert!(id.starts_with("s_"));

        store
            .append_message(&id, &Message::user_text("hello"))
            .unwrap();
        store
            .append_message(&id, &Message::assistant_text("hi there"))
            .unwrap();

        let msgs = store.load_messages(&id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text(), "hello");
        assert_eq!(msgs[1].text(), "hi there");

        let info = store.info(&id).unwrap();
        assert_eq!(info.model, "claude-sonnet-4-6");
    }

    #[test]
    fn rewrite_messages_replaces_the_log() {
        let store = SessionStore::in_memory().unwrap();
        let id = store.create_session("m").unwrap();
        for i in 0..5 {
            store
                .append_message(&id, &Message::user_text(format!("m{i}")))
                .unwrap();
        }
        assert_eq!(store.load_messages(&id).unwrap().len(), 5);

        store
            .rewrite_messages(
                &id,
                &[Message::user_text("summary"), Message::user_text("recent")],
            )
            .unwrap();
        let msgs = store.load_messages(&id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text(), "summary");
        assert_eq!(msgs[1].text(), "recent");

        // appending after a rewrite continues from the new sequence
        store
            .append_message(&id, &Message::user_text("more"))
            .unwrap();
        assert_eq!(store.load_messages(&id).unwrap().len(), 3);
    }

    #[test]
    fn latest_session_tracks_newest() {
        let store = SessionStore::in_memory().unwrap();
        assert!(store.latest_session_id().unwrap().is_none());
        let _a = store.create_session("m").unwrap();
        let b = store.create_session("m").unwrap();
        assert_eq!(store.latest_session_id().unwrap(), Some(b));
    }

    #[test]
    fn bad_id_errors() {
        let store = SessionStore::in_memory().unwrap();
        assert!(store.load_messages("nope").is_err());
        assert!(store.info("s_999").is_err());
    }

    #[test]
    fn list_returns_newest_first_with_counts() {
        let store = SessionStore::in_memory().unwrap();
        let a = store.create_session("m1").unwrap();
        store.append_message(&a, &Message::user_text("hi")).unwrap();
        store
            .append_message(&a, &Message::user_text("there"))
            .unwrap();
        let b = store.create_session("m2").unwrap();

        let list = store.list(10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, b, "newest first");
        assert_eq!(list[0].messages, 0);
        assert_eq!(list[1].id, a);
        assert_eq!(list[1].messages, 2);
        assert_eq!(list[1].model, "m1");
        // limit is honored
        assert_eq!(store.list(1).unwrap().len(), 1);
    }

    #[test]
    fn set_model_updates_listing() {
        let store = SessionStore::in_memory().unwrap();
        let a = store.create_session("sonnet").unwrap();
        store.set_model(&a, "opus").unwrap();
        assert_eq!(store.list(1).unwrap()[0].model, "opus");
        assert_eq!(store.info(&a).unwrap().model, "opus");
    }
}
