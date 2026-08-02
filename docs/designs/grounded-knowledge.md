# Design: grounded knowledge — KB-injection into the system prompt + multi-shape, embeddable datasources

**Status:** ✅ shipped (A-19 · D-50 · D-51 done) · **Pillar:** Agent (prompt assembly) + Core (datasource layer) · **Builds on:** [datasource-rag.md](datasource-rag.md) ·
**Stories:** [A-19](../stories/A-19-context-block-injection.md) ·
[D-50](../stories/D-50-text-file-chunking-ingester.md) ·
[D-51](../stories/D-51-local-embeddings-vector-store.md)

Master plan (cross-repo, incl. the ai-agents consumer half): `~/.claude/plans/shiny-seeking-beaver.md`.

## Why

flux's datasource layer (D-07) delivers knowledge to a model **only** as retrieval **tool calls**
(`search`/`get`/…). Two gaps surface when a consumer (a downstream ai-agents service) builds a
customer-facing knowledge feature on top:

1. **No prompt-injection path.** A small KB (an FAQ, a page of policy) is best handed to the model
   *inline* — no tool round-trip, and it grounds even a near-empty agent. There is no `add_context` /
   `<knowledge-base>` seam anywhere today; retrieval is the only ingress. (This is also why a *bare* agent
   with an empty system prompt is ungoverned: nothing constrains it, so a voice model free-associates from
   its own training — the incident that triggered this epic.)
2. **Narrow ingestion + no per-KB semantic control.** The only ingesters are a whole-file
   `ingest_markdown` (no chunking) and `ingest_openapi`. Semantic search exists (`SemanticIndex` +
   `Embedder` + `OpenAiEmbedder`) but is feature-gated, OpenAI-only, in-memory (rebuilt on boot), and
   applies to *every* record — there is no per-source opt-in and no durable vector store. `SqliteBackend`
   (FTS5/BM25) is implemented but unwired.

This epic adds the two reusable primitives a grounded-knowledge product needs, keeping retrieval
tool-based and unchanged.

## Design

### A-19 · Context-block injection (`add_context`) — Agent pillar

- `AgentSpec` grows `context: Vec<ContextBlock>` where `ContextBlock { id, title, meta, body }`
  (`flux-agent/src/lib.rs`). At context-package assembly the blocks render, after the profile,
  authored instructions, and repository layers, as:
  ```
  <knowledge-base id="hours" title="Opening hours">
  Mon–Fri 09:00–18:00 CET …
  </knowledge-base>
  ```
  A byte budget bounds the total; over-budget content truncates with a visible marker (never a silent
  drop). Empty context changes the prompt not at all (cache-stable).
- SDK: `FlowClient::builder().add_context(id, title, body)` and `AgentSpec::with_context(...)`.
- App path: `flux-app/src/app.rs` `agent_spec_from_decl` renders injected blocks alongside the
  `description`/`instructions`/`instruction_files` persona assembly.
- Shared renderer `render_knowledge_blocks(records, budget) -> String` in
  `flux-capabilities::datasource`, so datasource records and hand-supplied context produce identical block
  text (a consumer injecting KB records reuses it).

### D-50 · Raw-text / file(text) chunking ingester — Core pillar

- A size-aware chunker + `ingest_text(backend, source, id, text, opts)` producing chunked
  `file.document` records (chunk index in `meta`), in `flux-capabilities/src/datasource/ingest.rs`.
  `ingest_markdown` becomes a thin caller of it; a file upload is the same path after a UTF-8 read.
- **Text formats first** (.md/.txt/.csv/.json). Binary extraction (PDF/DOCX) is deferred.

### D-51 · KB-level embeddings: local embedder + durable generic vector store — Core pillar

- **Local embedder:** `FastEmbedEmbedder` (fastembed-rs / ONNX) implementing the existing `Embedder`
  trait, behind a `local-embeddings` Cargo feature (parallel to the OpenAI `embeddings` feature). CPU,
  batched, bge-small (384-dim) default.
- **Generic durable vector store:** a `VectorStore` trait (upsert/query vectors, scoped by source) with a
  **`sqlite-vec`** implementation co-located in the `SqliteBackend` DB (loadable extension via rusqlite
  `load_extension`), plus an in-memory impl for tests. `SemanticIndex` persists/queries through it instead
  of its in-memory `HashMap`, so vectors survive a reopen (no re-embed on boot). Records (FTS5) + vectors
  share one file — no external DB, no new infra service.
- **Per-KB opt-in:** the `embeddings` choice (`default` = keyword/BM25 · `local` = in-process CPU ·
  `openai/<model>` deferred) is carried on the **source** (a `Declaration` field or a reserved source-config
  map — **not** on `Record`). `SemanticIndex` embeds + vector-reranks only `local` sources; `default`
  sources stay keyword-only but fully searchable.
- Wire `SqliteBackend` as a durable runtime backend (today only `MemoryBackend` is constructed by the CLI).

## Reused, not reinvented

`DatasourceBackend`/`MemoryBackend`/`SqliteBackend`, `SemanticIndex` (hybrid keyword+cosine rerank),
`Embedder` trait, `ingest_markdown`, the 5 retrieval ops, and the `DatasourceHostCaps` plugin→index bridge
all stand. This epic turns existing scaffolding on (SqliteBackend, SemanticIndex) and adds three seams
(context blocks, text chunker, local embedder + vector store).

## Consumer (ai-agents)

The downstream ai-agents service wires to these: raw-text/file KB shapes ingest via D-50; a small attached
KB injects via A-19 (`<knowledge-base>` blocks in the voice/text persona) while a large one keeps the
`search` tool; a KB's `embeddings=local` turns on D-51's local semantic path in that account's SQLite
store. See the ai-agents `knowledge-sources-v2` epic + the master plan.
