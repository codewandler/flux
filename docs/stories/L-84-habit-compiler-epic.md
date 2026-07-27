---
id: L-84
title: "The habit compiler — an automation ratchet from session history to authored Flux (epic)"
pillar: Language
status: backlog
epic: habit-compiler
design:
note: "EPIC — mine recurring plan shapes across sessions into authored composite ops; report how much work migrated from model tokens to the deterministic runtime"
---

# The habit compiler — an automation ratchet from session history to authored Flux (epic)

## Goal
Mine the session history for recurring plan shapes ("every session in this repo starts
fmt→clippy→test") and offer to compile them into authored `.flux` composite ops, so repeated work
migrates from model tokens to the deterministic runtime. Then report the ratchet: "38% of your
turns last week executed fully deterministically." L-06 lets an agent register a composite
in-session and `docs/designs/plan-corpus-and-small-model.md` trains a planner; neither learns
*across* sessions from the corpus you're already storing.

## Acceptance
- [ ] A design doc (`docs/designs/habit-compiler.md`) covering: plan-shape mining over the stored
      `plan_source` corpus (D-53/L-38), the propose-and-ratify flow into `.flux` composite ops
      (L-04/L-06 scopes), and the deterministic-share ratchet metric.
- [ ] The epic is broken into implementation stories on the board; each behavioral change ships
      with a failing-first test.
- [ ] Headline proof: a recurring plan shape across sessions yields a proposed composite op that,
      once accepted, executes the habit deterministically — and the ratchet report quantifies the
      migrated share.

## Progress
- (not started — epic filed from the 2026-07-28 out-of-the-box ideas session)

## Notes
- Raw material already ships: every accepted plan since v0.2.15 carries parseable `plan_source`
  (L-38), and D-53 exports it as a corpus.
- Related but in-session only: L-04 (composite ops), L-06 (agent-registered composites).
