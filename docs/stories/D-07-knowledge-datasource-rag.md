---
id: D-07
title: Knowledge datasource — a real RAG layer (record schema, persistent index, retrieval ops)
pillar: Core
status: done
theme: downstream-managed-agents
design: docs/designs/datasource-rag.md
---

# Knowledge datasource — a real RAG layer

## Goal
Turn `flux-capabilities`'s `datasource` module from an in-memory keyword index into a real knowledge
layer: a typed **record schema** (entity type / id / source / title / links), a **persistent** index, the
retrieval ops `search` / `list` / `get` / `relation` / `batch_get`, and a **reindex/freshness** API — so an
app can ground answers in local docs and OpenAPI specs. v1 ships keyword/BM25 retrieval behind a
**pluggable embeddings seam** (a semantic backend is deferred, not wired).

## Why (downstream: Slack-channel assistant, managed-agents)
The downstream Slack-channel assistant is, at its core, a **knowledge-grounded Q&A assistant** over a help-center
snapshot + bundled OpenAPI references + skills (its `bot/data/knowledge/**`). The fluxplane bot leaned on
`fluxplane-datasource` (semantic + keyword indexing, datasource records, freshness). flux today only has an
**in-memory keyword TF index + a `search` tool** (`flux-capabilities::datasource`) — no record schema, no
persistence, no `list`/`get`/`relation`/`batch_get`, no OpenAPI ingest. This is the largest *un-storied*
gap behind the Slack-channel assistant's v0 (journey RAG) and v1 (agentic) milestones.

## flux gap
`flux-capabilities` `datasource` provides an `Index` (term-frequency keyword scoring) + a `SearchTool`
(the agent-facing `search` op). Missing: a record/entity schema, a durable index, the four other retrieval
ops, an ingester for markdown + OpenAPI, and any embeddings seam.

## Acceptance
- [x] A datasource **record schema** in a **new L0 crate `flux-datasource`**: `Record`/`RecordBase`
      (entity / id / source{plugin,instance} / title / body / links / meta), `Declaration` +
      `EntitySchema`, and `Search`/`Get`/`List`/`Relation`/`BatchGet` input/output + `Match{score,
      matched_fields}`. Pure, no IO — shared by `flux-plugin` (L4) and `flux-capabilities` (L5); classified
      L0 in `flux-codegate`. `EntitySchema` declared explicitly (derive deferred). Commit `2642479`.
- [x] A **persistent** index — the **`SqliteBackend`** (records table + FTS5 virtual table over title+body,
      `bm25()` ranking, WAL); the in-memory `MemoryBackend` stays the default, both behind a
      `DatasourceBackend` trait. Reopen-the-store test proves durability. Commit `5241c97`.
- [x] Retrieval ops `search` / `get` / `list` / `relation` / `batch_get` implement `flux_runtime::Tool`
      with input JSON Schemas, registered via `register_datasource_ops`. Commit `e6d7279`.
- [x] Ingesters: `ingest_markdown` + `ingest_openapi` (operations + component schemas → typed records).
      Tests: markdown index → `search("warm transfer")` hits the article; OpenAPI → operation/schema
      records + `get` round-trip. Commit `5241c97`.
- [x] `reindex` (clear-then-reingest, via `DatasourceBackend::clear`) + `freshness` (record count). The
      **embeddings** path is the `Embedder` trait seam with **no backend wired** (deferred). Commit `5241c97`.
- [x] Full gate green; `flux-codegate` layer placement confirmed (`flux-datasource` L0; `flux-capabilities`
      stays L5).

## Progress
- **Done** (commits `2642479` → `e6d7279` → `5241c97`). The whole knowledge layer: the L0 `flux-datasource`
  schema, the `DatasourceBackend` trait with in-memory + SQLite-FTS5 backends, the five retrieval ops,
  markdown + OpenAPI ingesters, reindex/freshness, and the (unwired) embeddings seam. Unblocks **D-10**
  (plugins emit these records) and **D-08**.

## Notes
- Reuse, don't reimplement: the existing `datasource::Index`/`SearchTool`, `flux-events`' sqlite/WAL
  patterns, the op-input-schema + `Executor::dispatch` machinery. Record/lookup shapes ported (not copied)
  from `fluxplane-datasource`.
- The shared `flux-datasource` crate is the record contract: integration plugins (**D-08**, over the
  **D-10** protocol) contribute records (e.g. `gitlab.merge_request`, `slack.channel`) into this same
  schema via the L5 `DatasourceHostCaps` bridge — keep it plugin-friendly.
- Serves Slack-channel assistant **S-01/S-03**. Non-goal (v1): vector/embedding retrieval, hybrid rerank, a cross-source
  lookup-fanout resolver (those land behind the embeddings seam on demand).
