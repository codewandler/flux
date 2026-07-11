---
id: D-161
title: web_fetch extracts text from PDFs instead of returning raw bytes
pillar: Agent
status: done
priority:
epic: web-capabilities
design: ../designs/web-capabilities.md
note: "downstream ask (ai-agent-platform, consumer ask A-47): web_fetch on a PDF returns extracted text, not raw lossy bytes — a content-type/%PDF branch in the non-HTML path; lifts part of the grounded-knowledge binary-extraction deferral for the web path only"
---

# web_fetch extracts text from PDFs instead of returning raw bytes

## Goal
`web_fetch` on a PDF URL returns readable extracted text, the same way it returns condensed markdown
for HTML — so a model fetching a linked PDF gets usable content instead of the current lossy-UTF-8
byte dump. Serves the Agent pillar's web-read capability and the downstream consumer's document
ingestion.

## Acceptance
- [x] Content-type dispatch in the non-HTML branch of `web_fetch` (`crates/flux-web/src/fetch.rs`):
      a PDF (content-type `application/pdf` **or** `%PDF` magic-byte sniff via `looks_like_pdf`) is
      run through `extract_pdf_text` and capped via `cap_str`, exactly as HTML output is capped.
      Extraction is panic-safe (`catch_unwind`; `pdf-extract` panics on some inputs) with a raw
      fallback — a malformed PDF never crashes `web_fetch`.
- [x] Failing-first: `pdf_body_is_returned_as_extracted_text` (declared `application/pdf`) and
      `pdf_extracted_via_magic_byte_sniff_when_mislabeled` (`application/octet-stream`) — both failed
      with the old branch (raw `%PDF` bytes leaked), pass after. Fixtures generated in-test via a
      `lopdf` dev-dep (no binary blob to track).
- [x] Non-PDF binary behavior unchanged — regression test `non_pdf_binary_stays_raw`.
- [x] `web_fetch` tool description updated to state PDFs return extracted text (and the public/engine
      op-catalog rows note it).
- [x] Extraction uses the pure-Rust `pdf-extract` 0.12 (walks the PDF via `lopdf` + font parsers; no
      `*-sys`, no pdfium, no shelling out).

## Progress
- 2026-07-11 — IMPLEMENTED (flux-web). `web_fetch` now routes PDF responses to text extraction
  (`is_pdf` = content-type or `%PDF` sniff → `extract_pdf_text`, panic-safe, raw fallback), capped
  like HTML. Added `pdf-extract` 0.12 (dep) + `lopdf` (dev-dep, builds fixture PDFs). 3 new tests,
  failing-first confirmed. Scoped gate green: `cargo test -p codewandler-flux-web` (44),
  `cargo clippy … -D warnings`, `cargo fmt` — all clean. Known limit: a PDF larger than the 256KB
  per-page read cap falls back to raw (its xref/trailer is truncated) — a follow-up if large-PDF
  fetch is needed.

## Notes
- Scope boundary: this is the **web-fetch path only**. First-class datasource *file* ingestion of
  PDFs/DOCX remains deferred (D-50 `## Notes`, `docs/designs/grounded-knowledge.md:55`) — out of
  scope here; this story lifts the deferral only for content pulled over `web_fetch`.
- Insertion point verified: `fetch.rs` currently has a single binary HTML-vs-not branch
  (`is_html` at `fetch.rs:155`); there is no per-content-type dispatch and no `%PDF` sniff today.
- Lands in flux-web (see C-51 for publishing).
