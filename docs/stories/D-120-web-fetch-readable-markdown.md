---
id: D-120
title: web.fetch reads pages as condensed markdown — flux-web condenser + unified fetch egress
pillar: Agent
status: done
priority: 18
epic: web-capabilities
design: docs/designs/web-capabilities.md
note: "tier 2, native: flux-web::condense (html5ever family in flux-web, NOT flux-markdown) → readable extraction → flux-markdown AST → markdown; web_fetch moves into flux-web, returns markdown for text/html, and cuts over to the `[private_net] web` scope (per-tool special case DELETED); pure op html_to_markdown; web.page datasource records; needs D-98"
---

# web.fetch reads pages as condensed markdown — flux-web condenser + unified fetch egress

## Goal
"Read this page" returns a *document*, not markup: condensed markdown with boilerplate stripped,
capped after condensation so the budget buys content. Native and zero-install. Tier 2 of
[web-capabilities](../designs/web-capabilities.md): documents → `web.fetch`. Completes the
security half of the original D-98: the per-tool `web_fetch` private-net special case is deleted
in favor of the family-wide `web` scope.

## Acceptance
- [x] Condenser core `flux-web::condense`: `html_to_markdown(html)` parsing via the html5ever family
      (`scraper`; the dep lands in flux-web — flux-markdown stays a pure markdown engine, consumed for
      its AST + writer), readability-style extraction (main-content region by text length, boilerplate
      dropped). Tests: a well-formed article page AND a div-soup page condense to markdown; a table
      becomes a pipe table; empty input → empty output.
- [x] `web_fetch` moved from `flux-capabilities::browser` into `flux-web::fetch` and upgraded in
      place: `text/html` responses (content-type + `looks_like_html` sniff) return condensed markdown;
      non-HTML stays raw; `raw: true` escape hatch; the byte cap applies to the *condensed* output
      (`cap_str`, char-boundary safe). `flux-capabilities/src/browser.rs` deleted; the crate's now-unused
      `reqwest` dep dropped.
- [x] Clean cutover to the D-98 `web` scope: `effective_web_fetch_private_hosts` (flux-cli) and the
      `[private_net] web_fetch` key + `web_fetch_private_hosts()` accessor (flux-config) **deleted**;
      config reference + D-96 note updated; `WHATS-NEW` Action-needed added. Test
      `private_refused_without_grant_but_admitted_and_audited_with_one`: refused without a `web` grant,
      admitted with one, audited `PrivateNetAdmit` `caller: "web:web_fetch"`.
- [x] Pure op `html_to_markdown` registered (no egress, `effects: []`; composes with `http.request`) —
      test `pure_html_to_markdown_op_condenses_without_egress`.
- [x] Fetched HTML pages contribute `web.page` datasource records (title/url/content) via a light
      `flux_web::RecordSink` seam, adapted in flux-cli to the workspace doc-index backend
      (`BackendRecordSink`). Test `html_is_returned_as_condensed_markdown_and_recorded`.
- [x] Dep-weight recorded: `scraper 0.23` pulls the html5ever family — `html5ever 0.29`,
      `markup5ever 0.14`, `cssparser 0.34`, `selectors 0.26`, `ego-tree 0.10`, `string_cache`,
      `tendril` — all pure Rust and modest. `cargo build --workspace` + `cargo test --workspace` +
      `flux-codegate` all green.

## Progress
- 2026-07-09 — **DONE.** `flux-web::condense` (scraper→flux-markdown AST→writer) + `flux-web::fetch`
  (`WebFetchTool` markdown upgrade + `HtmlToMarkdownTool`); `register_web` registers all three; the
  `web_fetch` per-tool egress path deleted end-to-end (flux-cli helper + flux-config field/accessor +
  merge + tests migrated to `web`); `flux-capabilities::browser` retired; `web.page` records wired via
  `RecordSink`/`BackendRecordSink`. 15 flux-web tests green; ops-reference/skill/config docs +
  CHANGELOG + WHATS-NEW updated.
- 2026-07-09 — **Re-scoped native** (user call): condenser home moves from a feature-gated
  flux-markdown `html` module to `flux-web::condense`; `web_fetch` moves into flux-web; the
  "public-only web_fetch" framing is superseded by the family-wide `web` scope (there is no
  plugin path anymore — private fetching is a `web` grant).
- 2026-07-09 — Filed (split out of D-98 into the web-capabilities epic).

## Notes
- `web_search` and the `websearch` plugin are untouched (epic non-goal).
- Needs [D-98](D-98-flux-web-crate-and-http-request-op.md) (the crate + the `web` scope).
