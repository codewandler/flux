---
id: C-582
title: "The operations TUI makes Fleet main, workers and Boards one supervisable surface (epic)"
pillar: Core
status: done
epic: board-fleet-tui
design: docs/designs/board-fleet-tui.md
areas: [flux-tui, flux-cli, flux-orchestrate, flux-capabilities]
note: "Decision 0010 follow-up epic — explicit Fleet-main attachment, coordinator-first conversation and bounded typed Board/Fleet views"
---

# The operations TUI makes Fleet main, workers and Boards one supervisable surface

## Goal

Let an operator launch one honest `flux tui` surface that either says it is standalone or explicitly
attaches to the durable Fleet main coordinator, then supervise the Board, schedule, decisions,
workers and evidence without parsing CLI output or watching tmux.

## Acceptance

- [x] C-556 ships explicit Fleet-main attachment, a coordinator-first conversation and the
      responsive attention rail while preserving standalone `flux tui` behavior.
- [x] C-557 ships bounded native Board, worker, decision, failure and statistics views over the same
      typed projections used by the CLI.
- [x] Every view reconstructs from durable Board/Fleet/session state, labels unavailable data
      honestly and performs no terminal, tmux, ANSI or subprocess scraping.
- [x] Only explicit conversation intake and decision controls may mutate state; observation never
      gains push, release, deploy, hidden Board mutation or worktree-cleanup authority.
- [x] Public operator documentation explains standalone versus attached launch, navigation,
      acknowledgement states and recovery after restart.
- [x] Narrow/wide, empty/active/attention/failure snapshots and interaction tests protect the
      experience, and the repository gate is green.

## Progress

- 2026-08-05 — promoted from the separately contracted C-556/C-557 follow-up stories at the user's
  request. The epic owns only the native `flux tui` surface; generic external harness adapters and
  remote Fleet membership remain separate.
- 2026-08-05 — completed with explicit Fleet-main attachment, durable acknowledgement and bounded
  native operations views. Public TUI, Board and Fleet guides document the launch, keyboard flow,
  recovery and authority boundary; focused and full crate gates pass.

## Notes

- Canonical design: [board-fleet-tui.md](../designs/board-fleet-tui.md).
- Delivery order is C-556 followed by C-557 because the typed attachment shell owns the projection
  source and lifecycle that every detailed view consumes.
