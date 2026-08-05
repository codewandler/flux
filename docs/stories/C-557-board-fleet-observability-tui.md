---
id: C-557
title: "Polished TUI views expose worker channels, board decisions and exact stats"
pillar: Core
status: in-progress
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

- [x] A worker peek correlates channel/activity, bounded logs, assignment, worktree, handoff, review
      and rework without granting an extra write path; absent evidence is labelled unavailable.
- [ ] Loop binding, acknowledged progress/yield and hierarchical budget usage are projected from
      C-569/C-570/C-571 typed state; the UI does not infer them from output or recalculate totals.
- [ ] C-573 policy, pins and last adaptation reasons are visible with metric freshness and
      reported/estimated/unsupported labels; the view itself owns no control policy.
- [x] Board views cover ready/in-progress/blocked work, dependency graph, vision/roadmap/design links
      and open/decided/superseded decisions.
- [x] Statistics render the exact `flux.board-stats/v1` cube with compact ratios and history trends;
      no UI-side progress calculation can drift from CLI JSON.
- [x] Conflict/red-gate views preserve exact candidate and evidence and link back to the responsible
      story/session.
- [x] Large boards/workers stay bounded and responsive; snapshots cover empty, active, attention and
      failure states in narrow and wide terminals.
- [x] CLI human rendering remains compact and independent; the TUI uses typed state, not ANSI capture.

## Progress

- 2026-08-05 — reconciled under C-582 and made explicitly dependent on C-556's attachment and typed
  projection-source lifecycle.
- 2026-08-05 — shipped bounded native Overview, Board, Workers, Decisions and Stats views. The
  projection reports totals beyond display caps, preserves exact red-gate candidate/evidence,
  renders durable planning links and intake acknowledgements, and consumes the Board stats cube
  without UI-side progress arithmetic. Source and layout fixtures plus the complete crate suites
  pass.
- 2026-08-06 — moved initial and refreshed Board/Fleet projection builds off the terminal event
  loop after live roadmap scale exposed synchronous startup and refresh stalls. Attachment supplies
  a cheap truthful seed and durable-state token while one coalesced blocking worker builds the full
  view; loading, unavailable and stale states are explicit and a failed refresh preserves the last
  good projection. Regression coverage holds a source snapshot blocked while the async caller stays
  responsive and verifies startup-error versus later-stale behavior.

## Notes

- Depends on C-556's main shell and the settled progress/budget projections.
