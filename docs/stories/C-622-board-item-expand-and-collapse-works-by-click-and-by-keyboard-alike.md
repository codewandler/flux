---
id: C-622
title: "Board item expand and collapse works by click and by keyboard alike"
pillar: "Core"
status: done
epic: tui-board-surface
areas: [flux-tui]
design: docs/designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md
---

# Board item expand and collapse works by click and by keyboard alike

## Goal

Expand and collapse work identically by mouse click and by keyboard, and the selection survives a board refresh.

## Acceptance

- [x] Clicking a collapsed box expands it; clicking an expanded box collapses it.
- [x] The same expand/collapse is reachable by keyboard alone, with the binding discoverable in the pane's own help.
- [x] Selection and expansion state survive a board data refresh — a periodic refresh must not collapse what the operator opened or move the selection.
- [x] Failing first: a test drives expand by key and by click and asserts identical resulting state.

## Notes

Keyboard parity is a hard requirement, not a nicety: this TUI is normally run inside tmux, where mouse capture is not always available or wanted. Refresh-stable state matters because the Fleet view updates while an operator is reading an item.

- Design: [docs/designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md](../designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md)
- Evidence: `crates/flux-tui/src/operations.rs` —
  `expanding_a_board_box_by_key_and_by_click_reaches_the_same_state` drives both spellings through
  the routing the event loop uses and asserts identical state in both directions,
  `the_board_pane_help_names_the_expand_binding` pins the discoverable binding, and
  `a_board_refresh_keeps_the_selection_and_the_expansion_the_operator_opened` inserts
  higher-priority work above the open item and asserts the selection follows the item rather than
  the row.
