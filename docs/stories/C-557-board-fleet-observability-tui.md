---
id: C-557
title: "Polished TUI views expose worker channels, board decisions and exact stats"
pillar: Core
status: backlog
epic: board-fleet-tui
design: docs/designs/board-fleet-tui.md
dependencies: [C-556]
areas: [flux-tui, flux-cli, flux-capabilities]
depends_on: [C-556, C-570, C-571, C-573]
note: "follow-up UI — read-only peeks and native visualizations, not embedded CLI output"
---

# Polished TUI views expose worker channels, board decisions and exact stats

## Goal

Turn durable fleet and board projections into useful native views for supervision and diagnosis.

## Acceptance

- [ ] A worker peek correlates channel/activity, bounded logs, assignment, worktree, handoff, review
      and rework without granting an extra write path; absent evidence is labelled unavailable.
- [ ] Loop binding, acknowledged progress/yield and hierarchical budget usage are projected from
      C-569/C-570/C-571 typed state; the UI does not infer them from output or recalculate totals.
- [ ] C-573 policy, pins and last adaptation reasons are visible with metric freshness and
      reported/estimated/unsupported labels; the view itself owns no control policy.
- [ ] Board views cover ready/in-progress/blocked work, dependency graph, vision/roadmap/design links
      and open/decided/superseded decisions.
- [ ] Statistics render the exact `flux.board-stats/v1` cube with compact ratios and history trends;
      no UI-side progress calculation can drift from CLI JSON.
- [ ] Conflict/red-gate views preserve exact candidate and evidence and link back to the responsible
      story/session.
- [ ] Large boards/workers stay bounded and responsive; snapshots cover empty, active, attention and
      failure states in narrow and wide terminals.
- [ ] CLI human rendering remains compact and independent; the TUI uses typed state, not ANSI capture.

## Progress

- 2026-08-05 — reconciled under C-582 and made explicitly dependent on C-556's attachment and typed
  projection-source lifecycle.

## Notes

- Depends on C-556's main shell and the settled progress/budget projections.
