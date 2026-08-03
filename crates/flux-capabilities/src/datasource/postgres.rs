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
    SourceSummary,
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

/// `true` when a query failed because `ds_records` does not exist (Postgres SQLSTATE `42P01`,
/// `undefined_table`). The shared tolerance check for the cross-scope reads ([`namespaces`]
/// (PostgresBackend::namespaces), [`scan`](PostgresBackend::scan)): an enumeration over a database
/// where [`PostgresBackend::ensure_schema`] never ran means "no records yet" — the same empty
/// answer a scan over zero per-scope SQLite files gives — not an error.
fn is_undefined_table(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("42P01"))
}

/// A Postgres-backed datasource index, scoped to one namespace bound at construction.
pub struct PostgresBackend {
    handle: Arc<PgHandle>,
    ns: String,
}

impl PostgresBackend {
    /// Bind `namespace` for the life of this backend (the equivalent of one SQLite file per scope).
    /// I/O-free and infallible: construction runs **no** DDL — call [`Self::ensure_schema`] once
    /// from wherever the deployment opens its stores, before the first backend touches the table.
    pub fn new(handle: Arc<PgHandle>, namespace: impl Into<String>) -> Self {
        Self {
            handle,
            ns: namespace.into(),
        }
    }

    /// Create the shared `ds_records` table + GIN index. Idempotent — call it **once** from
    /// wherever a deployment opens its stores; per-scope construction ([`Self::new`]) then stays
    /// free of I/O.
    ///
    /// The DDL runs inside one transaction whose **first** statement is the global flux DDL
    /// advisory lock ([`flux_pg::ddl_lock`]): Postgres `IF NOT EXISTS` DDL is not atomic, so
    /// concurrent first-boots would otherwise race the catalog insert and the loser errors.
    pub fn ensure_schema(handle: &Arc<PgHandle>) -> Result<()> {
        let pool = handle.pool().clone();
        handle.block_on(async move {
            let mut tx = pool.begin().await.map_err(map_pg)?;
            flux_pg::ddl_lock(&mut tx).await?;
            sqlx::query(CREATE_TABLE)
                .execute(&mut *tx)
                .await
                .map_err(map_pg)?;
            sqlx::query(CREATE_INDEX)
                .execute(&mut *tx)
                .await
                .map_err(map_pg)?;
            tx.commit().await.map_err(map_pg)?;
            Ok(())
        })
    }

    /// Enumerate the distinct namespaces whose key starts with `prefix` — the analog of scanning a
    /// directory of per-scope SQLite files. Only namespaces that hold at least one record appear.
    /// A database where [`Self::ensure_schema`] never ran yields `Ok(vec![])`, matching that
    /// zero-files scan.
    pub fn namespaces(handle: &Arc<PgHandle>, prefix: &str) -> Result<Vec<String>> {
        let pool = handle.pool().clone();
        let prefix = prefix.to_string();
        handle.block_on(async move {
            let rows = match sqlx::query_scalar::<_, String>(
                "SELECT DISTINCT ns FROM ds_records WHERE ns LIKE $1 || '%' ORDER BY ns",
            )
            .bind(prefix.as_str())
            .fetch_all(&pool)
            .await
            {
                Ok(rows) => rows,
                Err(e) if is_undefined_table(&e) => Vec::new(),
                Err(e) => return Err(map_pg(e)),
            };
            Ok(rows)
        })
    }

    /// Cross-namespace entity scan: every record of `entity` in every namespace starting with
    /// `ns_prefix`, as `(namespace, record)` pairs ordered by `(ns, id)`. One query replaces the
    /// 1+N per-scope round-trip loop ([`Self::namespaces`] + a per-scope backend + `list` each)
    /// that a global lookup over per-scope namespaces otherwise costs.
    ///
    /// Prefix matching mirrors [`Self::namespaces`]; records are built by the same row mapping as
    /// `list` (`source` round-trips intact) and `ns` is returned verbatim from its own column.
    /// Deliberately an associated fn, not a [`DatasourceBackend`] method — the trait stays
    /// per-scope by design. No limit: callers filter. A database where [`Self::ensure_schema`]
    /// never ran yields `Ok(vec![])`, like [`Self::namespaces`].
    pub fn scan(
        handle: &Arc<PgHandle>,
        ns_prefix: &str,
        entity: &str,
    ) -> Result<Vec<(String, Record)>> {
        let pool = handle.pool().clone();
        let prefix = ns_prefix.to_string();
        let entity = entity.to_string();
        handle.block_on(async move {
            let rows = match sqlx::query(
                "SELECT ns, source, entity, id, title, body, links, meta FROM ds_records \
                 WHERE ns LIKE $1 || '%' AND entity = $2 ORDER BY ns, id",
            )
            .bind(prefix.as_str())
            .bind(entity.as_str())
            .fetch_all(&pool)
            .await
            {
                Ok(rows) => rows,
                Err(e) if is_undefined_table(&e) => Vec::new(),
                Err(e) => return Err(map_pg(e)),
            };
            Ok(rows
                .into_iter()
                .map(|row| {
                    let ns: String = row.get("ns");
                    (ns, row_to_record(row))
                })
                .collect())
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
            // The dynamic parts are only `${idx}` placeholder numbers; every value rides in a
            // bind below.
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
            // The dynamic parts are only `${idx}` placeholder numbers; every value rides in a
            // bind below.
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

    fn sources(&self) -> Result<Vec<SourceSummary>> {
        let pool = self.handle.pool().clone();
        let ns = self.ns.clone();
        self.handle.block_on(async move {
            let rows = sqlx::query(
                "SELECT source, entity, COUNT(*) AS cnt FROM ds_records WHERE ns = $1 \
                 GROUP BY source, entity ORDER BY source, entity",
            )
            .bind(ns.as_str())
            .fetch_all(&pool)
            .await
            .map_err(map_pg)?;
            // Rows arrive ordered by (source, entity), so a source's rows are consecutive: fold
            // each into the running summary, starting a new one whenever the source key changes.
            let mut out: Vec<SourceSummary> = Vec::new();
            for row in rows {
                let source: String = row.get("source");
                let entity: String = row.get("entity");
                let count: i64 = row.get("cnt");
                match out.last_mut() {
                    Some(last) if last.source == source => {
                        last.entities.push(entity);
                        last.count += count as usize;
                    }
                    _ => out.push(SourceSummary {
                        source,
                        entities: vec![entity],
                        count: count as usize,
                    }),
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
        let schema = format!("t_{}", ulid::Ulid::generate().to_string().to_lowercase());
        let sep = if base.contains('?') { '&' } else { '?' };
        Some((format!("{base}{sep}schema={schema}"), schema))
    }

    fn drop_schema(handle: &Arc<PgHandle>, schema: &str) {
        let pool = handle.pool().clone();
        let sql = format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE");
        handle.block_on(async move {
            // The schema identifier is minted from a ULID by `test_env`, not user input.
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
        PostgresBackend::ensure_schema(&h1).unwrap();
        let b = PostgresBackend::new(h1.clone(), "kb");
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
        let b2 = PostgresBackend::new(h2.clone(), "kb");
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
        PostgresBackend::ensure_schema(&h).unwrap();
        let b = PostgresBackend::new(h.clone(), "kb");
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
        PostgresBackend::ensure_schema(&h).unwrap();
        let b = PostgresBackend::new(h.clone(), "kb");
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
        let b2 = PostgresBackend::new(h2.clone(), "kb");
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
    fn pg_sources_reports_distinct_sources_entities_and_counts() {
        let Some((url, schema)) = test_env() else {
            eprintln!(
                "skipping pg_sources_reports_distinct_sources_entities_and_counts: \
                 TEST_POSTGRES_URL unset"
            );
            return;
        };
        let h = PgHandle::connect(&url).unwrap();
        PostgresBackend::ensure_schema(&h).unwrap();
        let b = PostgresBackend::new(h.clone(), "kb");
        b.upsert(&[
            doc("a", "Agent loop", "streams tokens"),
            doc("b", "Permissions", "gate every call"),
            Record::new(
                Source::new("gitlab"),
                "gitlab.merge_request",
                "1",
                "MR",
                "body",
            ),
            Record::new(Source::new("gitlab"), "gitlab.issue", "1", "Issue", "body"),
        ])
        .unwrap();

        let sources = b.sources().unwrap();
        assert_eq!(sources.len(), 2, "sources: {sources:?}");
        assert_eq!(sources[0].source, "gitlab");
        assert_eq!(sources[0].count, 2);
        assert_eq!(
            sources[0].entities,
            vec!["gitlab.issue", "gitlab.merge_request"]
        );
        assert_eq!(sources[1].source, "local");
        assert_eq!(sources[1].count, 2);
        assert_eq!(sources[1].entities, vec!["file.document"]);

        // A different namespace on the same table sees no sources (structural isolation).
        let other = PostgresBackend::new(h.clone(), "other-kb");
        assert!(other.sources().unwrap().is_empty());

        drop_schema(&h, &schema);
    }

    #[test]
    fn pg_namespaces_are_isolated() {
        let Some((url, schema)) = test_env() else {
            eprintln!("skipping pg_namespaces_are_isolated: TEST_POSTGRES_URL unset");
            return;
        };
        let h = PgHandle::connect(&url).unwrap();
        PostgresBackend::ensure_schema(&h).unwrap();
        // Two backends on ONE pool, different namespaces — the exact analog of two SQLite files.
        let a = PostgresBackend::new(h.clone(), "kb-a");
        let b = PostgresBackend::new(h.clone(), "kb-b");

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

    #[test]
    fn pg_namespaces_tolerates_missing_table() {
        let Some((url, schema)) = test_env() else {
            eprintln!("skipping pg_namespaces_tolerates_missing_table: TEST_POSTGRES_URL unset");
            return;
        };
        let h = PgHandle::connect(&url).unwrap();
        // `ensure_schema` never ran in this throwaway schema: enumerating namespaces over "no
        // records yet" is Ok and empty, exactly like scanning zero per-scope SQLite files.
        let names = PostgresBackend::namespaces(&h, "").unwrap();
        assert!(names.is_empty(), "fresh schema lists nothing: {names:?}");
        drop_schema(&h, &schema);
    }

    #[test]
    fn pg_concurrent_ensure_schema_bootstrap() {
        let Some((url, schema)) = test_env() else {
            eprintln!("skipping pg_concurrent_ensure_schema_bootstrap: TEST_POSTGRES_URL unset");
            return;
        };
        // Regression tripwire for the non-atomic IF-NOT-EXISTS race (D-76): N first-boots against
        // ONE fresh schema, barrier-aligned so the DDL transactions overlap. Without the advisory
        // lock the losers can error with a duplicate-key catalog violation; the red state is
        // probabilistic, so this is a tripwire, not a reliable reproducer.
        const N: usize = 8;
        let handles: Vec<_> = (0..N).map(|_| PgHandle::connect(&url).unwrap()).collect();
        let barrier = Arc::new(std::sync::Barrier::new(N));
        let results: Vec<Result<()>> = handles
            .into_iter()
            .map(|h| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    PostgresBackend::ensure_schema(&h)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().expect("bootstrap thread panicked"))
            .collect();

        // Clean up before asserting so a failure never leaks the throwaway schema.
        let h = PgHandle::connect(&url).unwrap();
        drop_schema(&h, &schema);
        for (i, r) in results.into_iter().enumerate() {
            r.unwrap_or_else(|e| panic!("concurrent bootstrap {i} failed: {e}"));
        }
    }

    #[test]
    fn pg_scan_across_namespaces() {
        let Some((url, schema)) = test_env() else {
            eprintln!("skipping pg_scan_across_namespaces: TEST_POSTGRES_URL unset");
            return;
        };
        let h = PgHandle::connect(&url).unwrap();
        PostgresBackend::ensure_schema(&h).unwrap();

        // Three namespaces under two prefixes; each holds one `head` record plus other-entity
        // noise. The head sources include a `plugin/instance` split to prove source round-trip.
        for ns in ["a:1", "a:2", "b:1"] {
            let b = PostgresBackend::new(h.clone(), ns);
            b.upsert(&[
                Record::new(
                    Source::with_instance("registry", "main"),
                    "head",
                    format!("head-{ns}"),
                    format!("Head of {ns}"),
                    format!("head body for {ns}"),
                ),
                Record::new(
                    Source::new("registry"),
                    "noise",
                    "n1",
                    "Noise",
                    "not a head",
                ),
            ])
            .unwrap();
        }

        let hits = PostgresBackend::scan(&h, "a:", "head").unwrap();
        assert_eq!(hits.len(), 2, "exactly the a:* heads: {hits:?}");
        // Ordered by (ns, id); ns comes back verbatim, paired with its own record.
        assert_eq!(hits[0].0, "a:1");
        assert_eq!(hits[0].1.id, "head-a:1");
        assert_eq!(hits[1].0, "a:2");
        assert_eq!(hits[1].1.id, "head-a:2");
        assert!(
            hits.iter().all(|(_, r)| r.entity == "head"),
            "noise entities excluded: {hits:?}"
        );

        // Record shape is identical to `list` on the scoped backend (source round-trips intact).
        let listed = PostgresBackend::new(h.clone(), "a:1")
            .list(&ListInput {
                source: "registry/main".into(),
                entity: Some("head".into()),
                offset: None,
                limit: None,
            })
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(hits[0].1, listed[0], "scan record == list record");
        assert_eq!(hits[0].1.source.plugin, "registry");
        assert_eq!(hits[0].1.source.instance.as_deref(), Some("main"));

        // A prefix that matches no namespace is empty, not an error.
        assert!(PostgresBackend::scan(&h, "z:", "head").unwrap().is_empty());

        drop_schema(&h, &schema);
    }

    #[test]
    fn pg_scan_tolerates_missing_table() {
        let Some((url, schema)) = test_env() else {
            eprintln!("skipping pg_scan_tolerates_missing_table: TEST_POSTGRES_URL unset");
            return;
        };
        let h = PgHandle::connect(&url).unwrap();
        // `ensure_schema` never ran: same 42P01 tolerance as `namespaces()`.
        let hits = PostgresBackend::scan(&h, "a:", "head").unwrap();
        assert!(hits.is_empty(), "fresh schema scans empty: {hits:?}");
        drop_schema(&h, &schema);
    }
}
