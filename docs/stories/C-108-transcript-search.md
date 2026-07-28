---
id: C-108
title: Ctrl-F transcript search with match highlight
pillar: Core
status: done
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
- [x] Ctrl-F opens search (typing mode); query edits update matches live; Enter commits; `n`/`N`
      step and center the current match (detaches follow); Esc exits and clears highlights.
- [x] Matches computed over wrapped transcript rows, cached keyed on (revision, width) and
      recomputed lazily on change — resize cannot leave stale row indices (unit test on the
      match-row fn against a hand-built layout).
- [x] Visible matches highlighted REVERSED (current match additionally accent) by post-processing
      only the cloned viewport slice — the layout cache itself is untouched (TestBackend test
      asserts the modifier on a match cell and the footer counter ` 3/17`).
- [x] Documented v1 limitation: matches spanning a wrap boundary aren't found.

## Progress
- Done 2026-07-28: find_match_rows + highlight_matches (span-splitting, viewport-only); matches cached keyed on (revision,width), refreshed lazily; n/N center via center_current_match; footer counter. Tests: find_match_rows_matches_flattened_rows, transcript_search_highlights_and_centers.

## Notes
- Seams: `TranscriptLayout` `lib.rs:380`, `transcript_viewport` `lib.rs:1005`, `scroll_up/down`
  `lib.rs:2526`.
- Ctrl-F shadows tui-textarea forward-char — accepted (arrows remain; Ctrl-E precedent).
