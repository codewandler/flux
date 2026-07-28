---
id: C-153
title: Fuzzy filtering and overflow indicators for the TUI pickers
pillar: Core
status: backlog
epic: tui-polish-round-2
design:
note: "fuzzy_rank already exists and serves @-path completion (lib.rs:520) while the session picker has no query at all and slash matching is a separate prefix/substring path (lib.rs:229); the 10/12-row popups give no more-below signal"
---

# Fuzzy filtering and overflow indicators for the TUI pickers

## Goal
`fuzzy_rank` is implemented and used for `@`-triggered path completion (`lib.rs:520`, pinned by
`fuzzy_rank_orders_prefix_substring_subsequence` at `lib.rs:4569`), but the session picker has no
query field (`rendering.rs:233-276`) and slash commands use a separate prefix/substring matcher
(`slash_matches`, `lib.rs:229`). A long session list or command list is therefore navigated by
arrow key alone, and neither popup signals that more rows exist below the window
(`rendering.rs:187,234`). Reuse the ranker and add an overflow affordance.

## Acceptance
- [ ] Typing in the session picker filters rows through `fuzzy_rank` (or a shared ranker built on
      it) with the selection clamped to the filtered set — failing-first test asserting the ranked
      order and a stable selection after filtering.
- [ ] Slash-command matching goes through the same ranker so `/thm` finds `/theme`, with existing
      `slash_matches` behavior preserved for exact prefixes.
- [ ] Overlays whose content exceeds the window show an overflow indicator (a scrollbar, matching
      the transcript's `Scrollbar` at `rendering.rs:77-89`, or a rendered counter) — asserted by
      test at a list longer than the window.
- [ ] Esc clears the query before closing the overlay (one Esc = one undo step).

## Progress
- (not started)

## Notes
- Land after C-152 so the query row and overflow indicator are added once in the shared chrome.
- **Scope note (2026-07-28 Amp feature-mining pass, [../research/amp.md](../research/amp.md)):** Amp
  ships a single fuzzy **command palette** (Ctrl+O) over every command and action, rather than
  per-overlay filtering. That is a superset of this story: once slash matching and the session
  picker share one ranker, a unified palette is mostly a third caller plus a keybinding. Deliberately
  *not* filed as a separate story — decide here whether the shared ranker's callers are the two
  existing pickers or a palette that subsumes them, and widen the acceptance if so.
