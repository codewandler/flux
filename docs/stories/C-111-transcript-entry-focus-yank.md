---
id: C-111
title: Transcript entry focus — per-card expansion + OSC-52 yank
pillar: Core
status: ready
priority: P3
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# Transcript entry focus — per-card expansion + OSC-52 yank

## Goal
`expand_tools` is one global bool: Ctrl-E flips every card at once, so a single 30-line detail
forces every other card open too, and there is no way to copy one entry cleanly. Add a focusable
transcript cursor: Enter expands/collapses just the focused card, `y` copies the focused entry as
an OSC 52 clipboard write (works over SSH).

## Acceptance
- [ ] Shift-↑/Shift-↓ move an entry cursor through the transcript (detaches follow; Esc clears
      focus); the focused entry renders with the selection background — TestBackend test.
- [ ] Enter on a focused tool card toggles per-card expansion overriding the global
      `expand_tools`; neighbors stay unchanged (TestBackend test: one card expanded, the next one
      still collapsed). Ctrl-E keeps its global meaning; per-entry state bumps the transcript
      revision so the layout cache re-keys.
- [ ] `y` on a focused entry emits the entry's full text (message text, or a tool card's
      un-truncated detail) as an OSC 52 sequence through the terminal writer, confirmed by a
      `copied N lines` notice. The sequence is built by a pure helper — unit tests pin the base64
      payload and a size cap.
- [ ] Key precedence follows the epic's chain: approval modal > help overlay > search takeovers >
      transcript focus > composer.

## Progress
-

## Notes
- Seams: `expand_tools` `crates/flux-tui/src/state.rs:21`, Ctrl-E toggle `lib.rs:522`,
  `MAX_DETAIL` `lib.rs:115`, terminal writer `terminal_io.rs`, selection style in `theme.rs`.
- Complements C-105, doesn't replace it: native selection (capture off) copies wrapped screen
  rows with gutters; yank copies a whole entry cleanly and works over SSH where OSC 52 is the
  only clipboard path.
