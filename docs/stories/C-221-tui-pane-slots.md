---
id: C-221
title: Pane slots in the TUI — layout split, bounds, and narrow-width suppression
pillar: Core
status: done
priority: 12
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-tui]
note: "render() is a fixed six-row Layout::vertical (rendering.rs:122-171) with no horizontal split at all; panes need left/right/bottom slots that are bounded and suppressed at narrow widths rather than squeezing the transcript — host-pushed only, no model path until C-223"
---

# Pane slots in the TUI

## Goal
Give `flux-tui` somewhere to put a pane. `ChatState` grows a pane collection, `render` grows the
`left` / `right` / `bottom` slots, and the whole thing is driven **by the host only** — a test can
push a pane, the model cannot yet. Rendering lands before reachability.

## Acceptance
- [x] `ChatState.panes` holds host-pushed panes keyed by id, with `turn` and `session` lifetimes
      honoured (`turn` panes cleared at turn end). `project` is not implemented — the reporter
      already rejects it (C-220).
- [x] `render` (`crates/flux-tui/src/rendering.rs:122`) splits the transcript row horizontally for
      `left`/`right` and takes one extra vertical constraint for `bottom`. `overlay` reuses
      `render_overlay_panel` (`:36`) rather than growing a second overlay chrome — C-152 consolidated
      that on purpose.
- [x] **Bounded by surface constants, not by model input:** max pane count, max rows per pane, max
      total width fraction. A pane exceeding its bound is truncated by the surface, never allowed to
      push the transcript below its minimum.
- [x] **Failing-first test (`TestBackend`):** below a minimum transcript width, panes are not drawn
      at all — the same posture `EMPTY_CARD_MIN_WIDTH` (`:66`) established and C-102 took for the
      header/footer bars. A narrow terminal renders exactly as it does today.
- [x] Panes never participate in `transcript_viewport`: no layout cache entry, no `focused` index, no
      scroll bookkeeping. `render_empty_state_card`'s doc comment (`:70-74`) states why; the same
      rule applies and is asserted.
- [x] Each `kind` renders through machinery the TUI already owns — `markdown` via `flux-markdown`,
      `tree` via `plan.rs` — so no widget dependency is added under the `ratatui` 0.29 hold.
- [x] Full gate green; existing TUI layout tests unchanged (a session with no panes must render
      byte-identically to today).

## Progress
- (not started — depends on C-220's types)

## Notes
- The layout to modify is `Layout::vertical([Length(1), Min(1), Length(queue_h), Length(menu_h),
  Length(input_h), Length(1)])` at `rendering.rs:160-171`. The horizontal split goes around
  `transcript_area` only — header, footer and composer keep the full width.
- Scroll/focus interactions are the trap. C-111 (focus), C-108 (search) and C-106 (the scroll
  indicator) all key off wrapped transcript rows; a pane must be invisible to all three.
- No trust chrome in this story beyond a placeholder — C-222 owns the mark, the border style and the
  approval-sheet ordering, and it is the story that proves them.
