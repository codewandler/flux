//! Ingesters that turn source material into datasource [`Record`]s, plus reindex/freshness helpers.
//!
//! Ingesters take already-read content (the file walk lives in the caller, over `flux-system`) and
//! upsert typed records: markdown/text → `file.document`, an OpenAPI spec → `openapi.operation` +
//! `openapi.schema`. [`reindex`] clears the index (the caller re-ingests); [`freshness`] reports the
//! record count.

use serde_json::{json, Value};

use flux_core::Result;
use flux_datasource::{Record, Source};

use super::DatasourceBackend;

/// Ingest `(path, text)` markdown/text docs as `file.document` records under `source` (id = path).
pub fn ingest_markdown(
    backend: &dyn DatasourceBackend,
    source: &str,
    docs: &[(String, String)],
) -> Result<usize> {
    let recs: Vec<Record> = docs
        .iter()
        .map(|(path, text)| {
            let mut r = Record::new(
                Source::new(source),
                "file.document",
                path.clone(),
                path.clone(),
                text.clone(),
            );
            r.meta = json!({ "path": path });
            r
        })
        .collect();
    backend.upsert(&recs)?;
    Ok(recs.len())
}

const HTTP_METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];

/// Ingest an OpenAPI JSON spec: every operation → an `openapi.operation` record (id = `operationId` or
/// `METHOD /path`; title = summary; body = method/path + summary + description + parameter names), and
/// every `components/schemas` entry → an `openapi.schema` record (body = description + property names).
pub fn ingest_openapi(
    backend: &dyn DatasourceBackend,
    source: &str,
    spec: &Value,
) -> Result<usize> {
    let src = Source::new(source);
    let mut recs = Vec::new();

    if let Some(paths) = spec.get("paths").and_then(|v| v.as_object()) {
        for (path, item) in paths {
            let Some(methods) = item.as_object() else {
                continue;
            };
            for (method, op) in methods {
                if !HTTP_METHODS.contains(&method.to_lowercase().as_str()) {
                    continue;
                }
                let op_id = op
                    .get("operationId")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| format!("{} {}", method.to_uppercase(), path));
                let summary = op.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                let desc = op.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let mut body = format!("{} {}", method.to_uppercase(), path);
                if !summary.is_empty() {
                    body.push('\n');
                    body.push_str(summary);
                }
                if !desc.is_empty() {
                    body.push('\n');
                    body.push_str(desc);
                }
                if let Some(params) = op.get("parameters").and_then(|v| v.as_array()) {
                    for p in params {
                        if let Some(name) = p.get("name").and_then(|v| v.as_str()) {
                            body.push_str("\nparam: ");
                            body.push_str(name);
                        }
                    }
                }
                let title = if summary.is_empty() {
                    op_id.clone()
                } else {
                    summary.to_string()
                };
                recs.push(Record::new(
                    src.clone(),
                    "openapi.operation",
                    op_id,
                    title,
                    body,
                ));
            }
        }
    }

    if let Some(schemas) = spec
        .pointer("/components/schemas")
        .and_then(|v| v.as_object())
    {
        for (name, schema) in schemas {
            let desc = schema
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let props = schema
                .get("properties")
                .and_then(|v| v.as_object())
                .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            let mut body = desc.to_string();
            if !props.is_empty() {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str("fields: ");
                body.push_str(&props);
            }
            recs.push(Record::new(
                src.clone(),
                "openapi.schema",
                name.clone(),
                name.clone(),
                body,
            ));
        }
    }

    backend.upsert(&recs)?;
    Ok(recs.len())
}

/// Reindex: drop everything (the rebuild half — the caller then re-ingests fresh content).
pub fn reindex(backend: &dyn DatasourceBackend) -> Result<()> {
    backend.clear()
}

/// Freshness: the number of indexed records (a non-zero count means the index is populated). A finer
/// staleness check compares a record's `meta.updated_at` against the source — deferred to the caller.
pub fn freshness(backend: &dyn DatasourceBackend) -> usize {
    backend.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datasource::MemoryBackend;
    use flux_datasource::{GetInput, SearchInput};

    #[test]
    fn ingest_markdown_indexes_docs_and_search_hits() {
        let b = MemoryBackend::new();
        let docs = vec![
            (
                "kb/warm-transfer.md".to_string(),
                "A warm transfer connects the caller to an agent after an announcement."
                    .to_string(),
            ),
            (
                "kb/metrics.md".to_string(),
                "Common metrics include handle time and abandonment rate.".to_string(),
            ),
        ];
        let n = ingest_markdown(&b, "local", &docs).unwrap();
        assert_eq!(n, 2);
        let hits = b
            .search(&SearchInput {
                query: "warm transfer".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits[0].record.id, "kb/warm-transfer.md");
    }

    #[test]
    fn ingest_openapi_creates_operation_and_schema_records() {
        let spec = json!({
            "openapi": "3.0.0",
            "paths": {
                "/calls/{id}": {
                    "get": {
                        "operationId": "getCall",
                        "summary": "Fetch a call",
                        "description": "Returns one call by id.",
                        "parameters": [{ "name": "id", "in": "path" }]
                    }
                }
            },
            "components": {
                "schemas": {
                    "Call": {
                        "description": "A phone call.",
                        "properties": { "id": {}, "duration": {} }
                    }
                }
            }
        });
        let b = MemoryBackend::new();
        let n = ingest_openapi(&b, "manager-api", &spec).unwrap();
        assert_eq!(n, 2); // one operation + one schema

        let op = b
            .get(&GetInput {
                source: "manager-api".into(),
                entity: "openapi.operation".into(),
                id: "getCall".into(),
            })
            .unwrap()
            .unwrap();
        assert_eq!(op.title, "Fetch a call");
        assert!(op.body.contains("GET /calls/{id}"));
        assert!(op.body.contains("param: id"));

        let schema = b
            .get(&GetInput {
                source: "manager-api".into(),
                entity: "openapi.schema".into(),
                id: "Call".into(),
            })
            .unwrap()
            .unwrap();
        // property order follows serde_json's Map ordering; assert both are listed, not the order.
        assert!(schema.body.contains("fields:"));
        assert!(schema.body.contains("id") && schema.body.contains("duration"));
    }

    #[test]
    fn reindex_clears_then_freshness_reports_count() {
        let b = MemoryBackend::new();
        ingest_markdown(&b, "local", &[("a.md".into(), "hello".into())]).unwrap();
        assert_eq!(freshness(&b), 1);
        reindex(&b).unwrap();
        assert_eq!(freshness(&b), 0);
    }
}
