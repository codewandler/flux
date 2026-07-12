---
id: D-163
title: Rename web_fetch/web_search to web.fetch/web.search — op-family naming consistency
pillar: Agent
status: done
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
- [x] `web_fetch` → `web.fetch` in `crates/flux-web/src/fetch.rs` (the `ToolSpec` name, and the
      internal audit-label strings `web:web_fetch` used at the egress call sites).
- [x] `web_search` → `web.search` in `crates/flux-tools/src/extra.rs`.
- [x] All string-literal references in tests/fixtures across `flux-web`, `flux-tools`, `flux-sdk`
      updated (`grep -rn '"web_fetch"\|"web_search"'` clean).
- [x] `crates/flux-flow/docs/ops-reference.md` and `website/docs/language/ops.md` rows updated;
      `docs/designs/web-capabilities.md` already says `web.fetch` — verify it now matches the code
      instead of the other way around.
- [x] CHANGELOG/WHATS-NEW entry noting the breaking rename (pre-1.0 → ships as a MINOR per the
      flux SemVer rule).

## Progress
- 2026-07-11 — Filed from an op-naming consistency audit across core + all 20 plugins. Full
  findings: web is the only op family mixing snake_case (`web_fetch`, `web_search`) with dot
  namespace (`http.request`, `web.crawl`, `browser.*`); every one of the 13 real integration
  plugins uses `<plugin>.<resource>.<action>` with no exceptions found.
- 2026-07-12 — IMPLEMENTED. Clean cutover, no alias. Renamed the `ToolSpec` names, egress `op`
  labels, error prefixes and the `web:web.fetch` private-admit audit label in `flux-web::fetch`;
  `web.search` in `flux-tools::extra`. Updated cross-crate matchers (`flux-tui` toolview arm,
  `flux-lsp` catalog assertion + example flow, `flux-tools` builtins-name test), the agent
  system-prompt safety line (`flux-agent`) and the `--allow-private-net` CLI warning. Docs: the
  `ops-reference.md` + website `ops.md` rows (the latter enforced by the `website_contract`
  operations-catalog test), the flux-flow SKILL ops row, website language-doc examples, the
  `web-capabilities` design (op refs → `web.fetch`, retired `effective_web_fetch_private_hosts` /
  `[private_net] web_fetch` **key** left intact), and the two scout agent `tools:` grants
  (`web_search` → `web.search`). CHANGELOG `[Unreleased] Changed` + WHATS-NEW `Action needed` added;
  website whats-new mirror regenerated. The retired `[private_net] web_fetch` config key (README,
  config.md, troubleshooting, WHATS-NEW migration notes) and all historical CHANGELOG/story entries
  were deliberately NOT rewritten. Gate: green.

## Notes
- Superseded sequencing note: C-51 already shipped (`codewandler-flux-web` is live on crates.io),
  so this is a real breaking rename for external consumers, not the in-tree-only cleanup originally
  envisioned — still ships as a MINOR per the flux SemVer rule.
- See also C-52 (small op-naming/doc cleanups from the same audit) for unrelated minor findings
  that don't need to block on this one.
