---
id: C-357
title: Record bus factor and independent review as owned risks with a review date
pillar: Core
status: backlog
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "one administrator and 26 merged PRs but zero recorded reviews; no succession or incident-exercise evidence. Independent-review governance remains a visible residual, not a v0.56.0 publication blocker"
---

# Record bus factor and independent review as owned risks

## Goal

Stop carrying a governance risk as an open review finding that gets rediscovered every pass. Own it,
date it, and state what would change it.

## Acceptance

- [ ] A risk entry exists (roadmap or a dedicated register) covering: single-maintainer commit and
      admin concentration, no second-party review, no succession or release-authority document, no
      incident exercise, and no external audit.
- [ ] Each entry names the evidence that would retire it and a review date.
- [ ] The security documentation states the project's security-response expectation, or states
      explicitly that none is committed — three reviews flagged its absence.
- [ ] The entry distinguishes what is repository-verifiable (author distribution) from what needed
      platform queries (protection, environments, collaborators) and records how to re-derive both.

## Progress

- 2026-08-01 — filed from validation of ASSURE-04. The "external-unknown" half resolved to verified
  absent during this pass; the risk itself remains open by nature.
- 2026-08-04 — revalidated at canonical
  `9e3108b1b6856e30fa2e0baa2475d75d21fbc19f`: 26 PRs are merged, zero reviews are recorded and one
  administrator remains. The story stays `backlog` with every acceptance box open. This is an
  independent-review/governance residual, not a v0.56.0 publication blocker; C-353 may remove
  bypasses without pretending a second reviewer exists.

## Notes

- C-255 already recorded bus factor as a residual governance risk rather than a fictional code
  story. This gives it an owner and a date instead of a footnote.
- A green protected release is not evidence of independent review and does not close this story.
