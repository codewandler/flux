---
id: D-50
title: Raw-text / file(text) chunking ingester
pillar: Core
status: done
epic: grounded-knowledge
design: docs/designs/grounded-knowledge.md
note: raw text + uploaded text files become chunked file.document records (one seam, reused by ingest_markdown)
---

# Raw-text / file(text) chunking ingester

## Goal
Turn arbitrary text (a pasted raw-text field, an uploaded text file) into well-sized, searchable
`file.document` records via a size-aware chunker — the ingestion primitive the ai-agents "knowledge
shapes" feature (raw_text / file) needs. Today `ingest_markdown` stores a whole file as one record.

## Acceptance
- [ ] `ingest_text(backend, source, id, text, opts)` chunks `text` into N `file.document` records
      (chunk index in `meta`) and upserts them. **Failing-first test**: a long string → N>1 searchable
      chunks (each independently a search hit); a short string → exactly 1 record.
- [ ] `ingest_markdown` is re-expressed in terms of the chunker (behavior for existing callers preserved
      or improved — existing `flux-capabilities` datasource tests stay green).
- [ ] Chunk size/overlap are `opts` with sane defaults; boundaries prefer paragraph/sentence breaks over
      mid-word cuts (asserted loosely — no cut inside a token where avoidable).
- [ ] Text formats only for v1 (.md/.txt/.csv/.json read as UTF-8 upstream); PDF/DOCX explicitly deferred
      (noted, not implemented).

## Progress
- **Done.** `ChunkOptions{max_chars,overlap}` (default 1500/150) + `chunk_text` (paragraph packing →
  sentence split → hard char-window fallback, with prepended overlap that doesn't compound) +
  `ingest_text(backend, source, id, text, opts)` → chunked `file.document` records. Single-chunk docs keep
  `id` verbatim; multi-chunk get `id#N` ids + `chunk`/`of` in `meta`. `ingest_markdown` re-expressed on the
  chunker (long docs now chunk; short docs unchanged). Re-exported at the crate root. Tests: short→1 chunk,
  empty→0, long→N>1 chunks each ≤max and each keyword independently searchable, single-chunk keeps base id;
  existing `flux-capabilities` datasource + `flux-cli build_datasources` tests stay green.

## Notes
- `flux-capabilities/src/datasource/ingest.rs` (`ingest_markdown` at :16). Keep the caller-does-the-walk
  contract (ingesters take already-read `(path,text)`); the chunker is pure.
- Design: [grounded-knowledge.md](../designs/grounded-knowledge.md). Consumer: ai-agents A-08.
