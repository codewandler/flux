# Design: knowledge datasource — a real RAG layer

**Status:** proposed (story [D-07](../stories/D-07-knowledge-datasource-rag.md)) · **Layers:** L0 (new
`flux-datasource` schema crate) + L5 (`flux-capabilities` `datasource` module) · **Owner:** Timo

## Why

`flux-capabilities::datasource` today is an **in-memory keyword TF index** + a `search` tool. The
downstream Slack-channel assistant is a knowledge-grounded assistant over a help-center snapshot + OpenAPI references +
skills; it needs durable retrieval with a record model and several retrieval verbs — the shape
`fluxplane-datasource` had. This lifts that shape into flux, additively (the existing in-memory `Index`/
`search` keep working), and keeps the record contract **plugin-friendly** so D-08 integration plugins
contribute records into the same schema.

## Shape

### 1. Record schema — a new **L0 `flux-datasource` crate**
The record/declaration/lookup types live in a **pure, no-IO L0 crate `flux-datasource`**, so **both**
`flux-plugin` (L4 — plugins emit records, via [D-10](process-plugin-protocol.md)) **and**
`flux-capabilities` (L5 — the index) share one contract without a layering violation. `EntitySchema` is
declared **explicitly**; a `flux-datasource-derive` `#[derive(EntitySchema)]` is an **optional** convenience
(flux has no proc-macro precedent — explicit values are the baseline). Shapes ported (not copied) from
`fluxplane-datasource`:
```rust
pub struct Record {
    pub entity: String,         // "file.document" | "openapi.operation" | "gitlab.merge_request" | …
    pub id: String,             // stable within (source, entity)
    pub source: Source,         // { plugin, instance } — the datasource origin
    pub title: String,
    pub body: String,           // indexed text (the chunk)
    pub links: Vec<Link>,       // typed relations: {rel, target_entity, target_id}
    pub meta: serde_json::Value,// freeform (url, path, line, updated_at)
}
// + Declaration / EntitySchema / SchemaField (manifest-facing) and
//   Lookup / Search / Get input+output with Match { score, matched_fields } (retrieval).
```
A record is `(source, entity, id)`-addressable; `links` carry relations for `relation`. `Declaration` +
`EntitySchema` are what a D-08 plugin's manifest uses to declare a contributed datasource.

### 2. Persistent index
A sqlite-backed store (reuse `flux-events`' `Connection`+WAL patterns; a `datasource.db` or a table set),
holding records + a **FTS5** virtual table over `title`+`body` for keyword/**BM25** ranking (the workspace
`rusqlite` is `features = ["bundled"]`, so SQLite ships with FTS5 and the built-in `bm25()` function — no
hand-rolled inverted index). The in-memory `Index` stays the default backend behind a small trait so the
persistent one is a drop-in:
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

### 5. Embeddings seam — wired behind a feature gate (C-02)
A `Embedder` trait (`embed(texts) -> Vec<Vec<f32>>`). Originally defined-but-unwired; **now wired by
[C-02](../stories/C-02-integration-stack-hardening.md) behind the `embeddings` Cargo feature** (off by
default — the keyword path is unchanged): a `SemanticIndex` decorator wraps any `DatasourceBackend`,
embeds title+body on upsert, and on search reranks the keyword candidates by a blend of normalized keyword
score + query/record cosine similarity; the concrete `OpenAiEmbedder` calls an OpenAI-compatible
`/v1/embeddings` (runtime-free `ureq` + `guard_url`, env config). v1 holds vectors in-memory (rebuilt on
ingest) — durable vector storage + a local embedder are follow-ups.

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
  `Executor::dispatch` machinery. The new `flux-datasource` L0 crate is the shared record contract:
  D-08 plugins emit `Record`s into it over the D-10 protocol, reaching the L5 index through the
  `DatasourceHostCaps` bridge (see [integration-plugins.md](integration-plugins.md)) — keep the contract
  stable. Classify `flux-datasource`/`-derive` as **L0** in `flux-codegate`.
