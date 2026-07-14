---
id: C-70
title: Consolidate web search behind one guarded implementation
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: native and plugin Tavily paths duplicate transport/auth while only the plugin provides DuckDuckGo fallback
---

# Consolidate web search behind one guarded implementation

## Goal

Own Tavily/DuckDuckGo search, secret resolution, redirects, result shaping, and datasource
contribution in one guarded implementation while preserving a clear compatibility path for
`web.search` users.

## Acceptance

- [x] A documented ownership decision selects the native `flux-web` or first-party plugin seam and
      removes the duplicate Tavily request implementation.
- [x] No model-facing schema accepts an API key and no search tool reads `TAVILY_API_KEY` directly;
      secrets resolve through the host redactor/secret capability and never enter logs/results.
- [x] All search egress uses the canonical DNS-aware, redirect-safe guard and scoped private-network
      policy from C-59/C-62; failing-first tests cover redirects and secret-header containment.
- [x] Tavily behavior and the DuckDuckGo no-key fallback are retained, or a user-visible migration
      explicitly replaces them with equivalent provider selection.
- [x] Existing `web.search` call sites receive a compatibility alias/deprecation window where needed;
      operation catalogs, groups, generated docs, and skills expose only one default search op.
- [x] The redundant HTTP dependency leaves `flux-tools` when no longer needed, and result/datasource
      contract tests prove no silent regression.

## Progress

- 2026-07-14 — Made the first-party websearch plugin the sole `web.search` owner, removed the native
  Tavily client and `flux-tools` HTTP dependency, and retained Tavily plus DuckDuckGo fallback.
  Manifest, secret-containment, backend, schema, result, and host-egress tests cover the cutover.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Sequence after C-62; coordinate plugin dependency choices with C-69.
- Primary implementation: `plugins/websearch/src/main.rs`; the retired native path lived in
  `crates/flux-tools/src/extra.rs`.
