---
id: C-359
title: Triage the open critical CodeQL alerts and make the lane gate
pillar: Core
status: backlog
epic: assurance-lane-residuals
design: docs/designs/assurance-lane-residuals.md
note: "13 open critical rust/hard-coded-cryptographic-value alerts (10x flux-plugin/src/host.rs, 2x rooms/xmpp/mod.rs, 1x plugins/sql/src/main.rs), untriaged, on a job that succeeds regardless of findings"
---

# Triage the open critical CodeQL alerts and make the lane gate

## Goal

An open `critical` on a non-blocking lane trains everyone to ignore the lane. Resolve the backlog,
then make new findings fail.

## Acceptance

- [ ] Each of the 13 open `critical` alerts is fixed or dismissed with a recorded justification;
      no alert is left open and unexplained.
- [ ] The `codeql-rust` job fails on new `critical`/`high` findings rather than succeeding
      regardless.
- [ ] A severity threshold and dismissal policy is written down, so the next batch is triaged
      against a rule rather than a judgement call.

## Progress

- 2026-08-01 — alert inventory read live during validation (last analysis 2026-07-31 21:55,
  results_count 13).

## Notes

- All 13 are the same query. Expect most to be false positives on protocol constants — that is a
  reason to dismiss with a reason, not a reason to leave them open.
