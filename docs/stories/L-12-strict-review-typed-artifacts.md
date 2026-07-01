---
id: L-12
title: Strict review — typed artifacts + deterministic aggregator (Phase 3)
pillar: Language
status: backlog
epic: strict-review-flows
design: docs/designs/strict-review-flows.md
note: ReviewRequest/ReviewFinding/ReviewReport + review.normalize/aggregate (fingerprint/dedupe/rank)
---

# Strict review — typed artifacts + deterministic aggregator (Phase 3)

## Goal

Give the protocol typed, reusable artifacts and a deterministic aggregator: `ReviewRequest`,
`ReviewFinding`, and `ReviewReport` (as schemas first, promoted to prelude types once multiple
surfaces consume them) plus `review.normalize` / `review.aggregate` composite ops that parse reviewer
output, quarantine malformed findings as gaps, fingerprint by category/file/line/normalized-title,
deduplicate, and rank by severity/confidence/agreement with stable ordering. Serves the Language
pillar: aggregation is deterministic runtime, and the model is used only for final prose synthesis
against a fixed schema — never to decide which tools to run or reviewers to spawn.

Full design: [docs/designs/strict-review-flows.md](../designs/strict-review-flows.md) — Phase 3 &
"Aggregation".

## Acceptance

- [ ] **Failing-first test:** given fixed reviewer outputs, `review.aggregate` produces a report with
  **stable ordering** across runs, and a malformed reviewer output is reported as a **gap** (not
  silently accepted) — added red, then green.
- [ ] `ReviewRequest`/`ReviewFinding`/`ReviewReport` exist as schemas (embedded first; prelude-type
  promotion tracked when a second surface consumes them).
- [ ] `review.normalize`/`review.aggregate` implemented as deterministic composite ops (native Rust
  only if fingerprinting/ranking needs a stable built-in).
- [ ] Duplicate findings collapse by fingerprint; ranking is by severity, then confidence, then
  reviewer agreement.
- [ ] `strict_review` (L-10) is migrated to emit a typed `ReviewReport`.
- [ ] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.
- [ ] CHANGELOG entry.

## Notes
- Open question to settle: preserve reviewer disagreement as separate findings, or merge with an
  agreement count.
- Depends on [L-10](L-10-strict-review-example-flow.md); consumed by
  [L-13](L-13-strict-review-journey-cli.md).
