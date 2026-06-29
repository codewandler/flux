# Design: knowledge datasource — a real RAG layer

**Status:** proposed (story [D-07](../stories/D-07-knowledge-datasource-rag.md)) · **Layer:** L5
(`flux-capabilities`, `datasource` module) · **Owner:** Timo

## Why

`flux-capabilities::datasource` today is an **in-memory keyword TF index** + a `search` tool. The
downstream Slack-channel assistant is a knowledge-grounded assistant over a help-center snapshot + OpenAPI references +
skills; it needs durable retrieval with a record model and several retrieval verbs — the shape
`fluxplane-datasource` had. This lifts that shape into flux, additively (the existing in-memory `Index`/
`search` keep working), and keeps the record contract **plugin-friendly** so D-08 integration plugins
contribute records into the same schema.

## Shape

### 1. Record schema
```rust
pub struct Record {
    pub entity: String,         // "file.document" | "openapi.operation" | "gitlab.merge_request" | …
    pub id: String,             // stable within (source, entity)
    pub source: String,         // datasource name, e.g. "local-docs", "downstream-manager_api_docs"
    pub title: String,
    pub body: String,           // indexed text (the chunk)
    pub links: Vec<Link>,       // typed relations: {rel, target_entity, target_id}
    pub meta: serde_json::Value,// freeform (url, path, line, updated_at)
}
```
A record is `(source, entity, id)`-addressable; `links` carry relations for `relation`.

### 2. Persistent index
A sqlite-backed store (reuse `flux-events`' sqlite/WAL patterns; a `datasource.db` or a table set), holding
records + an inverted keyword/BM25 index over `title`+`body`. The in-memory `Index` becomes the default
backend behind a small trait so the persistent one is a drop-in:
```rust
pub trait DatasourceBackend: Send + Sync {
    fn upsert(&self, recs: &[Record]) -> Result<()>;
    fn search(&self, q: &Query) -> Result<Vec<Hit>>;     // keyword/BM25 in v1
    fn get(&self, source: &str, id: &str) -> Result<Option<Record>>;
    fn list(&self, source: &str, entity: Option<&str>, page: Page) -> Result<Vec<Record>>;
    fn relation(&self, source: &str, id: &str, rel: Option<&str>) -> Result<Vec<Record>>;
}
```

### 3. Retrieval ops (agent-facing tools)
Each implements `flux_runtime::Tool` with an input JSON Schema and dispatches through `Executor`:
- `search(query, source?, entity?, k?)` → ranked hits (title, snippet, id, source).
- `get(source, id)` → one record.
- `list(source, entity?, page?)` → records (enumerate a datasource).
- `relation(source, id, rel?)` → linked records.
- `batch_get(source, ids[])` → records (one round-trip for a hit set).

### 4. Ingester + freshness
- `ingest_dir(path, globs, source, entity)` — markdown → chunked `file.document` records.
- `ingest_openapi(json, source)` — operations/schemas/params → `openapi.*` records.
- `reindex(source)` + a `freshness(source)` staleness check (an `updated_at` watermark). The Slack-channel assistant calls
  these at boot over `bot/data/knowledge/**`.

### 5. Embeddings seam (deferred backend)
A `Embedder` trait (`embed(texts) -> Vec<Vec<f32>>`) and a `semantic` query path are **defined but not
wired** in v1 — keyword/BM25 only. A vector backend (and hybrid rerank) lands behind this seam on demand.

## Testing (hermetic)
- Ingest the help-center fixtures → `search("warm transfer")` returns the matching article; `get(id)`
  round-trips it; `list("local-docs", "file.document")` enumerates them. No network.
- Persistence: upsert, reopen the store, `get` still resolves (proves durability).
- Op envelope: each op dispatches through a real `Executor` (deny-by-default outside the read set).

## Non-goals (v1)
- Vector/embedding retrieval, hybrid rerank, cross-source lookup-fanout (all behind the embeddings seam).
- A `.dex`-style endpoint registry / instance scoping (out of scope; config + env for the bot).
- Per-record ACLs (the datasource is read-only knowledge; authorization is at the op/tool layer).

## Reuse, don't reimplement
- The existing `datasource::Index`/`SearchTool`; `flux-events`' sqlite patterns; the op-input-schema +
  `Executor::dispatch` machinery. D-08 plugins emit `Record`s into this schema (the integration datasource
  surface) — keep the contract stable.
