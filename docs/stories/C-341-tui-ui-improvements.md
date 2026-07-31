---
id: C-341
title: "Polish the TUI's narrow, monochrome, and long-running states"
pillar: Improve
status: done
epic: tui-ux-ui-epic
design: docs/designs/tui-ux-ui-epic.md
note: "Visible selection and focus, responsive intermediate widths, failure navigation, terminal tool states, turn boundaries, queue/session discoverability, and light-theme card surfaces"
---

# Polish the TUI's narrow, monochrome, and long-running states

## Goal

Finish the accepted UI review scope so the terminal interface remains legible without color, degrades deliberately at intermediate widths, and exposes navigation and status cues during long turns.

## Acceptance

- [x] Queue and session selection carry a `▸` glyph and bold modifier in monochrome; the composer carries an idle/running accent bar; overflow keeps an overlaid scrollbar visible.
- [x] Below 60 columns queued previews and slash descriptions disappear; below 40 the composer is one row; a running footer shows a droppable `+N queued` segment.
- [x] Ctrl-G / Ctrl-Shift-G cycle failed tool cards; interrupted cards seal as `⊘ cancelled`.
- [x] New user turns carry compact muted separators with the prior duration when available; light themes apply `panel_bg` to tool-card rows only.
- [x] The empty state advertises resumable durable sessions when any exist.
- [x] Focused render/state tests cover the accepted behavior and the crate/workspace gate passes.

## Progress

- 2026-07-31 — reconciled the accepted handoff against `docs/designs/tui-ux-ui-epic.md`, implemented all ten accepted items, and added focused regression coverage.

## Notes

- This story intentionally does not include the separate Markdown code-block gutter item from the older design; it was not in the accepted handoff's ten-item scope.
