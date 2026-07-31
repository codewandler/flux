---
id: C-357
title: Record bus factor and independent review as owned risks with a review date
pillar: Core
status: backlog
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "486 of the last 500 commits from one identity, one admin collaborator, zero merged PRs ever, no release environment; three reviews rated this a material risk and no code change can close it"
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

## Notes

- C-255 already recorded bus factor as a residual governance risk rather than a fictional code
  story. This gives it an owner and a date instead of a footnote.
