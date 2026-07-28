---
id: C-106
title: Transcript scroll position indicator
pillar: Core
status: ready
priority: P2
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# Transcript scroll position indicator

## Goal
When detached from follow mode the scroll position is invisible (only the unread counter hints at
it). Show a scrollbar and a percent readout while detached.

## Acceptance
- [ ] While `!follow` with scrollable content: ratatui `Scrollbar` rendered on the transcript's
      right column (position from existing `scroll`/`last_max_scroll`; no new state) — TestBackend
      test asserts scrollbar glyphs at `x = width-1` after `scroll_up`, and none in follow mode.
- [ ] Footer right side gains a `⤓ NN%` segment while detached (via C-102's segment mechanism).

## Progress
-

## Notes
- Seams: `rendering.rs:56`, `footer_line` `lib.rs:1204`.
- The scrollbar overlays the last transcript column only while detached — deliberately NOT
  shrinking the transcript width (would re-key the layout cache on attach/detach).
