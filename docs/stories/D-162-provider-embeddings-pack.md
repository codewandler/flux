---
id: D-162
title: Provider embeddings pack — explicit config, per-source routing, usage capture
pillar: Core
status: done
priority:
epic: grounded-knowledge
design: ../designs/grounded-knowledge.md
note: "downstream ask (ai-agent-platform): make the embeddings seam production-ready — explicit-config OpenAiEmbedder constructor + per-source embedder/model routing + embed-usage capture; Bedrock embeddings and pgvector are explicit stretch"
---

# Provider embeddings pack — explicit config, per-source routing, usage capture

## Goal
Close the three production gaps in the embeddings seam so a hosting consumer can configure and
observe embeddings the way it configures chat providers: explicit (non-env) construction, per-source
embedder/model selection, and visible embedding token usage. Serves the Core pillar (shared provider
machinery under the grounded-knowledge datasource layer).

## Acceptance
- [x] **Explicit-config constructor.** `OpenAiEmbedder::new(api_key, endpoint, model)` takes explicit
      config with no environment access; `from_env()` is now a thin wrapper over it. Added `model()`/
      `endpoint()` accessors. Failing-first test `new_takes_explicit_config_without_env`
      (`crates/flux-capabilities/src/datasource/embeddings.rs`).
- [x] **Per-source embedder routing.** `SemanticIndex::with_source_embedder(source_key, embedder)`
      routes a source to its own embedder (`.../datasource/semantic.rs`); the default single-embedder
      path is byte-identical when no override is configured — the CLI `datasource_backend`
      (`crates/flux-cli/src/main.rs:1221`) is **unchanged**. Search stays within one embedding space:
      a scoped query is embedded by its source's embedder and cosine only applies to same-embedder
      vectors (`Arc::ptr_eq`). Failing-first tests
      `per_source_routing_embeds_each_source_with_its_own_embedder` and
      `scoped_search_uses_the_source_routed_embedder_for_the_query`.
- [x] **Embed-usage capture.** The embeddings `usage` object is no longer discarded: a pure
      `parse_embeddings_response` extracts `(vectors, EmbeddingUsage)`, the embedder accumulates it
      (`usage_snapshot()`), and `EmbeddingUsage::as_usage()` folds it onto the shared
      `flux_core::Usage` tally (prompt → input tokens). Failing-first tests
      `parses_vectors_and_captures_usage`, `missing_usage_is_a_zero_tally_not_an_error`,
      `embedding_usage_maps_onto_core_usage`.
- [ ] **Follow-up (thin, deferred):** emit an `EventKind::CallUsage` from the CLI after an indexing
      run so embedding spend shows in `flux usage`. The capture + `as_usage()` seam is in place; the
      remaining piece is CLI-side event emission, deliberately left out of this change to avoid
      touching the flux-cli event wiring the concurrent sdk-surface epic is also editing. A pricing
      tier for embedding models can ride the same follow-up.
- [ ] **Stretch (separate follow-up stories):** a Bedrock embeddings `Embedder` reusing the SigV4 /
      credential machinery in `crates/flux-providers/src/bedrock.rs`; a pgvector `VectorStore` for the
      Postgres backend (durable vectors are SQLite `vec0` only today).

## Progress
- 2026-07-11 — IMPLEMENTED (engine-side, flux-capabilities only; no flux-cli change). Three
  production gaps closed: explicit-config `OpenAiEmbedder::new` (+ `from_env` wrapper + accessors);
  per-source embedder routing on `SemanticIndex` (`with_source_embedder`, `embedder_for`, per-source
  upsert grouping, same-embedding-space search gating) with the default path byte-identical; and
  embed-usage capture (`EmbeddingUsage` type in `datasource/mod.rs`, pure `parse_embeddings_response`,
  accumulation via atomics, `usage_snapshot()`, `as_usage()` → `flux_core::Usage`). 6 failing-first
  tests added. Scoped gate green: `cargo test -p codewandler-flux-capabilities` (57) and
  `--features embeddings` (61), `cargo clippy` both feature configs `-D warnings`, `cargo fmt`,
  `cargo test -p flux-codegate` (layering) — all clean. The `flux usage` event-projection emission is
  the one documented CLI follow-up (see Acceptance); the usage is captured and mappable, only the
  emit remains.

## Notes
- Engine-side only — `flux-capabilities` internals; the CLI `datasource_backend` wiring is untouched,
  so the default single-embedder behavior is unchanged. **No new SDK surface** — consistent with the
  sdk-surface design's exclusion of a first-class datasource API before D-62.
- Verified against the pre-change code: one global embedder (per-source only on/off via
  `with_semantic_sources`), usage discarded, no Bedrock embeddings, no pgvector.
