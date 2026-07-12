---
id: D-159
title: Datasource recipe documentation — register_pack + flux-capabilities walkthrough
pillar: Agent
status: done
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 4, docs-only — deliberate non-API until D-62 lands the async paged seam"
---

# Datasource recipe documentation

## Goal
Document (not API-ify) how an embedder attaches datasources today:
`FlowClient::register_pack(|r| register_datasource_ops(r, backend))` with a direct
`flux-capabilities` dep — with an explicit caveat that the first-class SDK datasource surface
waits for D-62 (the async paged live-backend seam) so we don't freeze the wrong contract.

## Acceptance
- [x] Website SDK docs gain the walkthrough (compilable, doc-test-style snippet).
- [x] The design doc's out-of-scope rationale is linked from the page.
- [x] D-62 cross-referenced from the page and from this story.

## Progress
- **Done (unreleased).** New website page `website/docs/sdk/datasources.md` (added to the SDK
  sidebar) documents the recipe: a direct `codewandler-flux-capabilities` dep +
  `register_pack(|r| register_datasource_ops(r, backend))`. The snippet is backed by a **gate-verified**
  example `crates/flux-sdk/examples/datasource_recipe.rs` (compiles + runs; `flux-capabilities` added
  as a `flux-sdk` **dev-dependency only** — no runtime dep, keeping the SDK datasource-free).
- The page links the sdk-surface design's "Out of scope" rationale
  (`docs/designs/sdk-surface.md`, via GitHub) and cross-references **D-62** (the async paged
  live-backend seam) as the gate for a first-class `with_datasource(...)` API — both from the page
  and here. If D-62 lands, convert this into the first-class API story.
- No `flux-sdk` code/behavior change. CHANGELOG updated (docs); no WHATS-NEW (documents an existing
  capability). Gate green (workspace 2171; example compiles under clippy `--all-targets` / fmt /
  codegate; website_in_sync unaffected). **This completes the sdk-surface epic (D-142…D-159).**

## Notes
- No code changes. If D-62 lands first, convert this story into the first-class
  `with_datasource(...)` API story instead.
