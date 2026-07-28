---
id: A-94
title: Mid-turn steering — queue user guidance into a running turn
pillar: Agent
status: backlog
priority:
epic:
design:
note: "a running turn is take-it-or-Ctrl-C today; let the user type while the agent executes — the message queues and injects at the next planner consultation as a steering block, without cancelling in-flight ops or losing the turn; the multipass loop already re-consults, so the seam exists"
---

# Mid-turn steering — queue user guidance into a running turn

## Goal
Let the user talk to the agent while it runs: input typed during execution queues and injects at
the next planner consultation as a clearly-attributed steering block ("stop touching the tests,
focus on the parser"), without cancelling in-flight ops or losing the turn. Today the only options
are wait or Ctrl-C.

## Acceptance
- [ ] A steering message submitted mid-turn is injected at the next planner consultation, visibly
  attributed as mid-turn user guidance, and persists in the session log — failing-first test
  driving a mock-provider multipass turn.
- [ ] In-flight ops are never cancelled or re-fired by steering; approvals pending at injection
  time are unaffected — behavior-lock test.
- [ ] TUI: the composer stays live during execution with a "queued" indicator; queued messages are
  editable/retractable until consumed.
- [ ] Plain-CLI: a documented equivalent (or an explicit statement that steering is TUI-only in v1).
- [ ] Multiple queued messages inject in order at one consultation.

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

## Notes
- Seam: the agent loop's planner consultation point (multipass loop re-consults each pass) +
  `flux-tui` composer state.
- Interaction with compaction and the typed session log (A-93) worth a design note before build.
