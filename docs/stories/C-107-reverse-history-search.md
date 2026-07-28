---
id: C-107
title: Ctrl-R reverse incremental history search
pillar: Core
status: done
priority: P2
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# Ctrl-R reverse incremental history search

## Goal
Prompt history (durable, 500 cap) is only reachable via ↑/↓ stepping. Add readline-style Ctrl-R
reverse incremental search.

## Acceptance
- [x] Ctrl-R opens search (saving the draft via the existing `history_draft` mechanism); printable
      chars/Backspace edit the query; matches place the entry live into the composer; Ctrl-R again
      steps older; Enter keeps the composer content (does not send); Esc restores the draft.
- [x] Pure `rsearch(history, query, before)` fn — unit-tested (substring, backwards, stepping).
- [x] Footer takeover line `(reverse-i-search) 'q':` while active — render test. Footer takeover
      precedence shared with C-108 (search > history-search > normal).
- [x] Shadowing note: Ctrl-R was tui-textarea redo — deliberately shadowed (Ctrl-U undo remains);
      documented in the help text.

## Progress
- Done 2026-07-28: rsearch pure fn; Ctrl-R mode with live composer recall, Ctrl-R steps older, Enter keeps, Esc restores draft; footer takeover line. Tests: rsearch_steps_backwards_case_insensitive, history_search_footer_takeover_renders.

## Notes
- Seams: history arms `lib.rs:2036`, `history_prev/next`, `footer_line`.
