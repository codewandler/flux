---
id: C-102
title: Graceful narrow-width header/footer bars
pillar: Core
status: done
priority: P2
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# Graceful narrow-width header/footer bars

## Goal
On narrow terminals the TUI header/footer drop their entire right segment at once. Degrade
progressively instead: drop cost → cache → tokens one segment at a time, keeping as much
information visible as fits.

## Acceptance
- [x] `bar_line` accepts an ordered list of droppable right-side segments and drops from the end
      until the line fits (empty right side remains the floor). Pure unit tests pin the surviving
      segments at three widths.
- [x] Header at ~50 cols with tokens+cache+cost populated shows tokens (and cache if it fits) but
      not cost — pinned by a TestBackend render test.
- [x] Footer right side goes through the same segment mechanism (prepares C-105/C-106 additions).

## Progress
- Done 2026-07-28: bar_line takes ordered droppable segments (+1-col right margin); header splits tokens/cache/cost (cost drops first); footer on the same mechanism with leading separators per non-first segment. Tests: bar_line_drops_right_segments_progressively, narrow_header_keeps_tokens_drops_cost.

## Notes
- Seams: `bar_line` `crates/flux-tui/src/lib.rs:1464`, `header_line` `:1117`, `footer_line` `:1160`.
- Lands first in the epic — later stories add footer segments on top of this mechanism.
