---
id: D-07
title: Knowledge datasource — a real RAG layer (record schema, persistent index, retrieval ops)
pillar: Core
status: backlog
priority:
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
- [ ] A datasource **record schema** (entity type, id, source, title, body/snippet, links/relations) +
      a **persistent** index (sqlite-backed; reuse `flux-events`' storage patterns). Additive — the
      existing in-memory `Index`/`search` keep working.
- [ ] Retrieval ops `search` / `list` / `get` / `relation` / `batch_get` implement `flux_runtime::Tool`
      and dispatch through `Executor` (the safety envelope), each with an input JSON Schema.
- [ ] An **ingester** loads a directory of markdown + an OpenAPI JSON into typed records. Failing-first
      test: index the help-center fixtures → `search("warm transfer")` returns the matching article record;
      `get(id)` round-trips it.
- [ ] A **reindex/freshness** API (rebuild + a staleness check). The **embeddings** path is a trait seam
      with **no backend wired** in v1 (documented as the deferred slice).
- [ ] Full gate green; `flux-codegate` layer placement confirmed (datasource stays within `flux-capabilities`).

## Progress
- Backlog. Design doc to write: `docs/designs/datasource-rag.md`.

## Notes
- Reuse, don't reimplement: the existing `datasource::Index`/`SearchTool`, `flux-events`' sqlite/WAL
  patterns, the op-input-schema + `Executor::dispatch` machinery.
- Integration plugins (**D-08**) contribute datasource records (e.g. `gitlab.merge_requests`,
  `slack.messages`) into this same schema — keep the record contract plugin-friendly.
- Serves Slack-channel assistant **S-01/S-03**. Non-goal (v1): vector/embedding retrieval, hybrid rerank, a cross-source
  lookup-fanout resolver (those land behind the embeddings seam on demand).
