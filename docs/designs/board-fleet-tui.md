# Design — The board and fleet operations TUI

**Status:** proposed follow-up · **Stories:** [C-556](../stories/C-556-fleet-main-agent-tui-shell.md),
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
