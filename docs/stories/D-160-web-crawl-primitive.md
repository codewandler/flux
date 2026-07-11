---
id: D-160
title: web.crawl — a bounded, SSRF-guarded crawl primitive
pillar: Agent
status: done
priority:
epic: web-capabilities
design: ../designs/web-capabilities.md
note: "downstream ask (ai-agent-platform, consumer ask A-44): a bounded, SSRF-guarded web.crawl op — seed + same-host link-following under page/depth caps — reversing the web-capabilities crawling non-goal"
---

# web.crawl — a bounded, SSRF-guarded crawl primitive

## Goal
Give the agent a native `web.crawl` op: from a seed URL, follow same-host links to bounded depth and
page counts, returning per-page condensed markdown (and contributing `web.page` records), so a model
can read a small site/section without one-URL-at-a-time `web_fetch` loops. Serves the Agent pillar
(the agent's own web reach) and unblocks the downstream consumer's document-set ingestion.

## Acceptance
- [x] New `web.crawl` op (`WebCrawlTool`, `crates/flux-web/src/crawl.rs`) registered in
      `register_web()` beside `web_fetch`; params `url`, `max_pages` (default 10, ceiling 50),
      `max_depth` (default 2, ceiling 5), same-host by default. New module consts cap every axis
      (`MAX_PAGE_BYTES`, `MAX_PAGE_RENDER_BYTES`, `MAX_TOTAL_RENDER_BYTES`, `MAX_FRONTIER` 512,
      timeouts) — no unbounded fan-out.
- [x] Every hop — seed, each discovered link, and every redirect — goes through `guard_url_scoped`
      + `egress::send_guarded`; private admits emit `EgressAudit` as `web:web.crawl`; a refused seed
      errors, a refused/failed discovered page is skipped (not fatal). Failing-first test
      `private_seed_refused_without_grant_but_ok_with_grant`.
- [x] Outlink extraction as data: new `pub(crate) fn condense::extract_links(html, base)` resolves
      `<a href>` against the page URL, drops fragment-only/non-http(s), de-dups preserving order.
      Frontier is de-duplicated (visited set) and host-scoped. Failing-first test
      `off_host_links_are_not_followed` + `extract_links` unit test.
- [x] End-to-end failing-first test `bfs_stops_at_depth_and_page_caps` against a local multi-path
      `site_server`: a seed→/a→/b chain crawled at `max_depth=1`/`max_pages=2` fetches exactly the
      seed + one linked page and stops at the cap.
- [x] `docs/designs/web-capabilities.md` Non-goals updated: crawling moves from non-goal to a
      shipped, bounded, same-host primitive; the caps + remaining non-goals are recorded.
- [x] Op-catalog docs updated: `website/docs/language/ops.md` (public catalog — the
      `website_contract` test requires it), `crates/flux-flow/docs/ops-reference.md`, and the
      flux-flow engine skill's registered-ops table.

## Progress
- 2026-07-11 — IMPLEMENTED (flux-web). `WebCrawlTool` (`crates/flux-web/src/crawl.rs`, new) does a
  bounded, same-host breadth-first crawl over the same egress envelope as `web_fetch`; link
  extraction via new `condense::extract_links`; registered in `register_web()`
  (`crates/flux-web/src/lib.rs`). 3 crawl tests + the `extract_links` unit test, all failing-first
  confirmed (execute stubbed → all three failed; restored → pass). Scoped gate green:
  `cargo test -p codewandler-flux-web` (44), `cargo clippy … -D warnings`, `cargo fmt` — all clean.
  Op-catalog docs (website ops.md, ops-reference.md, engine skill) updated for the new op.

## Notes
- Reuse map: SSRF `flux_system::net::{guard_url_scoped, PrivateNetAllow, host_resolves_private}`
  (`crates/flux-system/src/net.rs`); redirect-safe egress `egress::{send_guarded, read_body_capped,
  GuardedRequest}` (`crates/flux-web/src/egress.rs`); HTML parse/condense `condense::html_to_markdown`
  (`crates/flux-web/src/condense.rs`); wiring `WebOptions`/`EgressAudit`/`RecordSink`
  (`crates/flux-web/src/lib.rs:41-67`).
- Explicit v1 non-goals (document in the op description): robots.txt, sitemaps, cross-host crawl,
  JS-rendered crawl (that is the Tier-3 `browser.*` path, not this). Keep it same-host + bounded.
- The op lands in flux-web, which is not yet in the crates.io publish closure — see C-51.
