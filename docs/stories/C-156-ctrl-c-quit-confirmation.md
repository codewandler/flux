---
id: C-156
title: Confirm the Ctrl-C quit instead of exiting on the first blank-line press
pillar: Core
status: backlog
epic: tui-polish-round-2
design:
note: "Ctrl-C with a running turn interrupts, with a non-blank line clears, and with a blank line quits IMMEDIATELY and unconfirmed (lib.rs:3222-3248) — the one destructive-feeling path in the key map"
---

# Confirm the Ctrl-C quit instead of exiting on the first blank-line press

## Goal
The Ctrl-C handler has three arms (`lib.rs:3222-3248`): a running turn is cancelled with an
`(interrupting…)` notice, a non-blank composer is cleared, and a blank composer `break`s the event
loop — the session ends on a single keystroke with no confirmation and no on-screen hint that the
key is about to quit. Require a second press within a short window, announced in the footer.

## Acceptance
- [ ] With a blank composer and no running turn, the first Ctrl-C arms a transient "Ctrl-C again to
      quit" state and does not exit; a second press within the window exits — failing-first test
      driving two key events and asserting the loop survives the first.
- [ ] The armed state is visible in the footer and clears on any other input or on timeout.
- [ ] The footer state slots into the existing idle-left precedence without displacing the unread
      indicator or the C-105 mouse-off hint (`lib.rs:2018-2032`).
- [ ] The interrupt and clear arms are behavior-preserving.

## Progress
- (not started)

## Notes
- Correction recorded during review: this is not a three-step Ctrl-C ladder; the real gap is the
  unconfirmed instant quit on a blank line.
