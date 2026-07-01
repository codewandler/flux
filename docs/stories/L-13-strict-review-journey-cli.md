---
id: L-13
title: Strict review — app journey + flux review CLI & CI surfaces (Phase 4)
pillar: Agent
status: backlog
epic: strict-review-flows
design: docs/designs/strict-review-flows.md
note: flux-app review_code journey + optional flux review command + CI output modes
---

# Strict review — app journey + flux review CLI & CI surfaces (Phase 4)

## Goal

Make the strict-review flow a product surface: a `flux-app` `review_code` journey (owns trigger +
input mapping) that runs `strict_review`, an optional `flux review --files …` convenience command,
and CI-friendly output modes (markdown, JSON, nonzero exit on high severity). The journey owns app
routing; the flow owns execution semantics — keeping app plumbing separate from review correctness.
Serves the Agent pillar: an app-level entrypoint that wakes a governed protocol on command/event.

Full design: [docs/designs/strict-review-flows.md](../designs/strict-review-flows.md) — Phase 4 &
"Journey integration".

## Acceptance

- [ ] **Failing-first test:** the journey path and the direct flow path produce the **same**
  `ReviewReport` for the same inputs — added red, then green.
- [ ] A `flux-app` `review_code(input)` journey runs `strict_review(files, diff, reviewers?)`.
- [ ] Optional `flux review --files …` invokes the same flow through the safety envelope.
- [ ] CI output modes: markdown, JSON, and a nonzero exit when a finding meets a configurable
  severity threshold.
- [ ] Write/network/report-publishing effects stay outside the strict-review core and require
  explicit approval (per the design's security considerations).
- [ ] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.
- [ ] CHANGELOG entry.

## Notes
- Depends on [L-10](L-10-strict-review-example-flow.md) (flow) and
  [L-12](L-12-strict-review-typed-artifacts.md) (typed `ReviewReport`); best after
  [L-11](L-11-strict-review-scoped-capabilities.md) so the served protocol is enforced, not advisory.
- Open question to settle: is strict review a built-in sample, a project template, or a first-class
  CLI command (this story picks CLI + journey).
