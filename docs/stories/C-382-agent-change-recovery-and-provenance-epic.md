---
id: C-382
title: "Agent change recovery and provenance — know what you changed, and be able to take it back (epic)"
pillar: Agent
status: backlog
epic: agent-change-recovery-and-provenance
design: docs/designs/agent-change-recovery-and-provenance.md
note: "EPIC — 15 git ops and no history-preserving undo; write receipts are pre-dispatch and content-blind; every read-only git observer is refused the evidence phase; the transcript datasource is built and reachable from no shipped assembly"
---

# Agent change recovery and provenance

## Goal

Let the harness prove which changes are its own, recover from its own mistaken commit without losing
the patch, and stop reporting states it cannot observe.

## Acceptance

- [ ] C-383 adds a history-preserving uncommit that fails closed on every ambiguity.
- [ ] C-384 records success-time, content-anchored write receipts.
- [ ] C-385 lets staging target only receipt-owned changes.
- [ ] C-386 resolves — deliberately — how read-only Git observers reach the evidence phase.
- [ ] C-387 wires the harness-history datasource into a shipped assembly behind a config key.
- [ ] C-388 makes flux-native history honest about what it cannot read.
- [ ] C-389 makes pane results state acceptance rather than visibility.
- [ ] C-390 collapses the timed-pane authoring pattern in docs and in the shipped demo flow.

## Progress

- 2026-08-01 — opened from validation of GIT-01/02/03, HAR-04, HAR-06 and LANG-01.

## Notes

- LANG-01 did **not** validate as a language gap: the collapsed `each` + nested `loop for …, every:`
  form parses, lowers and executes correctly today — this was run during the validation pass. Per
  H's explicit constraint, no `pane.sequence` op is proposed anywhere in this epic.
