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

- [x] Each item renders as a bordered box showing id, title, status and priority — nothing else while collapsed.
- [x] Boxes are grouped by status and ordered by priority within a group, matching `board next` ordering for the ready group.
- [x] An item with a Fleet wave in flight is visibly marked as such, sourced from Fleet state rather than inferred from the Board.
- [x] The pane does not build a box per item up front — it pages or virtualizes, proven by a test over a board with >1000 items.
- [x] Failing first: a test asserts group order and within-group priority order from a fixture board.

## Notes

The board has >1100 items today, so eager construction is a real cost rather than a theoretical one. Collapsed content is deliberately minimal: the point of the collapsed view is to be scannable, and every extra field competes with the item count that fits on screen.

- Design: [docs/designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md](../designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md)

## Evidence

- `BoardPane` in `crates/flux-tui/src/operations.rs` owns a grouped, priority-ordered *index* over
  the snapshot's items (one `usize` per item) and formats collapsed boxes only for the rows the
  viewport shows, paging to the selection.
- Group order mirrors the Board projection's ranking (in-progress, ready, blocked, backlog, done);
  inside a group the order is `board next` order — ascending priority, unprioritized last, then a
  natural id tiebreak so `C-99` precedes `C-100`.
- The in-flight marker is Fleet-sourced: a live worker's assignment or active-wave membership. A
  Board status of `in-progress` alone never marks a box.
- Failing first: `board_pane_groups_by_status_and_orders_by_priority` failed against the previous
  flat table (`flux/C-42 before flux/C-43`, fixture order preserved) before the pane existed.
- `cargo test -p flux-tui board_pane` — 3 passed (ordering, collapsed box + wave marker, and paging
  over a 1200-item board where a 24-row viewport builds exactly 24 rows).
