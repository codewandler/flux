---
id: D-166
title: web.crawl — a caller byte/page budget that stops the crawl early
pillar: Agent
status: done
priority:
epic: web-capabilities
design: ../designs/web-capabilities.md
note: "downstream ask (ai-agent-platform, consumer ask C-37): let web.crawl accept a caller byte budget (or a stop-signalling RecordSink) so a crawl halts as soon as the budget is spent, instead of always running to max_pages"
---

# web.crawl — a caller byte/page budget that stops the crawl early

## Goal
Extend the D-160 `web.crawl` primitive so the caller can bound a crawl by **total content bytes**, not
only `max_pages`/`max_depth`. Today a crawl always fetches up to `max_pages` and the caller discovers
any overshoot only after the fact (`crates/flux-web/src/crawl.rs:215` breaks solely on
`fetched >= max_pages`, and `RecordSink::contribute` returns `()` — `crates/flux-web/src/lib.rs:43` —
so a consumer has no way to stop the BFS mid-stream). Serves the Agent pillar (bounded, polite web
reach) and unblocks the downstream consumer's per-account byte quota without wasted egress.

## Acceptance
- [x] `web.crawl` accepts a caller **byte budget** — a `max_total_bytes` param (checked against
      the running condensed-content total after each page). The BFS stops as soon as
      the budget is crossed; the pages already gathered return `Ok` (partial crawl, the existing
      per-page skip-not-fatal contract). Chose the param over a stop-signalling `RecordSink` — it reuses
      the existing `total_render`/`MAX_TOTAL_RENDER_BYTES` accounting and leaves the sink seam untouched.
- [x] Failing-first test: a multi-page `site_server` fixture crawled with a low byte budget fetches
      **fewer than `max_pages`** pages and stops (mirrors `bfs_stops_at_depth_and_page_caps`, but
      capped on bytes rather than page count) — `byte_budget_stops_crawl_before_page_cap`.
- [x] The SSRF/egress envelope, same-host scoping, and existing caps are unchanged; the new budget is
      an additional upper bound (clamped to the 512 KiB ceiling), never a widening of any axis.
- [x] Op-catalog docs updated for the new param, the same set D-160 touched: `website/docs/language/ops.md`
      (the `website_contract` test), `crates/flux-flow/docs/ops-reference.md`, and the engine skill's
      registered-ops table.

## Progress
- (done, 2026-07-12) — added a `max_total_bytes` param to `web.crawl` (`crates/flux-web/src/crawl.rs`):
  parsed and clamped to `[1, MAX_TOTAL_RENDER_BYTES]` (default = the 512 KiB ceiling), then used as the
  loop's byte-budget break in place of the bare ceiling — so it stops early on the running condensed
  total and always still yields at least the seed. Failing-first test
  `byte_budget_stops_crawl_before_page_cap` added. Docs mirrored in all three catalogs. Gate green
  (flux-web tests, clippy, skill/website contract tests, codegate layering).

## Notes
- **Why now.** The consumer (ai-agent-platform) enforces a per-account `max_knowledge_bytes` quota. It
  already has an app-side backstop (C-36: a crawl that overshoots is rejected *after* the fetch,
  ingesting nothing), but that wastes the egress of the overshoot. This op-level budget lets the crawl
  stop early. C-37 is the consumer story; it stays gated on this landing + a flux-web publish.
- **Extends** [D-160](D-160-web-crawl-primitive.md) (the crawl primitive). Reuse the same wiring:
  `WebOptions`/`RecordSink` (`crates/flux-web/src/lib.rs:41-67`), the BFS loop + cap consts
  (`crates/flux-web/src/crawl.rs`), condensed-content byte accounting (already computed per page for
  `MAX_TOTAL_RENDER_BYTES`).
- **Publish closure.** flux-web is not yet in the crates.io publish closure ([C-51](C-51-flux-web-publish-closure.md));
  the consumer picks this up once flux-web ships a release carrying it.
- Keep the v1 non-goals (robots.txt, sitemaps, cross-host, JS-rendered) unchanged — this is purely an
  additional caller-side bound.
