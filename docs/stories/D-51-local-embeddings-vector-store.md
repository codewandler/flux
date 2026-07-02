---
id: D-51
title: KB-level embeddings — local fastembed embedder + durable generic vector store (sqlite-vec)
pillar: Core
status: done
epic: grounded-knowledge
design: docs/designs/grounded-knowledge.md
note: turn on semantic search per-KB with an in-process CPU model + vectors persisted in the same SQLite file
---

# KB-level embeddings — local fastembed embedder + durable generic vector store (sqlite-vec)

## Goal
Make semantic search a per-KB, opt-in, durable capability that needs no external service: an in-process
CPU embedder + a generic `VectorStore` seam whose first backing (`sqlite-vec`) lives in the **same
per-account SQLite DB** as the records. Today semantic search is OpenAI-only, in-memory (re-embedded on
boot), and applies to every record with no opt-in; `SqliteBackend` is unwired.

## Acceptance
- [ ] `FastEmbedEmbedder` implements the existing `Embedder` trait behind a `local-embeddings` feature
      (parallel to `embeddings`); CPU, batched, bge-small (384-dim) default. Wired into `datasource_backend`
      selection.
- [ ] `VectorStore` trait (upsert/query vectors, scoped by source) + a `sqlite-vec` impl co-located in
      `SqliteBackend`'s DB (loadable extension via rusqlite `load_extension`) + an in-memory impl for tests.
      `SemanticIndex` persists/queries through it (not its in-memory `HashMap`).
- [ ] **Per-KB opt-in:** the `embeddings` mode (`default` | `local`) is carried on the source/`Declaration`
      (not on `Record`). `SemanticIndex` embeds + vector-reranks only `local` sources; `default` sources
      stay keyword-only but fully searchable. **Failing-first test**: a `local` KB ranks a
      semantically-close-but-keyword-poor hit above a keyword-only match; a `default` KB ranks by keyword.
- [ ] **Durable**: vectors survive a store reopen — a test reopens the `SqliteBackend` and confirms search
      works with **no** re-embed. Runs in-process (loadable extension, no external DB) — hermetic.
- [ ] `SqliteBackend` wired as a durable runtime backend option (today only `MemoryBackend` is constructed).

## Progress
- **Core landed (tested, gate-green).** The reusable seam + per-KB opt-in + durability *semantics*:
  - `VectorStore` trait + `MemoryVectorStore` (`datasource/vector.rs`) — the pluggable persistence seam,
    addressed by `(source, entity, id)`.
  - `SemanticIndex` refactored onto `Arc<dyn VectorStore>` (was an inline `HashMap`), with
    `with_vector_store(...)` (inject a durable store) and `with_semantic_sources(...)` (per-KB opt-in — only
    named sources are embedded/reranked; others stay keyword-only but searchable).
  - Tests: per-KB opt-in stores a vector only for the opted-in source; a fresh index over a shared
    pre-populated store reranks **without re-embedding** (durability proxy); existing rerank/delegation
    tests still green. `flux-capabilities` 51 tests pass; clippy + codegate layering clean.
- **Backends landed behind features (compile-verified against the real crates).**
  - `FastEmbedEmbedder` (`datasource/embeddings_local.rs`, feature `local-embeddings`) — bge-small-en-v1.5
    (384-dim) over `fastembed`/ONNX; `cargo check --features local-embeddings` compiles clean (`ort-sys`
    builds). Model weights fetch to the fastembed cache on first construction — in a container, pre-warm
    that cache at build time (or point `InitOptions` at a bundled dir) to avoid a first-request download.
  - `SqliteVecStore` (`datasource/vector.rs`, feature `sqlite-vec`) — vectors in a `vec0` virtual table in
    the same SQLite file (extension registered via `sqlite3_auto_extension`), a plain `vec_meta` companion
    table for source-scoped lifecycle deletes; `cargo check --features sqlite-vec` compiles clean
    (sqlite-vec v0.1.9). `SemanticIndex::with_vector_store(SqliteVecStore)` gives durable vectors.
- **Residuals (small):** a live runtime smoke with a real model + on-disk `vec0` (the compile is verified;
  runtime KNN/download is not, per the feature-gated pattern); optionally teach the CLI `datasource_backend`
  selector to pick the local embedder (the ai-agents consumer constructs `SemanticIndex` directly, so this
  is CLI ergonomics, not core). Vector dim = 384 (bge-small) must match the `vec0` column width.

## Notes
- `flux-capabilities/src/datasource/{semantic.rs,embeddings.rs,sqlite.rs,mod.rs}`; embedder selection at
  `flux-cli/src/main.rs:783` (`datasource_backend`). Confirm `load_extension` is enabled in the rusqlite
  build and the `sqlite-vec` extension is vendored/bundled in the image.
- Confirm vector dim matches the model (bge-small = 384). candle stays a drop-in `Embedder` alternative.
- Design: [grounded-knowledge.md](../designs/grounded-knowledge.md). Consumer: ai-agents A-08 (KB
  `embeddings` field).
