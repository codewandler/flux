---
id: C-623
title: "The TUI board pane is read-only and cannot mutate planning state"
pillar: "Core"
status: ready
priority: 23
epic: tui-board-surface
areas: [flux-tui]
design: docs/designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md
---

# The TUI board pane is read-only and cannot mutate planning state

## Goal

Keep the board pane read-only. It renders planning state and never mutates it.

## Acceptance

- [ ] No interaction in the board pane changes an item's status, priority, or any other planning field.
- [ ] Status changes remain the Board CLI's responsibility, which validates the transition; the pane offers no bypass.
- [ ] Failing first: a test drives every board-pane interaction and asserts the board revision is unchanged afterwards.

## Notes

This invariant is why the rest of the epic is safe to build. A clickable surface is exactly where an unvalidated second mutation path appears by accident, and planning transitions have real rules — `ready -> done` is not legal, only `ready -> in-progress -> done` — which a direct write would silently violate. A workspace board additionally refuses mutation while its member checkout is off the canonical ref, so a surface that assumed it could write would fail in ways an operator could not interpret.

- Design: [docs/designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md](../designs/the-tui-board-is-a-real-board-collapsed-expandable-clickable-items-rendering-markdown-detail.md)
