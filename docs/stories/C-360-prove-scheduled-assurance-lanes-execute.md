---
id: C-360
title: Prove every scheduled assurance lane actually executes
pillar: Core
status: backlog
epic: assurance-lane-residuals
design: docs/designs/assurance-lane-residuals.md
note: "Miri and corpus-deep have never run (0 schedule/dispatch events); security-audit.yml's weekly cron has never fired across 43 runs, all event=push. Three declared lanes, zero executions"
---

# Prove every scheduled assurance lane actually executes

## Goal

Establish proof-of-life before the first cron fires, so a dead-on-arrival lane is discovered by us
rather than by the next reviewer.

## Acceptance

- [ ] Miri, `corpus-deep`, and the weekly `security-audit` lanes each record a completed run with a
      run id, duration and finding count.
- [ ] Miri closes on a completed **scheduled** run, not only a `workflow_dispatch` — a dispatch
      proves the job works, not that the schedule does.
- [ ] Any lane that fails on its first real execution gets its failure recorded here rather than
      silently disabled.
- [ ] The Miri lane's scope (two pure seams: plugin NDJSON frames and the flux-lang lexer) is stated
      in the workflow so its narrowness is not mistaken for coverage.

## Progress

- 2026-08-01 — run history read live during validation.

## Notes

- The first scheduled window after filing is 2026-08-05 for `corpus-deep`.
