---
id: D-159
title: Datasource recipe documentation — register_pack + flux-capabilities walkthrough
pillar: Agent
status: backlog
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
- [ ] Website SDK docs gain the walkthrough (compilable, doc-test-style snippet).
- [ ] The design doc's out-of-scope rationale is linked from the page.
- [ ] D-62 cross-referenced from the page and from this story.

## Progress
- (pending)

## Notes
- No code changes. If D-62 lands first, convert this story into the first-class
  `with_datasource(...)` API story instead.
