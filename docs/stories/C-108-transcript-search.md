---
id: C-108
title: Ctrl-F transcript search with match highlight
pillar: Core
status: ready
priority: P2
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# Ctrl-F transcript search with match highlight

## Goal
There is no way to find text in a long transcript. Add incremental search with n/N navigation and
visible match highlighting.

## Acceptance
- [ ] Ctrl-F opens search (typing mode); query edits update matches live; Enter commits; `n`/`N`
      step and center the current match (detaches follow); Esc exits and clears highlights.
- [ ] Matches computed over wrapped transcript rows, cached keyed on (revision, width) and
      recomputed lazily on change — resize cannot leave stale row indices (unit test on the
      match-row fn against a hand-built layout).
- [ ] Visible matches highlighted REVERSED (current match additionally accent) by post-processing
      only the cloned viewport slice — the layout cache itself is untouched (TestBackend test
      asserts the modifier on a match cell and the footer counter ` 3/17`).
- [ ] Documented v1 limitation: matches spanning a wrap boundary aren't found.

## Progress
-

## Notes
- Seams: `TranscriptLayout` `lib.rs:380`, `transcript_viewport` `lib.rs:1005`, `scroll_up/down`
  `lib.rs:2526`.
- Ctrl-F shadows tui-textarea forward-char — accepted (arrows remain; Ctrl-E precedent).
