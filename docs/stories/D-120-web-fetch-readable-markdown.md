---
id: D-120
title: web.fetch reads pages as condensed markdown — flux-web condenser + unified fetch egress
pillar: Agent
status: ready
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
- [ ] Condenser core `flux-web::condense`: `html_to_markdown(html, opts)` parsing via the
      html5ever family (dep lands in flux-web — flux-markdown stays a pure markdown engine,
      consumed for AST + writer), readability-style extraction (drop nav/script/boilerplate).
      Failing-first: golden fixture pages (a well-formed article AND a div-soup page) → expected
      markdown.
- [ ] `web_fetch` moves from `flux-capabilities::browser` into flux-web and upgrades in place:
      `text/html` responses (content-type + sniff) return condensed markdown; non-HTML stays raw;
      `raw: true` escape hatch; cap applies to the *condensed* output (test pins `len <= cap` —
      the A-24 lesson). `flux-capabilities/src/browser.rs` retires.
- [ ] Clean cutover to the D-98 `web` scope: `effective_web_fetch_private_hosts`
      (`flux-cli/src/main.rs:5497`) and the `[private_net] web_fetch` key are **deleted**
      (flux-cli + flux-config); D-96's docs/caveat updated. Test: a private-range URL through
      `web_fetch` is refused without a `web` grant and admitted with one (`PrivateNetAdmit`
      `caller: "web:web_fetch"`).
- [ ] Pure op `html_to_markdown` registered (no egress; composes in flux-lang with
      `http.request`).
- [ ] Fetched pages contribute `web.page` datasource records (title/url/content — the `websearch`
      → `web.result` pattern).
- [ ] Dep-weight check recorded: gate + `flux-codegate` accept the html5ever-family dep in the
      root workspace (build-time impact measured and noted in this story).

## Progress
- 2026-07-09 — **Re-scoped native** (user call): condenser home moves from a feature-gated
  flux-markdown `html` module to `flux-web::condense`; `web_fetch` moves into flux-web; the
  "public-only web_fetch" framing is superseded by the family-wide `web` scope (there is no
  plugin path anymore — private fetching is a `web` grant).
- 2026-07-09 — Filed (split out of D-98 into the web-capabilities epic).

## Notes
- `web_search` and the `websearch` plugin are untouched (epic non-goal).
- Needs [D-98](D-98-flux-web-crate-and-http-request-op.md) (the crate + the `web` scope).
