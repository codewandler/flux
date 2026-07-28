---
id: C-105
title: Mouse-capture toggle for native select/copy
pillar: Core
status: ready
priority: P1
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# Mouse-capture toggle for native select/copy

## Goal
Mouse capture is unconditionally on, which blocks terminal-native text selection/copy. Add a live
toggle so users can copy from the transcript without leaving the TUI.

## Acceptance
- [ ] Ctrl-T toggles mouse capture live (crossterm Enable/DisableMouseCapture on the same stdout;
      verified unbound in tui-textarea 0.7, so nothing is shadowed).
- [ ] While off, the footer idle hint shows ` mouse off · select/copy · Ctrl-T re-enable` in warn
      style — pinned by a TestBackend render test on `ChatState.mouse_capture = false`.
- [ ] Wheel scroll lost while off is accepted (PgUp/PgDn remain); `TerminalGuard` teardown stays
      correct in both states (double-disable is harmless).

## Progress
-

## Notes
- Seams: mouse arm `lib.rs:1828`, key dispatch `lib.rs:2022+`, `footer_line` `lib.rs:1160`,
  `terminal_io.rs:26`.
- Depends on C-102's footer segment mechanism.
