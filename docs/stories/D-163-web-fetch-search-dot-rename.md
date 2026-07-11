---
id: D-163
title: Rename web_fetch/web_search to web.fetch/web.search — op-family naming consistency
pillar: Agent
status: ready
priority: 10
design: ../designs/web-capabilities.md
epic: web-capabilities
areas: [flux-web]
note: "surfaced by an op-naming convention audit (2026-07-11): web_fetch/web_search are the only snake_case ops left in the web family — http.request, web.crawl, and browser.* are all dot-namespaced, and docs/designs/web-capabilities.md already calls the op web.fetch (lines 28, 96) while the shipped code/other docs still say web_fetch"
---

# Rename web_fetch/web_search to web.fetch/web.search — op-family naming consistency

## Goal
Bring `web_fetch` and `web_search` in line with the rest of their own op family
(`http.request`, `web.crawl`, `browser.open/goto/snapshot/act/close`) by renaming them to
`web.fetch`/`web.search`. Do this before C-51 (flux-web → crates.io publish closure) lands, so the
rename is a pre-publish cleanup rather than a breaking change to a crates.io-published op.

## Acceptance
- [ ] `web_fetch` → `web.fetch` in `crates/flux-web/src/fetch.rs` (the `ToolSpec` name, and the
      internal audit-label strings `web:web_fetch` used at the egress call sites).
- [ ] `web_search` → `web.search` in `crates/flux-tools/src/extra.rs`.
- [ ] All string-literal references in tests/fixtures across `flux-web`, `flux-tools`, `flux-sdk`
      updated (`grep -rn '"web_fetch"\|"web_search"'` clean).
- [ ] `crates/flux-flow/docs/ops-reference.md` and `website/docs/language/ops.md` rows updated;
      `docs/designs/web-capabilities.md` already says `web.fetch` — verify it now matches the code
      instead of the other way around.
- [ ] CHANGELOG/WHATS-NEW entry noting the breaking rename (pre-1.0 → ships as a MINOR per the
      flux SemVer rule).

## Progress
- 2026-07-11 — Filed from an op-naming consistency audit across core + all 20 plugins. Full
  findings: web is the only op family mixing snake_case (`web_fetch`, `web_search`) with dot
  namespace (`http.request`, `web.crawl`, `browser.*`); every one of the 13 real integration
  plugins uses `<plugin>.<resource>.<action>` with no exceptions found.

## Notes
- Sequencing: land before or together with C-51 (flux-web crates.io publish) — renaming after
  publish would make this a real breaking change for external consumers instead of an in-tree one.
- See also C-52 (small op-naming/doc cleanups from the same audit) for unrelated minor findings
  that don't need to block on this one.
