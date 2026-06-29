//! [`SqliteBackend`] — a persistent datasource index backed by SQLite + FTS5.
//!
//! Records live in a `records` table keyed by `(source, entity, id)`; an FTS5 virtual table
//! `records_fts` mirrors each record's `title`+`body` for keyword search ranked by the built-in
//! `bm25()` (the workspace `rusqlite` is `bundled`, so FTS5 is compiled in). Mirrors flux-events'
//! `Connection`+WAL pattern and serializes access with a `Mutex`.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use flux_core::{Error, Result};
use flux_datasource::{
    BatchGetInput, GetInput, Link, ListInput, Match, Record, RelationInput, SearchInput, Source,
};

use super::DatasourceBackend;

fn map_sql<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(format!("datasource sqlite: {e}"))
}

/// A persistent, FTS5-backed datasource index.
pub struct SqliteBackend {
    conn: Mutex<Connection>,
}

impl SqliteBackend {
    /// Open (creating if needed) a persistent index at `path`, WAL enabled.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_sql)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(map_sql)?;
        Self::init(conn)
    }

    /// An in-memory index (tests / ephemeral use).
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(map_sql)?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS records (
                 source TEXT NOT NULL,
                 entity TEXT NOT NULL,
                 id     TEXT NOT NULL,
                 title  TEXT NOT NULL DEFAULT '',
                 body   TEXT NOT NULL DEFAULT '',
                 links  TEXT NOT NULL DEFAULT '[]',
                 meta   TEXT NOT NULL DEFAULT 'null',
                 PRIMARY KEY (source, entity, id)
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS records_fts USING fts5(
                 title, body,
                 source UNINDEXED, entity UNINDEXED, id UNINDEXED
             );",
        )
        .map_err(map_sql)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

/// Build a record from a `records` row's columns.
fn row_to_record(
    source_key: &str,
    entity: String,
    id: String,
    title: String,
    body: String,
    links_json: &str,
    meta_json: &str,
) -> Record {
    let (plugin, instance) = match source_key.split_once('/') {
        Some((p, i)) => (p.to_string(), Some(i.to_string())),
        None => (source_key.to_string(), None),
    };
    let links: Vec<Link> = serde_json::from_str(links_json).unwrap_or_default();
    let meta: Value = serde_json::from_str(meta_json).unwrap_or(Value::Null);
    Record {
        entity,
        id,
        source: Source { plugin, instance },
        title,
        body,
        links,
        meta,
    }
}

/// Quote each whitespace term as an FTS5 phrase and OR them (keyword recall, parity with the in-memory
/// ranker). Returns `None` for a blank query.
fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

impl DatasourceBackend for SqliteBackend {
    fn upsert(&self, records: &[Record]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(map_sql)?;
        for r in records {
            let source = r.source.key();
            let links = serde_json::to_string(&r.links).map_err(map_sql)?;
            let meta = serde_json::to_string(&r.meta).map_err(map_sql)?;
            // Replace any prior row + its FTS mirror, then re-insert both (keeps FTS in sync).
            tx.execute(
                "DELETE FROM records WHERE source=?1 AND entity=?2 AND id=?3",
                params![source, r.entity, r.id],
            )
            .map_err(map_sql)?;
            tx.execute(
                "DELETE FROM records_fts WHERE source=?1 AND entity=?2 AND id=?3",
                params![source, r.entity, r.id],
            )
            .map_err(map_sql)?;
            tx.execute(
                "INSERT INTO records (source, entity, id, title, body, links, meta)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![source, r.entity, r.id, r.title, r.body, links, meta],
            )
            .map_err(map_sql)?;
            tx.execute(
                "INSERT INTO records_fts (title, body, source, entity, id)
                 VALUES (?1,?2,?3,?4,?5)",
                params![r.title, r.body, source, r.entity, r.id],
            )
            .map_err(map_sql)?;
        }
        tx.commit().map_err(map_sql)
    }

    fn search(&self, input: &SearchInput) -> Result<Vec<Match>> {
        let Some(match_expr) = fts_query(&input.query) else {
            return Ok(Vec::new());
        };
        let limit = input.limit.unwrap_or(5) as i64;
        let conn = self.conn.lock().unwrap();
        // bm25() is smaller-is-better; negate so higher = better (parity with MemoryBackend).
        let mut sql = String::from(
            "SELECT f.source, f.entity, f.id, -bm25(records_fts) AS score
             FROM records_fts f WHERE records_fts MATCH ?1",
        );
        if input.source.is_some() {
            sql.push_str(" AND f.source = ?2");
        }
        if input.entity.is_some() {
            sql.push_str(if input.source.is_some() {
                " AND f.entity = ?3"
            } else {
                " AND f.entity = ?2"
            });
        }
        sql.push_str(" ORDER BY score DESC, f.id ASC LIMIT ?_lim");
        // rusqlite has no named-after-positional mix here; build the final param list by position.
        let sql = sql.replace("?_lim", "?");
        let mut stmt = conn.prepare(&sql).map_err(map_sql)?;

        // Assemble positional params in the order they appear in `sql`.
        let mut binds: Vec<rusqlite::types::Value> = vec![match_expr.into()];
        if let Some(s) = &input.source {
            binds.push(s.clone().into());
        }
        if let Some(e) = &input.entity {
            binds.push(e.clone().into());
        }
        binds.push(limit.into());

        let rows = stmt
            .query_map(rusqlite::params_from_iter(binds), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            })
            .map_err(map_sql)?;

        let mut out = Vec::new();
        for row in rows {
            let (source, entity, id, score) = row.map_err(map_sql)?;
            if let Some(mut rec) = get_record(&conn, &source, &entity, &id)? {
                let matched = matched_fields(&rec, &input.query);
                rec.body = snippet(&rec.body, &input.query);
                out.push(Match {
                    record: rec,
                    score,
                    matched_fields: matched,
                });
            }
        }
        Ok(out)
    }

    fn get(&self, input: &GetInput) -> Result<Option<Record>> {
        let conn = self.conn.lock().unwrap();
        get_record(&conn, &input.source, &input.entity, &input.id)
    }

    fn list(&self, input: &ListInput) -> Result<Vec<Record>> {
        let conn = self.conn.lock().unwrap();
        let limit = input.limit.map(|n| n as i64).unwrap_or(-1); // -1 = no limit in SQLite
        let offset = input.offset.unwrap_or(0) as i64;
        let (sql, with_entity) = match &input.entity {
            Some(_) => (
                "SELECT source, entity, id, title, body, links, meta FROM records
                 WHERE source=?1 AND entity=?2 ORDER BY entity, id LIMIT ?3 OFFSET ?4",
                true,
            ),
            None => (
                "SELECT source, entity, id, title, body, links, meta FROM records
                 WHERE source=?1 ORDER BY entity, id LIMIT ?2 OFFSET ?3",
                false,
            ),
        };
        let mut stmt = conn.prepare(sql).map_err(map_sql)?;
        let map = |row: &rusqlite::Row| {
            Ok(row_to_record(
                &row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                &row.get::<_, String>(5)?,
                &row.get::<_, String>(6)?,
            ))
        };
        let rows = if with_entity {
            stmt.query_map(
                params![input.source, input.entity.as_ref().unwrap(), limit, offset],
                map,
            )
        } else {
            stmt.query_map(params![input.source, limit, offset], map)
        }
        .map_err(map_sql)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(map_sql)
    }

    fn relation(&self, input: &RelationInput) -> Result<Vec<Record>> {
        let conn = self.conn.lock().unwrap();
        let Some(origin) = get_record(&conn, &input.source, &input.entity, &input.id)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for link in &origin.links {
            if input.rel.as_deref().is_some_and(|rel| link.rel != rel) {
                continue;
            }
            if let Some(rec) =
                get_record(&conn, &input.source, &link.target_entity, &link.target_id)?
            {
                out.push(rec);
            }
        }
        Ok(out)
    }

    fn batch_get(&self, input: &BatchGetInput) -> Result<Vec<Record>> {
        let conn = self.conn.lock().unwrap();
        let mut out = Vec::new();
        for id in &input.ids {
            if let Some(rec) = get_record(&conn, &input.source, &input.entity, id)? {
                out.push(rec);
            }
        }
        Ok(out)
    }

    fn clear(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("DELETE FROM records; DELETE FROM records_fts;")
            .map_err(map_sql)
    }

    fn len(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM records", [], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .unwrap_or(0)
    }
}

/// Fetch one full record by address from an open connection.
fn get_record(conn: &Connection, source: &str, entity: &str, id: &str) -> Result<Option<Record>> {
    conn.query_row(
        "SELECT source, entity, id, title, body, links, meta FROM records
         WHERE source=?1 AND entity=?2 AND id=?3",
        params![source, entity, id],
        |row| {
            Ok(row_to_record(
                &row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                &row.get::<_, String>(5)?,
                &row.get::<_, String>(6)?,
            ))
        },
    )
    .optional()
    .map_err(map_sql)
}

/// Which fields (`title`/`body`) contain any query term (case-insensitive).
fn matched_fields(record: &Record, query: &str) -> Vec<String> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    let title = record.title.to_lowercase();
    let body = record.body.to_lowercase();
    let mut out = Vec::new();
    if terms.iter().any(|t| title.contains(t.as_str())) {
        out.push("title".to_string());
    }
    if terms.iter().any(|t| body.contains(t.as_str())) {
        out.push("body".to_string());
    }
    out
}

/// A ~160-char snippet around the first matching term in `body`.
fn snippet(body: &str, query: &str) -> String {
    let lower = body.to_lowercase();
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    let byte_pos = terms
        .iter()
        .filter_map(|t| lower.find(t.as_str()))
        .min()
        .unwrap_or(0);
    let pos = lower.get(..byte_pos).map_or(0, |s| s.chars().count());
    let start = pos.saturating_sub(40);
    let take = 160;
    let snip: String = body.chars().skip(start).take(take).collect();
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(snip.trim());
    if start + take < body.chars().count() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, title: &str, body: &str) -> Record {
        Record::new(Source::new("local"), "file.document", id, title, body)
    }

    #[test]
    fn search_get_and_persistence() {
        let dir = std::env::temp_dir().join(format!("flux-ds-sqlite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("datasource.db");
        {
            let b = SqliteBackend::open(&path).unwrap();
            b.upsert(&[
                doc(
                    "warm-transfer",
                    "Warm transfer",
                    "A warm transfer connects the caller to an agent after an announcement.",
                ),
                doc(
                    "cold-transfer",
                    "Cold transfer",
                    "A blind transfer with no announcement.",
                ),
            ])
            .unwrap();
            let hits = b
                .search(&SearchInput {
                    query: "warm transfer".into(),
                    limit: Some(5),
                    ..Default::default()
                })
                .unwrap();
            assert!(!hits.is_empty());
            assert_eq!(hits[0].record.id, "warm-transfer", "best match ranks first");
        }
        // Reopen the store: the data persists (durability).
        {
            let b = SqliteBackend::open(&path).unwrap();
            assert_eq!(b.len(), 2);
            let got = b
                .get(&GetInput {
                    source: "local".into(),
                    entity: "file.document".into(),
                    id: "warm-transfer".into(),
                })
                .unwrap()
                .unwrap();
            assert_eq!(got.title, "Warm transfer");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upsert_replaces_and_fts_stays_in_sync() {
        let b = SqliteBackend::in_memory().unwrap();
        b.upsert(&[doc("x", "alpha", "first body")]).unwrap();
        b.upsert(&[doc("x", "beta", "second body")]).unwrap();
        assert_eq!(b.len(), 1);
        // The old title is gone from FTS; the new one matches.
        assert!(b
            .search(&SearchInput {
                query: "alpha".into(),
                ..Default::default()
            })
            .unwrap()
            .is_empty());
        assert!(!b
            .search(&SearchInput {
                query: "beta".into(),
                ..Default::default()
            })
            .unwrap()
            .is_empty());
    }
}
