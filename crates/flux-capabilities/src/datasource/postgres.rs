//! [`PostgresBackend`] — a [`DatasourceBackend`] over Postgres, for record stores (agent
//! registries, knowledge corpora — anything on the records + keyword-search trait) that must live in
//! a shared, multi-writer database instead of a local SQLite file.
//!
//! Isolation is **structural**, exactly like `SqliteBackend::open(<scope>.db)`: a `ns` column is part
//! of the primary key and bound **once at construction** (never per call), so one `ds_records` table
//! holds many logically-separate stores. Keyword search reaches FTS5/bm25 parity through a stored
//! generated `tsvector` column + GIN index: the same OR-of-quoted-terms string the SQLite backend
//! builds is fed to `websearch_to_tsquery('simple', …)` (which never errors on malformed user input)
//! and ranked by `ts_rank`. The `snippet` + `matched_fields` shaping is the shared [`super::text`]
//! logic, so a [`Match`] from this backend is byte-identical to one from SQLite.
//!
//! All trait methods are synchronous; each wraps its sqlx work in [`PgHandle::block_on`], the
//! panic-safe sync↔async bridge owned by `flux-pg` (the one crate that depends on sqlx — reached here
//! through the `flux_pg::sqlx` re-export, never declared directly).

use std::sync::Arc;

use serde_json::Value;

use flux_core::{Error, Result};
use flux_datasource::{
    BatchGetInput, GetInput, Link, ListInput, Match, Record, RelationInput, SearchInput, Source,
};
use flux_pg::sqlx::{self, postgres::PgRow, Row};
use flux_pg::PgHandle;

use super::text::{matched_fields, snippet};
use super::DatasourceBackend;

/// One shared `ds_records` table; `ns` is part of the PK so many scopes coexist in one table. The
/// `fts` column is a stored generated `tsvector` (immutable two-arg `to_tsvector`) — upsert never
/// syncs a mirror table, it just writes the row.
const CREATE_TABLE: &str = "\
CREATE TABLE IF NOT EXISTS ds_records (
    ns     TEXT NOT NULL,
    source TEXT NOT NULL,
    entity TEXT NOT NULL,
    id     TEXT NOT NULL,
    title  TEXT NOT NULL DEFAULT '',
    body   TEXT NOT NULL DEFAULT '',
    links  TEXT NOT NULL DEFAULT '[]',
    meta   TEXT NOT NULL DEFAULT 'null',
    fts    tsvector GENERATED ALWAYS AS (to_tsvector('simple', title || ' ' || body)) STORED,
    PRIMARY KEY (ns, source, entity, id)
)";

const CREATE_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_ds_records_fts ON ds_records USING GIN (fts)";

fn map_pg<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(format!("datasource postgres: {e}"))
}

/// A Postgres-backed datasource index, scoped to one namespace bound at construction.
pub struct PostgresBackend {
    handle: Arc<PgHandle>,
    ns: String,
}

impl PostgresBackend {
    /// Bind `namespace` for the life of this backend (the equivalent of one SQLite file per scope)
    /// and ensure the shared `ds_records` table + GIN index exist. The table is created once and
    /// shared across every namespace; `new` is idempotent.
    pub fn new(handle: Arc<PgHandle>, namespace: impl Into<String>) -> Result<Self> {
        let ns = namespace.into();
        let pool = handle.pool().clone();
        handle.block_on(async move {
            sqlx::query(CREATE_TABLE)
                .execute(&pool)
                .await
                .map_err(map_pg)?;
            sqlx::query(CREATE_INDEX)
                .execute(&pool)
                .await
                .map_err(map_pg)?;
            Ok::<(), Error>(())
        })?;
        Ok(Self { handle, ns })
    }

    /// Enumerate the distinct namespaces whose key starts with `prefix` — the analog of scanning a
    /// directory of per-scope SQLite files. Only namespaces that hold at least one record appear.
    pub fn namespaces(handle: &Arc<PgHandle>, prefix: &str) -> Result<Vec<String>> {
        let pool = handle.pool().clone();
        let prefix = prefix.to_string();
        handle.block_on(async move {
            let rows: Vec<String> = sqlx::query_scalar(
                "SELECT DISTINCT ns FROM ds_records WHERE ns LIKE $1 || '%' ORDER BY ns",
            )
            .bind(prefix.as_str())
            .fetch_all(&pool)
            .await
            .map_err(map_pg)?;
            Ok(rows)
        })
    }
}

/// Rebuild a [`Record`] from a `ds_records` row. `source` round-trips intact: the `plugin/instance`
/// split happens here (never a stored prefix), so consumer-visible [`Source`] keys are untouched.
fn row_to_record(row: PgRow) -> Record {
    let source_key: String = row.get("source");
    let entity: String = row.get("entity");
    let id: String = row.get("id");
    let title: String = row.get("title");
    let body: String = row.get("body");
    let links_json: String = row.get("links");
    let meta_json: String = row.get("meta");
    let (plugin, instance) = match source_key.split_once('/') {
        Some((p, i)) => (p.to_string(), Some(i.to_string())),
        None => (source_key, None),
    };
    let links: Vec<Link> = serde_json::from_str(&links_json).unwrap_or_default();
    let meta: Value = serde_json::from_str(&meta_json).unwrap_or(Value::Null);
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

/// Quote each whitespace term as a phrase and OR them — identical construction to the SQLite backend,
/// so both backends recall the same records. `None` for a blank query. Fed to
/// `websearch_to_tsquery('simple', …)`, where `OR` is the disjunction operator and `"…"` a phrase.
fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

/// Fetch one full record by `(ns, source, entity, id)`.
async fn fetch_one(
    pool: &sqlx::PgPool,
    ns: &str,
    source: &str,
    entity: &str,
    id: &str,
) -> Result<Option<Record>> {
    let row = sqlx::query(
        "SELECT source, entity, id, title, body, links, meta FROM ds_records \
         WHERE ns = $1 AND source = $2 AND entity = $3 AND id = $4",
    )
    .bind(ns)
    .bind(source)
    .bind(entity)
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_pg)?;
    Ok(row.map(row_to_record))
}

impl DatasourceBackend for PostgresBackend {
    fn upsert(&self, records: &[Record]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        // Serialize + own every row up front so the spawned future is `'static`.
        let mut rows = Vec::with_capacity(records.len());
        for r in records {
            let links = serde_json::to_string(&r.links).map_err(map_pg)?;
            let meta = serde_json::to_string(&r.meta).map_err(map_pg)?;
            rows.push((
                r.source.key(),
                r.entity.clone(),
                r.id.clone(),
                r.title.clone(),
                r.body.clone(),
                links,
                meta,
            ));
        }
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        self.handle.block_on(async move {
            let mut tx = pool.begin().await.map_err(map_pg)?;
            for (source, entity, id, title, body, links, meta) in &rows {
                // The generated `fts` column stays in sync automatically — no mirror-table dance.
                sqlx::query(
                    "INSERT INTO ds_records (ns, source, entity, id, title, body, links, meta) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
                     ON CONFLICT (ns, source, entity, id) DO UPDATE SET \
                       title = EXCLUDED.title, body = EXCLUDED.body, \
                       links = EXCLUDED.links, meta  = EXCLUDED.meta",
                )
                .bind(ns.as_str())
                .bind(source.as_str())
                .bind(entity.as_str())
                .bind(id.as_str())
                .bind(title.as_str())
                .bind(body.as_str())
                .bind(links.as_str())
                .bind(meta.as_str())
                .execute(&mut *tx)
                .await
                .map_err(map_pg)?;
            }
            tx.commit().await.map_err(map_pg)?;
            Ok(())
        })
    }

    fn search(&self, input: &SearchInput) -> Result<Vec<Match>> {
        let Some(match_expr) = fts_query(&input.query) else {
            return Ok(Vec::new());
        };
        let limit = input.limit.unwrap_or(5) as i64;
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        let source = input.source.clone();
        let entity = input.entity.clone();
        let query = input.query.clone();

        // Positional placeholders, bound in this exact order: $1 match_expr, $2 ns,
        // [source], [entity], then limit.
        let mut sql = String::from(
            "SELECT r.source, r.entity, r.id, ts_rank(r.fts, q)::float8 AS score \
             FROM ds_records r, websearch_to_tsquery('simple', $1) q \
             WHERE r.ns = $2 AND r.fts @@ q",
        );
        let mut idx = 3;
        if source.is_some() {
            sql.push_str(&format!(" AND r.source = ${idx}"));
            idx += 1;
        }
        if entity.is_some() {
            sql.push_str(&format!(" AND r.entity = ${idx}"));
            idx += 1;
        }
        sql.push_str(&format!(" ORDER BY score DESC, r.id ASC LIMIT ${idx}"));

        self.handle.block_on(async move {
            let mut q = sqlx::query(&sql)
                .bind(match_expr.as_str())
                .bind(ns.as_str());
            if let Some(s) = &source {
                q = q.bind(s.as_str());
            }
            if let Some(e) = &entity {
                q = q.bind(e.as_str());
            }
            q = q.bind(limit);
            let hits = q.fetch_all(&pool).await.map_err(map_pg)?;

            let mut out = Vec::with_capacity(hits.len());
            for row in hits {
                let hit_source: String = row.get("source");
                let hit_entity: String = row.get("entity");
                let hit_id: String = row.get("id");
                let score: f64 = row.get("score");
                // Re-fetch the full record and shape it exactly like the SQLite backend: the stored
                // body stays intact; the returned copy carries the snippet.
                if let Some(mut rec) =
                    fetch_one(&pool, &ns, &hit_source, &hit_entity, &hit_id).await?
                {
                    let matched = matched_fields(&rec, &query);
                    rec.body = snippet(&rec.body, &query);
                    out.push(Match {
                        record: rec,
                        score,
                        matched_fields: matched,
                    });
                }
            }
            Ok(out)
        })
    }

    fn get(&self, input: &GetInput) -> Result<Option<Record>> {
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        let source = input.source.clone();
        let entity = input.entity.clone();
        let id = input.id.clone();
        self.handle
            .block_on(async move { fetch_one(&pool, &ns, &source, &entity, &id).await })
    }

    fn list(&self, input: &ListInput) -> Result<Vec<Record>> {
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        let source = input.source.clone();
        let entity = input.entity.clone();
        let offset = input.offset.unwrap_or(0) as i64;
        let limit = input.limit.map(|n| n as i64);

        // $1 ns, $2 source, [entity], [limit], offset — LIMIT is omitted entirely when `None`
        // (the analog of SQLite's `LIMIT -1` no-limit idiom).
        let mut sql = String::from(
            "SELECT source, entity, id, title, body, links, meta FROM ds_records \
             WHERE ns = $1 AND source = $2",
        );
        let mut idx = 3;
        if entity.is_some() {
            sql.push_str(&format!(" AND entity = ${idx}"));
            idx += 1;
        }
        sql.push_str(" ORDER BY entity, id");
        if limit.is_some() {
            sql.push_str(&format!(" LIMIT ${idx}"));
            idx += 1;
        }
        sql.push_str(&format!(" OFFSET ${idx}"));

        self.handle.block_on(async move {
            let mut q = sqlx::query(&sql).bind(ns.as_str()).bind(source.as_str());
            if let Some(e) = &entity {
                q = q.bind(e.as_str());
            }
            if let Some(l) = limit {
                q = q.bind(l);
            }
            q = q.bind(offset);
            let rows = q.fetch_all(&pool).await.map_err(map_pg)?;
            Ok(rows.into_iter().map(row_to_record).collect())
        })
    }

    fn relation(&self, input: &RelationInput) -> Result<Vec<Record>> {
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        let source = input.source.clone();
        let entity = input.entity.clone();
        let id = input.id.clone();
        let rel = input.rel.clone();
        self.handle.block_on(async move {
            let Some(origin) = fetch_one(&pool, &ns, &source, &entity, &id).await? else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for link in &origin.links {
                if rel.as_deref().is_some_and(|r| link.rel != r) {
                    continue;
                }
                if let Some(rec) =
                    fetch_one(&pool, &ns, &source, &link.target_entity, &link.target_id).await?
                {
                    out.push(rec);
                }
            }
            Ok(out)
        })
    }

    fn batch_get(&self, input: &BatchGetInput) -> Result<Vec<Record>> {
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        let source = input.source.clone();
        let entity = input.entity.clone();
        let ids = input.ids.clone();
        self.handle.block_on(async move {
            let mut out = Vec::new();
            for id in &ids {
                if let Some(rec) = fetch_one(&pool, &ns, &source, &entity, id).await? {
                    out.push(rec);
                }
            }
            Ok(out)
        })
    }

    fn clear(&self) -> Result<()> {
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        self.handle.block_on(async move {
            sqlx::query("DELETE FROM ds_records WHERE ns = $1")
                .bind(ns.as_str())
                .execute(&pool)
                .await
                .map_err(map_pg)?;
            Ok(())
        })
    }

    fn delete_source(&self, source: &str) -> Result<usize> {
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        let source = source.to_string();
        self.handle.block_on(async move {
            let done = sqlx::query("DELETE FROM ds_records WHERE ns = $1 AND source = $2")
                .bind(ns.as_str())
                .bind(source.as_str())
                .execute(&pool)
                .await
                .map_err(map_pg)?;
            Ok(done.rows_affected() as usize)
        })
    }

    fn delete(&self, source: &str, entity: &str, ids: &[String]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        let source = source.to_string();
        let entity = entity.to_string();
        let ids: Vec<String> = ids.to_vec();
        self.handle.block_on(async move {
            let mut tx = pool.begin().await.map_err(map_pg)?;
            let mut removed = 0u64;
            for id in &ids {
                let done = sqlx::query(
                    "DELETE FROM ds_records WHERE ns = $1 AND source = $2 AND entity = $3 AND id = $4",
                )
                .bind(ns.as_str())
                .bind(source.as_str())
                .bind(entity.as_str())
                .bind(id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(map_pg)?;
                removed += done.rows_affected();
            }
            tx.commit().await.map_err(map_pg)?;
            Ok(removed as usize)
        })
    }

    fn len(&self) -> usize {
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        self.handle
            .block_on(async move {
                let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ds_records WHERE ns = $1")
                    .bind(ns.as_str())
                    .fetch_one(&pool)
                    .await
                    .map_err(map_pg)?;
                Ok::<usize, Error>(n as usize)
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, title: &str, body: &str) -> Record {
        Record::new(Source::new("local"), "file.document", id, title, body)
    }

    /// A throwaway `(url, schema)` isolating one test in its own `?schema=t_<ulid>`, or `None` when
    /// `TEST_POSTGRES_URL` is unset (tests then skip rather than fail).
    fn test_env() -> Option<(String, String)> {
        let base = std::env::var("TEST_POSTGRES_URL").ok()?;
        let schema = format!("t_{}", ulid::Ulid::new().to_string().to_lowercase());
        let sep = if base.contains('?') { '&' } else { '?' };
        Some((format!("{base}{sep}schema={schema}"), schema))
    }

    fn drop_schema(handle: &Arc<PgHandle>, schema: &str) {
        let pool = handle.pool().clone();
        let sql = format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE");
        handle.block_on(async move {
            let _ = sqlx::query(&sql).execute(&pool).await;
        });
    }

    #[test]
    fn pg_search_get_and_persistence() {
        let Some((url, schema)) = test_env() else {
            eprintln!("skipping pg_search_get_and_persistence: TEST_POSTGRES_URL unset");
            return;
        };
        let h1 = PgHandle::connect(&url).unwrap();
        let b = PostgresBackend::new(h1.clone(), "kb").unwrap();
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
        assert!(
            hits[0].matched_fields.contains(&"title".to_string()),
            "matched_fields attributes the title: {:?}",
            hits[0].matched_fields
        );

        // Reconnect a brand-new handle to the same schema + ns: the rows persist (durability).
        let h2 = PgHandle::connect(&url).unwrap();
        let b2 = PostgresBackend::new(h2.clone(), "kb").unwrap();
        assert_eq!(b2.len(), 2);
        let got = b2
            .get(&GetInput {
                source: "local".into(),
                entity: "file.document".into(),
                id: "warm-transfer".into(),
            })
            .unwrap()
            .unwrap();
        assert_eq!(got.title, "Warm transfer");
        // `get` returns the intact stored body, not a search snippet.
        assert!(got.body.contains("connects the caller"));

        drop_schema(&h2, &schema);
    }

    #[test]
    fn pg_upsert_replaces_and_fts_stays_in_sync() {
        let Some((url, schema)) = test_env() else {
            eprintln!("skipping pg_upsert_replaces_and_fts_stays_in_sync: TEST_POSTGRES_URL unset");
            return;
        };
        let h = PgHandle::connect(&url).unwrap();
        let b = PostgresBackend::new(h.clone(), "kb").unwrap();
        b.upsert(&[doc("x", "alpha", "first body")]).unwrap();
        b.upsert(&[doc("x", "beta", "second body")]).unwrap();
        assert_eq!(b.len(), 1);
        // The generated tsvector followed the update: the old term is gone, the new one matches.
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
        drop_schema(&h, &schema);
    }

    #[test]
    fn pg_delete_source_and_by_id_remove_records_and_persist() {
        let Some((url, schema)) = test_env() else {
            eprintln!(
                "skipping pg_delete_source_and_by_id_remove_records_and_persist: TEST_POSTGRES_URL \
                 unset"
            );
            return;
        };
        let h = PgHandle::connect(&url).unwrap();
        let b = PostgresBackend::new(h.clone(), "kb").unwrap();
        b.upsert(&[
            Record::new(Source::new("kb-a"), "doc", "1", "alpha one", "body a1"),
            Record::new(Source::new("kb-a"), "doc", "2", "alpha two", "body a2"),
            Record::new(Source::new("kb-b"), "doc", "1", "beta", "body b1"),
        ])
        .unwrap();
        assert_eq!(b.len(), 3);

        // delete-by-id drops just that record (FTS follows, since it is the same row).
        assert_eq!(b.delete("kb-a", "doc", &["1".into()]).unwrap(), 1);
        assert_eq!(b.len(), 2);
        assert!(b
            .get(&GetInput {
                source: "kb-a".into(),
                entity: "doc".into(),
                id: "1".into(),
            })
            .unwrap()
            .is_none());
        assert!(
            b.search(&SearchInput {
                query: "one".into(), // a term unique to the deleted record
                ..Default::default()
            })
            .unwrap()
            .is_empty(),
            "deleted record is gone from FTS too"
        );

        // delete_source drops the rest of kb-a but leaves kb-b.
        assert_eq!(b.delete_source("kb-a").unwrap(), 1);
        assert_eq!(b.len(), 1);

        // Reconnect: the deletions persisted; only kb-b survives.
        let h2 = PgHandle::connect(&url).unwrap();
        let b2 = PostgresBackend::new(h2.clone(), "kb").unwrap();
        assert_eq!(b2.len(), 1);
        let only = b2
            .list(&ListInput {
                source: "kb-b".into(),
                entity: None,
                offset: None,
                limit: None,
            })
            .unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].id, "1");
        drop_schema(&h2, &schema);
    }

    #[test]
    fn pg_namespaces_are_isolated() {
        let Some((url, schema)) = test_env() else {
            eprintln!("skipping pg_namespaces_are_isolated: TEST_POSTGRES_URL unset");
            return;
        };
        let h = PgHandle::connect(&url).unwrap();
        // Two backends on ONE pool, different namespaces — the exact analog of two SQLite files.
        let a = PostgresBackend::new(h.clone(), "kb-a").unwrap();
        let b = PostgresBackend::new(h.clone(), "kb-b").unwrap();

        a.upsert(&[doc(
            "only-a",
            "Alpha doc",
            "content that lives under namespace a",
        )])
        .unwrap();

        // `b` (a different ns on the same table) sees nothing of `a`'s record, via every path.
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 0);
        assert!(b
            .get(&GetInput {
                source: "local".into(),
                entity: "file.document".into(),
                id: "only-a".into(),
            })
            .unwrap()
            .is_none());
        assert!(b
            .list(&ListInput {
                source: "local".into(),
                ..Default::default()
            })
            .unwrap()
            .is_empty());
        assert!(b
            .search(&SearchInput {
                query: "content".into(),
                ..Default::default()
            })
            .unwrap()
            .is_empty());
        // `a` sees its own record through get + search.
        assert!(a
            .get(&GetInput {
                source: "local".into(),
                entity: "file.document".into(),
                id: "only-a".into(),
            })
            .unwrap()
            .is_some());
        assert!(!a
            .search(&SearchInput {
                query: "content".into(),
                ..Default::default()
            })
            .unwrap()
            .is_empty());

        // `namespaces()` lists only namespaces that actually hold a record.
        let names = PostgresBackend::namespaces(&h, "kb-").unwrap();
        assert!(names.contains(&"kb-a".to_string()), "namespaces: {names:?}");
        assert!(
            !names.contains(&"kb-b".to_string()),
            "empty namespace is not listed: {names:?}"
        );

        drop_schema(&h, &schema);
    }
}
