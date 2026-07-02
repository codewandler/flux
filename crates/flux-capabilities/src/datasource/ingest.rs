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

/// Options for the text chunker (story D-50). `max_chars` bounds a chunk's length (in characters);
/// `overlap` characters of the previous chunk are prepended to each subsequent chunk so a fact spanning a
/// boundary stays retrievable. Defaults are tuned for keyword + embedding retrieval.
#[derive(Debug, Clone, Copy)]
pub struct ChunkOptions {
    /// Maximum characters per chunk.
    pub max_chars: usize,
    /// Characters of the prior chunk to prepend to each subsequent chunk.
    pub overlap: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        ChunkOptions {
            max_chars: 1500,
            overlap: 150,
        }
    }
}

/// The largest char-boundary prefix of `s` no longer than `max` **characters** — split `s` into
/// consecutive fixed-size char windows (the last-resort splitter for an over-long sentence).
fn char_windows(s: &str, max: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars
        .chunks(max.max(1))
        .map(|w| w.iter().collect())
        .collect()
}

/// Naive sentence split: cut after `.`/`!`/`?` when the next char is whitespace. Keeps the punctuation.
fn split_sentences(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?') && chars.peek().map(|n| n.is_whitespace()).unwrap_or(false)
        {
            out.push(cur.trim().to_string());
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out.retain(|x| !x.is_empty());
    out
}

/// Split one over-long paragraph into ≤ `max`-char pieces, preferring sentence boundaries, falling back
/// to hard char windows for a single sentence that itself exceeds `max`.
fn hard_split(para: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for sent in split_sentences(para) {
        let slen = sent.chars().count();
        if slen > max {
            if cur_len > 0 {
                out.push(std::mem::take(&mut cur));
                cur_len = 0;
            }
            out.extend(char_windows(&sent, max));
            continue;
        }
        if cur_len > 0 && cur_len + 1 + slen > max {
            out.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if cur_len > 0 {
            cur.push(' ');
            cur_len += 1;
        }
        cur.push_str(&sent);
        cur_len += slen;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Prepend `overlap` characters of each chunk's *original* predecessor to it (computed from the
/// pre-overlap chunks so overlap does not compound).
fn apply_overlap(chunks: &mut [String], overlap: usize) {
    let originals: Vec<String> = chunks.to_vec();
    for i in 1..chunks.len() {
        let prev: Vec<char> = originals[i - 1].chars().collect();
        let start = prev.len().saturating_sub(overlap);
        let tail: String = prev[start..].iter().collect();
        let tail = tail.trim();
        if !tail.is_empty() {
            chunks[i] = format!("{tail} {}", chunks[i]);
        }
    }
}

/// Split `text` into retrieval-sized chunks (story D-50): pack whole paragraphs up to `max_chars`,
/// hard-split any paragraph that alone exceeds it (by sentence, then by char window), and add
/// `overlap`-char context between consecutive chunks. Short text yields a single chunk; empty text yields
/// none.
pub fn chunk_text(text: &str, opts: &ChunkOptions) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let max = opts.max_chars.max(1);
    if text.chars().count() <= max {
        return vec![text.to_string()];
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        let plen = para.chars().count();
        if plen > max {
            if cur_len > 0 {
                chunks.push(std::mem::take(&mut cur));
                cur_len = 0;
            }
            chunks.extend(hard_split(para, max));
            continue;
        }
        if cur_len > 0 && cur_len + 2 + plen > max {
            chunks.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if cur_len > 0 {
            cur.push_str("\n\n");
            cur_len += 2;
        }
        cur.push_str(para);
        cur_len += plen;
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    if opts.overlap > 0 && chunks.len() > 1 {
        apply_overlap(&mut chunks, opts.overlap.min(max / 2));
    }
    chunks
}

/// Chunk `text` into `file.document` [`Record`]s under `(source, id)`. A single chunk keeps `id` verbatim
/// (stable address); multiple chunks get `id#N` ids and carry `chunk`/`of` in `meta`, merged over
/// `base_meta`. The doc `id` is the title so search results name the source document.
fn text_records(
    source: &str,
    id: &str,
    text: &str,
    opts: &ChunkOptions,
    base_meta: Value,
) -> Vec<Record> {
    let chunks = chunk_text(text, opts);
    let total = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(n, body)| {
            let rec_id = if total <= 1 {
                id.to_string()
            } else {
                format!("{id}#{n}")
            };
            let mut r = Record::new(Source::new(source), "file.document", rec_id, id, body);
            let mut meta = base_meta.as_object().cloned().unwrap_or_default();
            meta.insert("doc".into(), json!(id));
            if total > 1 {
                meta.insert("chunk".into(), json!(n));
                meta.insert("of".into(), json!(total));
            }
            r.meta = Value::Object(meta);
            r
        })
        .collect()
}

/// Ingest raw text (a pasted field, an uploaded UTF-8 text file) as chunked `file.document` records under
/// `(source, id)` (story D-50). Returns the number of records written.
pub fn ingest_text(
    backend: &dyn DatasourceBackend,
    source: &str,
    id: &str,
    text: &str,
    opts: &ChunkOptions,
) -> Result<usize> {
    let recs = text_records(source, id, text, opts, Value::Null);
    if recs.is_empty() {
        return Ok(0);
    }
    backend.upsert(&recs)?;
    Ok(recs.len())
}

/// Ingest `(path, text)` markdown/text docs as chunked `file.document` records under `source` (single-chunk
/// docs keep `id = path`). Chunking (D-50) keeps long docs sharp for keyword + embedding retrieval.
pub fn ingest_markdown(
    backend: &dyn DatasourceBackend,
    source: &str,
    docs: &[(String, String)],
) -> Result<usize> {
    let opts = ChunkOptions::default();
    let mut recs: Vec<Record> = Vec::new();
    for (path, text) in docs {
        recs.extend(text_records(
            source,
            path,
            text,
            &opts,
            json!({ "path": path }),
        ));
    }
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
    use flux_datasource::{GetInput, ListInput, SearchInput};

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

    // ── D-50: text chunking ───────────────────────────────────────────────────────────────────────

    #[test]
    fn short_text_is_a_single_chunk() {
        let opts = ChunkOptions {
            max_chars: 100,
            overlap: 0,
        };
        assert_eq!(
            chunk_text("just a short note", &opts),
            vec!["just a short note".to_string()]
        );
        // whitespace-only text yields no chunks
        assert!(chunk_text("   \n\n  ", &opts).is_empty());
    }

    #[test]
    fn long_text_chunks_into_many_each_searchable() {
        let b = MemoryBackend::new();
        // Four paragraphs, each with a UNIQUE keyword; total well over max_chars.
        let text = "The alpha section covers warm transfers and how announcements work.\n\n\
                    The bravo section covers refunds and the seven day processing window.\n\n\
                    The charlie section covers opening hours and the CET timezone.\n\n\
                    The delta section covers escalation to a human and ticket creation.";
        let opts = ChunkOptions {
            max_chars: 70,
            overlap: 0,
        };
        let chunks = chunk_text(text, &opts);
        assert!(chunks.len() > 1, "chunked into {} pieces", chunks.len());
        assert!(
            chunks.iter().all(|c| c.chars().count() <= 70),
            "each chunk within max: {chunks:?}"
        );

        let n = ingest_text(&b, "local", "handbook", text, &opts).unwrap();
        assert!(n > 1, "wrote {n} records");
        // Each section's unique keyword is independently retrievable.
        for kw in ["alpha", "bravo", "charlie", "delta"] {
            let hits = b
                .search(&SearchInput {
                    query: kw.into(),
                    ..Default::default()
                })
                .unwrap();
            assert!(!hits.is_empty(), "keyword `{kw}` not retrievable");
        }
        // Multi-chunk records are addressed `id#N`.
        let listed = b
            .list(&ListInput {
                source: "local".into(),
                entity: Some("file.document".into()),
                offset: None,
                limit: None,
            })
            .unwrap();
        assert!(
            listed.iter().any(|r| r.id.starts_with("handbook#")),
            "chunk ids: {:?}",
            listed.iter().map(|r| &r.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ingest_text_short_keeps_the_base_id() {
        let b = MemoryBackend::new();
        let n = ingest_text(
            &b,
            "local",
            "note",
            "a single short note",
            &ChunkOptions::default(),
        )
        .unwrap();
        assert_eq!(n, 1);
        let got = b
            .get(&GetInput {
                source: "local".into(),
                entity: "file.document".into(),
                id: "note".into(),
            })
            .unwrap();
        assert!(got.is_some(), "single-chunk record keeps id == base id");
    }
}
