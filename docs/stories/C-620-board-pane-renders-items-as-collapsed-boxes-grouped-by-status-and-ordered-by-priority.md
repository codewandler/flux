---
id: C-620
title: "Board pane renders items as collapsed boxes grouped by status and ordered by priority"
pillar: "Core"
status: ready
priority: 20
epic: tui-board-surface
areas: [flux-tui]
design: docs/designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md
---

# Board pane renders items as collapsed boxes grouped by status and ordered by priority

## Goal

Render the Board as a board: one collapsed box per item, grouped by status and ordered by priority, so "what is next" is answerable without expanding anything.

## Acceptance

- [ ] Each item renders as a bordered box showing id, title, status and priority — nothing else while collapsed.
- [ ] Boxes are grouped by status and ordered by priority within a group, matching `board next` ordering for the ready group.
- [ ] An item with a Fleet wave in flight is visibly marked as such, sourced from Fleet state rather than inferred from the Board.
- [ ] The pane does not build a box per item up front — it pages or virtualizes, proven by a test over a board with >1000 items.
- [ ] Failing first: a test asserts group order and within-group priority order from a fixture board.

## Notes

The board has >1100 items today, so eager construction is a real cost rather than a theoretical one. Collapsed content is deliberately minimal: the point of the collapsed view is to be scannable, and every extra field competes with the item count that fits on screen.

- Design: [docs/designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md](../designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md)
