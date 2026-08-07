---
id: C-621
title: "Expanding a board item renders its story body as markdown in place"
pillar: "Core"
status: done
epic: tui-board-surface
areas: [flux-tui]
design: docs/designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md
---

# Expanding a board item renders its story body as markdown in place

## Goal

Expanding an item renders its story body — Goal, Acceptance checkboxes, Notes — as markdown in place, so reading an item's contract never means leaving the TUI.

## Acceptance

- [x] Expanding renders the story body as markdown inline: headings, lists, checkboxes and fenced code are visually distinct from prose.
- [x] Acceptance checkboxes render their checked state, since that state is what decides whether a story is complete.
- [x] Rendering is width-aware and wraps within the pane, with no character lost at a row boundary and no horizontal overflow.
- [x] A long body scrolls within the expanded box rather than pushing the rest of the board off screen.
- [x] Failing first: a test renders a body whose wrapped width is exactly the pane boundary and asserts continuity across rows.

## Notes

The width-aware requirement is not speculative — the transcript pane already carried an off-by-one defect where `area_width` was used as usable columns, dropping one character per wrapped row. Two regression tests for it passed *without* the fix before a third caught it, so the test here must be built on a body that actually crosses the boundary.

- Design: [docs/designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md](../designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md)
