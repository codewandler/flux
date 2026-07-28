---
id: C-116
title: Header mode badges — shell, auto-ok, effort, gather
pillar: Core
status: done
priority: P3
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# Header mode badges — shell, auto-ok, effort, gather

## Goal
The header shows session · model · token totals, but the mode flags the state already tracks are
invisible: the `/shell` bash opt-in, `--yes` auto-approve, reasoning `/effort`, and gather mode.
Render compact right-side badges so a permissive or unusual mode is always visible at a glance.

## Acceptance
- [x] `header_line` gains right-side badge segments shown only when active/non-default:
      `shell` (warn style), `auto-ok` (warn style), `effort:<level>`, `gather` — TestBackend test
      with shell + auto-approve on shows both badges at full width.
- [x] Badges ride the existing `bar_line` droppable-segment mechanism; among the badges,
      safety-relevant `auto-ok` is ordered most-precious (dropped last) — pinned by a narrow-width
      unit test.
- [x] `/shell` and `/effort` changes reflect on the next render: shell reads
      `flux_runtime::shell_opt_in()` live; the chosen effort is mirrored into `ChatState` when the
      command runs so the sync render path can badge it.
- [x] The `gather` badge follows `state.gather_mode` (already phase-driven).

## Progress
- Done 2026-07-28: header badges `auto-ok` (warn), `shell` (live `flux_runtime::shell_opt_in()`), `gather`, `effort:<level>` (mirrored into ChatState by /effort, seeded from engine at startup); segment order [auto-ok, tokens, cache, cost, shell, gather, effort] — badges shed first, auto-ok most precious (survives when even tokens drop). Tests: header_shows_mode_badges_when_active, narrow_header_drops_badges_last_keeps_auto_ok.

## Notes
- Seams: `bar_line` droppable segments `crates/flux-tui/src/lib.rs:1481` (mechanism already
  landed with the C-102 work), `header_line` `lib.rs:1119`, `gather_mode` `state.rs:49`,
  `TuiRunOptions.auto_approve` `lib.rs:77`, shell toggle `lib.rs:2208-2210`, `/effort` handler
  `lib.rs:2406-2438`.
- Nearly free after C-102: badges are just more ordered segments on the same mechanism.
