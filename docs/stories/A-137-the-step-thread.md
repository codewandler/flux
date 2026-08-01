---
id: A-137
title: "The step thread — finished steps condensed to one line, the current one expanded"
pillar: Agent
status: ready
priority: 6
design: docs/designs/agent-loop-visibility.md
epic: agent-loop-visibility
areas: [flux-tui]
note: "the whole feature at its most useful, shippable alone. The vocabulary is already on the wire — UiEvent carries Plan, Phase, Planning, Intent, ToolCall, ToolTiming, ToolResult, ToolProgress, CallUsage — so this is a VIEW problem, not instrumentation"
---

# One line per step, expanded where it matters

## Goal

A live thread of the agent loop: each finished step condensed to one line, the current step expanded to
show what is happening inside it.

## Why it stands alone

The events already exist. `UiEvent` (`crates/flux-tui/src/controller.rs:8`) carries the loop's whole
vocabulary — `Planning`, `Plan`, `Phase`, `Intent`, `ToolCall`, `ToolTiming`, `ToolResult`,
`ToolProgress`, `CallUsage`. Nothing new needs recording. What is missing is the progressive-disclosure
thread that makes them legible while a run is moving.

## Acceptance

- [ ] **Failing-first**: a test driving a scripted sequence of `UiEvent`s and asserting the rendered
      thread — one line per finished step, the current step expanded — failing at the merge base.
- [ ] A finished step's line says what it was, how it went, and how long it took. ⚠ Timing is already
      carried (`ToolTiming`, `ModelCallTiming`); do not re-measure it in the view, or the number the
      audience sees will disagree with the one the evidence log holds.
- [ ] The current step expands to show what is inside it, and collapses when it completes.
- [ ] ⚠ **Elision is visible.** If steps are dropped or collapsed to fit, the view says so. A view that
      silently drops steps is worse than one that admits it is behind — and this is the surface most
      likely to be watched by someone deciding whether to trust flux.
- [ ] Works with the existing transcript rather than replacing it; a user who does not want the thread
      is not forced into it.
- [ ] Full gate green.

## Notes

- [A-138](A-138-expand-a-step-into-its-graph.md) adds the graph expansion on top; do not build it here,
  but do not make it a rewrite either.
- ⚠ A pause hotkey ([run-control](../designs/run-control.md)) and a debugger
  ([interactive-debugger](../designs/interactive-debugger.md)) both want to attach controls to a step in
  this thread. Leave the seam; build neither.
- `crates/flux-tui/src/panes.rs` and `toolview.rs` are the incumbents — extend rather than growing a
  third way to draw a running tool.
- Sub-agent activity already has a home in `fleet.rs`'s `FleetProjection`. Decide whether the thread
  shows it or defers to that pane; do not duplicate the projection.

## Progress

- Filed 2026-08-01 with the agent-loop-visibility epic.
