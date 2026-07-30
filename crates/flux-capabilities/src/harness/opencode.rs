//! Message extraction from opencode's SQLite database.
//!
//! Shape, as observed in `~/.local/share/opencode/opencode.db`: a `message` row carries the
//! envelope — `role`, `modelID`/`providerID`, `path.cwd`, `tokens` — as JSON in its `data` column,
//! and the **text is not in it**. Bodies live in a separate `part` table keyed by `message_id`, one
//! row per typed part (`text`, `reasoning`, `tool`, `step-start`, …). That is the difference
//! between this and `flux usage`: token counts are in `message.data`, so the usage parser never had
//! to know the `part` table existed.
//!
//! Everything about the schema is probed rather than assumed — opencode's own schema has drifted
//! across releases, and an unexpected one must degrade to an empty scan, never to an error.

use std::collections::BTreeMap;
use std::path::Path;

use flux_core::Result;
use rusqlite::Connection;
use serde_json::Value;

use super::message::{
    flatten_content, json_epoch_ms_at, normalize_epoch_ms, HarnessMessage, MessageRole,
};
use super::scan::{open_sqlite_read_only, sqlite_column_exists, sqlite_table_exists, ScanBudget};
use super::{HarnessKind, MessageSink, MessageStats};

/// Extract every message from an opencode database.
///
/// The database is opened **read-only**; only failing to open it is an error.
pub fn opencode_messages(
    db: &Path,
    budget: ScanBudget,
    emit: &mut dyn FnMut(HarnessMessage),
) -> Result<MessageStats> {
    let conn = open_sqlite_read_only(db)?;
    let mut sink = MessageSink::new(budget, emit);
    if !sqlite_table_exists(&conn, "message")? {
        return Ok(sink.into_stats());
    }
    let schema = Schema::probe(&conn)?;
    let directories = session_directories(&conn)?;

    let mut stmt = conn
        .prepare(&schema.sql())
        .map_err(|e| flux_core::Error::Other(e.to_string()))?;
    // The body cap is applied in the query so an enormous body is never transferred: `length` says
    // how big it really is, `substr` bounds what comes back. SQLite counts text in characters, so
    // this bounds bytes within UTF-8's constant factor rather than exactly — close enough for a
    // memory bound, and `MessageSink::offer` still rejects on the true byte length.
    let cap = i64::try_from(budget.max_message_bytes).unwrap_or(i64::MAX);
    let mut rows = stmt
        .query(rusqlite::params![cap])
        .map_err(|e| flux_core::Error::Other(e.to_string()))?;

    let mut ordinals = BTreeMap::<String, u32>::new();
    let mut open: Option<Pending> = None;

    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(_) => {
                // A row that will not decode is one skipped record, not a failed scan.
                sink.skip_malformed();
                break;
            }
        };
        let Ok(id) = row.get::<_, String>(0) else {
            sink.skip_malformed();
            continue;
        };
        if open.as_ref().is_some_and(|p| p.id != id) {
            // The join fans one message across its part rows, so a message is complete exactly when
            // the next id arrives.
            let complete = open.take();
            if !flush(complete, &mut sink, &mut ordinals, db, &directories, budget) {
                return Ok(sink.into_stats());
            }
        }
        if open.is_none() {
            sink.scanned();
            let session_id: Option<String> = row.get(1).ok().flatten();
            let created: Option<i64> = row.get::<_, Option<i64>>(2).ok().flatten();
            let over = row.get::<_, i64>(3).unwrap_or(0) > cap;
            let data: String = row.get(4).unwrap_or_default();
            let data = match serde_json::from_str::<Value>(&data) {
                Ok(data) => data,
                Err(_) => {
                    sink.skip_malformed();
                    // The whole message is unusable, but its part rows keep arriving; hold an
                    // explicitly poisoned entry so they are dropped rather than reattributed.
                    open = Some(Pending::poisoned(id));
                    continue;
                }
            };
            open = Some(Pending {
                id,
                session_id,
                created,
                data,
                parts: Vec::new(),
                bytes: 0,
                oversize: over,
                poisoned: false,
            });
        }
        let pending = open.as_mut().expect("just opened");
        if pending.poisoned || pending.oversize {
            continue;
        }
        if schema.has_parts {
            let part_over = row.get::<_, Option<i64>>(5).ok().flatten().unwrap_or(0) > cap;
            let part: Option<String> = row.get(6).ok().flatten();
            if part_over {
                pending.oversize = true;
                pending.parts.clear();
                continue;
            }
            if let Some(part) = part {
                match serde_json::from_str::<Value>(&part) {
                    Ok(part) => {
                        if is_structural(&part) {
                            continue;
                        }
                        pending.bytes +=
                            part.get("text").and_then(Value::as_str).map_or(0, str::len);
                        if pending.bytes > budget.max_message_bytes {
                            pending.oversize = true;
                            pending.parts.clear();
                        } else {
                            pending.parts.push(part);
                        }
                    }
                    Err(_) => sink.skip_malformed(),
                }
            }
        }
    }
    flush(
        open.take(),
        &mut sink,
        &mut ordinals,
        db,
        &directories,
        budget,
    );
    Ok(sink.into_stats())
}

/// Whether a part is turn bookkeeping rather than anything that was said.
///
/// opencode files these alongside the real content — they are the single most common part type in
/// a mature database — and the flattener's rule that an unrecognized block still leaves a `[type]`
/// marker would otherwise stamp every assistant body with `[step-start]` / `[step-finish]`. They
/// are named explicitly rather than filtered by "has no text", so a future part type that carries
/// content is surfaced rather than silently dropped.
fn is_structural(part: &Value) -> bool {
    matches!(
        part.get("type").and_then(Value::as_str),
        Some("step-start" | "step-finish" | "snapshot")
    )
}

/// One message being assembled from its (message row, part row) fan-out.
struct Pending {
    id: String,
    session_id: Option<String>,
    created: Option<i64>,
    data: Value,
    parts: Vec<Value>,
    bytes: usize,
    oversize: bool,
    poisoned: bool,
}

impl Pending {
    fn poisoned(id: String) -> Self {
        Self {
            id,
            session_id: None,
            created: None,
            data: Value::Null,
            parts: Vec::new(),
            bytes: 0,
            oversize: false,
            poisoned: true,
        }
    }
}

/// Emit one assembled message. Returns whether the scan should continue.
fn flush(
    pending: Option<Pending>,
    sink: &mut MessageSink<'_>,
    ordinals: &mut BTreeMap<String, u32>,
    db: &Path,
    directories: &BTreeMap<String, String>,
    budget: ScanBudget,
) -> bool {
    let Some(pending) = pending else {
        return true;
    };
    if pending.poisoned {
        return true;
    }
    if pending.oversize {
        sink.skip_oversize();
        return true;
    }
    let Some(role) = pending
        .data
        .get("role")
        .and_then(Value::as_str)
        .and_then(MessageRole::normalize)
    else {
        sink.skip_malformed();
        return true;
    };
    let session_id = pending
        .session_id
        .clone()
        .or_else(|| {
            pending
                .data
                .get("sessionID")
                .or_else(|| pending.data.get("session_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| pending.id.clone());

    // Parts are the body when the schema has them; older schemas kept it inline on the message.
    let text = if pending.parts.is_empty() {
        ["content", "parts", "text"]
            .into_iter()
            .find_map(|key| pending.data.get(key))
            .map(|content| flatten_content(content, budget.max_message_bytes))
            .unwrap_or_default()
    } else {
        flatten_content(&Value::Array(pending.parts), budget.max_message_bytes)
    };

    let ordinal = ordinals.entry(session_id.clone()).or_default();
    let message = HarnessMessage {
        harness: HarnessKind::Opencode,
        session_id: session_id.clone(),
        index: *ordinal,
        role,
        text,
        model: pending
            .data
            .get("modelID")
            .or_else(|| pending.data.get("model"))
            .and_then(Value::as_str)
            .map(str::to_string),
        workspace: pending
            .data
            .pointer("/path/cwd")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| directories.get(&session_id).cloned()),
        ts_ms: pending
            .created
            .map(normalize_epoch_ms)
            .or_else(|| json_epoch_ms_at(&pending.data, &["time", "created"])),
        path: db.to_path_buf(),
    };
    *ordinal += 1;
    sink.offer(message)
}

/// Which of the columns this adapter would like to use actually exist.
struct Schema {
    message_session_id: bool,
    message_time_created: bool,
    has_parts: bool,
    part_time_created: bool,
}

impl Schema {
    fn probe(conn: &Connection) -> Result<Self> {
        let has_parts = sqlite_table_exists(conn, "part")?
            && sqlite_column_exists(conn, "part", "message_id")?
            && sqlite_column_exists(conn, "part", "data")?;
        Ok(Self {
            message_session_id: sqlite_column_exists(conn, "message", "session_id")?,
            message_time_created: sqlite_column_exists(conn, "message", "time_created")?,
            has_parts,
            part_time_created: has_parts && sqlite_column_exists(conn, "part", "time_created")?,
        })
    }

    /// One ordered pass over `message` left-joined to `part`.
    ///
    /// A left join rather than a per-message lookup on purpose: the rows arrive grouped by message,
    /// so a body is assembled from a stream and forgotten, and a database with years of history
    /// costs one message of memory rather than one query per message.
    fn sql(&self) -> String {
        let session = if self.message_session_id {
            "m.session_id"
        } else {
            "null"
        };
        let created = if self.message_time_created {
            "m.time_created"
        } else {
            "null"
        };
        let (part_columns, join) = if self.has_parts {
            (
                "length(p.data), substr(p.data, 1, ?1)",
                "left join part p on p.message_id = m.id",
            )
        } else {
            ("null, null", "")
        };
        // Ordering must be total, or `index` addresses a different message on the next scan.
        let mut order = Vec::new();
        if self.message_time_created {
            order.push("m.time_created");
        }
        order.push("m.id");
        if self.part_time_created {
            order.push("p.time_created");
        }
        if self.has_parts {
            order.push("p.id");
        }
        format!(
            "select m.id, {session}, {created}, length(m.data), substr(m.data, 1, ?1), \
             {part_columns} from message m {join} order by {}",
            order.join(", ")
        )
    }
}

/// `session id -> directory`, when the schema records one. A message that names no workspace of its
/// own inherits its session's — bounded by the session count, not the message count.
fn session_directories(conn: &Connection) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    if !(sqlite_table_exists(conn, "session")?
        && sqlite_column_exists(conn, "session", "directory")?)
    {
        return Ok(out);
    }
    let mut stmt = conn
        .prepare("select id, directory from session")
        .map_err(|e| flux_core::Error::Other(e.to_string()))?;
    let mut rows = stmt
        .query([])
        .map_err(|e| flux_core::Error::Other(e.to_string()))?;
    while let Ok(Some(row)) = rows.next() {
        if let (Ok(id), Ok(Some(directory))) =
            (row.get::<_, String>(0), row.get::<_, Option<String>>(1))
        {
            out.insert(id, directory);
        }
    }
    Ok(out)
}
