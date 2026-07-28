---
id: C-152
title: One shared overlay chrome for the queue, session, and help panels
pillar: Core
status: backlog
epic: tui-polish-round-2
design:
note: "queue/sessions/help each hand-roll a header line, windowing math and an n/m footer as borderless paragraphs (rendering.rs:186-315) while approval is the only Block::bordered (rendering.rs:428-432) — three copies, three looks"
---

# One shared overlay chrome for the queue, session, and help panels

## Goal
Three overlays each rebuild the same chrome by hand: the queue (`rendering.rs:186-231`), the session
picker (`rendering.rs:233-276`), and help (`rendering.rs:280-315`) all construct an accent header
line over `panel_bg`, their own `saturating_sub` scroll window, and (queue/sessions) their own
`n/m` counter — while the approval sheet is the only bordered panel in the TUI
(`rendering.rs:428-432`). Extract one panel helper so the overlays read as one product and the
windowing math exists once.

## Acceptance
- [ ] A single helper renders title + body rows + hints + counter for all three overlays, and each
      call site loses its hand-rolled header/counter — pinned by a failing-first TestBackend test
      asserting the same chrome shape for queue, sessions, and help.
- [ ] Selection styling, key hints, and the active-session marker are behavior-preserving; existing
      overlay tests stay green without assertion churn beyond the chrome itself.
- [ ] The windowing/scroll computation lives in one place and is unit-tested directly (selected
      item always visible, no panic at len 0/1).

## Progress
- (not started)

## Notes
- Sequence before C-153 (fuzzy filtering) so the query row and overflow indicator land in one
  helper instead of three.
- The approval sheet keeps its own bordered layout (it is positioned against the composer,
  `rendering.rs:348-353`); the helper only has to accommodate it if that comes for free.
