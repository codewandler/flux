---
id: D-120
title: web.fetch reads pages as condensed markdown — condenser core + public-only native fetch
pillar: Agent
status: ready
priority: 18
epic: web-capabilities
design: docs/designs/web-capabilities.md
note: "tier 2: HTML → readable extraction → flux-markdown AST → markdown (feature-gated `html` module in flux-markdown); native web_fetch returns markdown for text/html and goes PUBLIC-ONLY (bespoke private-net path deleted — clean cutover); pure op html_to_markdown composes with http.request; web.page datasource records"
---

# web.fetch reads pages as condensed markdown — condenser core + public-only native fetch

## Goal
"Read this page" returns a *document*, not markup: condensed markdown with boilerplate stripped,
capped after condensation so the budget buys content. Stays native/zero-install. Tier 2 of
[web-capabilities](../designs/web-capabilities.md): documents → `web.fetch`. Completes the
security half of the original D-98: the native tool's bespoke private-net path is removed.

## Acceptance
- [ ] Condenser core: a feature-gated `html` module in `flux-markdown` —
      `html_to_markdown(html, opts)` parsing via the html5ever family, readability-style
      extraction (drop nav/script/boilerplate), emitting through the existing `flux-markdown` AST
      + writer. Failing-first: golden fixture pages (a well-formed article AND a div-soup page) →
      expected markdown.
- [ ] Native `web_fetch` upgrades in place: `text/html` responses (content-type + sniff) return
      condensed markdown; non-HTML stays raw; `raw: true` escape hatch; cap applies to the
      *condensed* output (test pins `len <= cap` — the A-24 lesson).
- [ ] Clean cutover, no fallback flag: `web_fetch` is public-only. `effective_web_fetch_private_hosts`
      and the `web_fetch` special-casing in `[private_net]` / `--allow-private-net` are **deleted**
      (flux-cli + flux-config); D-96's docs/caveat updated. Private fetching = the `web` plugin
      under a scoped grant (or `http.request → html_to_markdown` in a flow). Test: a private-range
      URL through `web_fetch` is refused even with `--allow-private-net`.
- [ ] Pure op `html_to_markdown` registered natively (no egress; composes in flux-lang with
      `http.request`).
- [ ] Fetched pages contribute `web.page` datasource records (title/url/content — the `websearch`
      → `web.result` pattern).
- [ ] Dep-weight check recorded: gate + `flux-codegate` accept the html5ever-family dep in the main
      workspace; if unacceptable, the documented fallback (condenser jails to the plugins
      workspace; native stays raw) re-scopes this story explicitly.

## Progress
- 2026-07-09 — Filed (split out of D-98 into the web-capabilities epic).

## Notes
- Home rationale: `flux-markdown` is L0 and already the markdown engine; L0 is the
  shared-with-plugins precedent (`flux-datasource`), and "prefer one crate + modules" holds.
- `web_search` and the `websearch` plugin are untouched (epic non-goal).
