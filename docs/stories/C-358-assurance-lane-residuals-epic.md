---
id: C-358
title: "Assurance lane residuals — a declared lane that has never run is not a lane (epic)"
pillar: Core
status: backlog
epic: assurance-lane-residuals
design: docs/designs/assurance-lane-residuals.md
note: "EPIC — ASSURE-01 split lane by lane: SAST/Miri/attestation are historical-fixed, fuzzing and sanitizers are still absent, and Miri + corpus-deep + the weekly dependency audit have NEVER executed (all runs are event=push)"
---

# Assurance lane residuals

## Goal

Turn the assurance surface from declared to demonstrated, one lane at a time, without letting any
single addition close the compound claim.

## Acceptance

- [ ] C-359 triages the open `critical` CodeQL alerts and makes the lane gate.
- [ ] C-360 proves Miri, `corpus-deep` and the weekly dependency audit actually execute.
- [ ] C-361 adds coverage-guided fuzzing with a persistent corpus.
- [ ] C-362 adds a sanitizer lane.
- [ ] Each lane reports a real run — id, duration, finding count — in its story before it closes.
- [ ] The changelog stops describing the deterministic adversarial corpus in terms that imply
      coverage-guided fuzzing.

## Progress

- 2026-08-01 — opened from the lane-by-lane table built during validation.

## Notes

- The corpus smoke lane is genuinely non-vacuous: its self-test proves disabled and comment-only
  decoys are rejected. That is the bar for the new lanes.
- These lanes are advisory until C-353 protects `main`.
