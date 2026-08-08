# Design — The board and fleet operations TUI

**Status:** accepted for implementation · **Epic:**
[C-582](../stories/C-582-board-fleet-operations-tui-epic.md) · **Stories:**
[C-556](../stories/C-556-fleet-main-agent-tui-shell.md),
[C-557](../stories/C-557-board-fleet-observability-tui.md)

## Why

The CLI is the automation API, but a human supervising autopilot needs a calm view of the main
coordinator, worker channels, blocked decisions and progress. Reprinting compact CLI output in panes
would waste the interaction model a TUI can provide.

## Experience

The primary surface is a conversation with the fleet's one main coordinator. A side rail shows
workers, waves and attention-required decisions without competing for focus. Read-only peeks open a
worker's correlated activity/channel, exact handoff and bounded log. Board views show the current
queue, dependency graph, vision/roadmap/design links, open/decided decisions and the exact stats cube.

The TUI never invents state or bypasses the CLI/runtime contracts. Every action uses the same typed
board/fleet operations and permission checks; views are projections over durable state. A compact
layout remains usable in narrow terminals, while a wide layout uses hierarchy, status color and
small trend visualizations rather than raw JSON.

## Launch and attachment

Ordinary `flux tui` is a standalone agent chat and says so in its header. `flux tui --fleet` attaches
to the Fleet rooted at the current directory; `--fleet=PATH` selects another root explicitly. The
attached surface opens the reserved main coordinator's own durable event store and resumes the exact
session named by Fleet state. If no main session exists yet, the surface creates it and records that
identity before accepting a requirement. Missing configuration, malformed state and a stopped Fleet
are visible connection states, never implicit fallback to standalone chat.

The compositor sends an attached requirement through a typed in-process bridge. The bridge journals
`accepted` before the turn starts, `delivered` when the main session owns it and `completed` or
`failed` at the terminal turn. This is the same durable vocabulary as `flux fleet ingest/message`;
there is no self-spawned CLI, shell pipeline, tmux injection or stdout parser. An attached restart
loads the transcript from the main store and reconstructs the rail from Fleet/Board state.

## Typed projection boundary

`flux-tui` owns presentation-only structs: attachment, goal, wave, worker, decision, Board item,
document link, metric and failure rows. It owns no repository parser. The embedding CLI supplies a
`FleetBoardSource` implementation backed by the same typed Board/Fleet readers and mutations as the
command family. The source returns bounded, deterministic snapshots and explicit unavailable/error
fields. Refresh is point-in-time and never blocks keyboard handling on an unbounded watch.

The view model has hard caps for rows and detail bytes. It includes total counts and truncation
markers, so a large Fleet cannot look smaller merely because the TUI bounded it. Statistics carry
the `flux.board-stats/v1` values; the TUI formats those values and never recomputes completion.

## Layout and interaction

At wide widths the transcript/composer remains primary and a surface-owned attention rail shows
Fleet state, active wave, worker counts, open decisions, blocked items and red gates. At narrow
widths the rail disappears so chat keeps the available columns; the persistent attached header
keeps connection, revision and the `F2` affordance visible, and `F2` opens the full operations overlay.
`/fleet` opens overview, `/board` opens Board work, and the overlay tabs are Overview, Board,
Workers, Decisions and Stats. Arrow keys move, Tab/Shift-Tab change tabs, Enter opens detail or begins
an explicitly labelled decision confirmation, `r` refreshes and Esc returns to chat. Mouse wheel
scrolls the active list; no mouse click can confirm a mutation.

Worker detail correlates only durable identifiers: assignment, wave, session, worktree, handoff,
review/rework, bounded activity and failure. It does not display secret-bearing prompts or arbitrary
worker output. Decision detail shows the question, options/trade-offs and recommendation; accepting
one requires a second explicit Enter and uses the Board decision operation.

## Authority and failure posture

Observation is read-only. The surface cannot dispatch, cancel, apply, push, publish, release,
deploy, delete a worktree or change story state. Requirements and confirmed decisions are the only
write paths in this epic, and each produces a durable acknowledgement. A read or refresh failure
keeps the last good snapshot with a stale/error marker rather than clearing workers or rendering
fabricated zeroes. A detached or stopped coordinator keeps Board/Fleet views available read-only but
disables the composer with the exact recovery command.
