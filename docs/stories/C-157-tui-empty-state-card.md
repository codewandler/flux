---
id: C-157
title: Render an empty-state card when the transcript has no entries
pillar: Core
status: backlog
epic: tui-polish-round-2
design:
note: "the splash dismisses (splash.rs:552-566, played at lib.rs:2578-2580) and leaves a blank transcript; the only onboarding is the one-line idle footer hint (lib.rs:2029-2032)"
---

# Render an empty-state card when the transcript has no entries

## Goal
After the decorative boot splash finishes (`splash_intro`, played at `lib.rs:2578-2580`), a fresh
session shows an empty transcript. The sole orientation is the idle footer hint
`Enter send · Ctrl-J newline · / commands` (`lib.rs:2029-2032`). A short centered card costs nothing
(no entries to displace) and answers "where am I and what can I do here".

## Acceptance
- [ ] When `entries` is empty, the transcript area renders a centered card naming the active model,
      the workspace root, and the primary affordances (`/help`, `/` commands, `@` file completion)
      — failing-first TestBackend test asserting the card on an empty state.
- [ ] The card disappears as soon as the first entry lands and never participates in the transcript
      layout cache, focus (C-111), or scrolling.
- [ ] Narrow terminals degrade gracefully (the card is skipped rather than wrapped into noise),
      matching the C-102 narrow-width posture.

## Progress
- (not started)

## Notes
- Seams: the transcript render arm in `rendering.rs:60-90`, `ChatState.entries` (`state.rs`),
  `centered` helper already used by the overlays (`rendering.rs:189`).
