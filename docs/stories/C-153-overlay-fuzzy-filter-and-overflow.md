---
id: C-153
title: Fuzzy filtering and overflow indicators for the TUI pickers
pillar: Core
status: done
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
- **2026-07-28:** Implemented all four Acceptance items:
  - `fuzzy_rank_indices` (`lib.rs:547`) is now the shared ranker for all three callers: `@`-path
    completion (unchanged), slash-command matching (`slash_matches`, `lib.rs:240`, now built on
    `fuzzy_rank_indices` over command names instead of its own prefix/substring pass — `/thm` finds
    `/theme` via subsequence tiering, pinned by
    `slash_matches_ranks_subsequence_like_at_path_completion`), and the new session picker
    (`ChatState::session_picker_matches`, `lib.rs:1358`, ranking `"<id> <model>"` labels).
  - **Reconciliation with C-164** (`EventStore::search`, which shipped after this story's text was
    written and names the session picker as its intended consumer): kept the two seams separate
    rather than merging them. `session_picker_matches` ranks/filters only the session summaries
    already loaded into the picker by id/model — it never re-queries the store per keystroke.
    `EventStore::search`'s free-text CONVERSATION search stays a complementary, not-yet-wired seam.
    Rationale: the Acceptance text only asks for `fuzzy_rank`-based ranking with a stable clamped
    selection, which is fully satisfied without touching the store; a content-search fallback would
    need to bypass the label ranker for content-only matches (a session whose id/model doesn't
    fuzzy-match the query at all but whose conversation does), which means a second ranking tier and
    new state to distinguish "already content-matched" from "still needs label filtering" — real
    complexity with no Acceptance line asking for it. Documented inline at `lib.rs:1350-1357` so the
    next story that wants full-text session search starts from an explicit decision, not a gap.
  - Overflow indicator: both the slash menu and the `@`-path popup render a ` n/m ` counter row
    (matching the existing queue-overlay counter's style) once their candidate count exceeds the
    6-row window, reserving one extra `menu_h` row for it (`rendering.rs`). Session picker already
    had one (unchanged).
  - Esc: `ChatState::session_esc` (`lib.rs:1377`) clears `session_query` on the first Esc and only
    closes the overlay on a second Esc once the query is already empty, pinned by
    `session_picker_esc_clears_query_before_closing_overlay`.
  - Gate: `cargo test -p flux-tui --lib` 137/137 green, `cargo clippy -p flux-tui --all-targets --
    -D warnings` clean, `cargo fmt --check -p flux-tui` clean.
  - Left `status: in-progress` and checkboxes unchecked per the coordinator's request — other
    interrupted agents are resuming in this tree and closure is being consolidated centrally.

## Notes
- Land after C-152 so the query row and overflow indicator are added once in the shared chrome.
- **Scope note (2026-07-28 Amp feature-mining pass, [../research/amp.md](../research/amp.md)):** Amp
  ships a single fuzzy **command palette** (Ctrl+O) over every command and action, rather than
  per-overlay filtering. That is a superset of this story: once slash matching and the session
  picker share one ranker, a unified palette is mostly a third caller plus a keybinding. Deliberately
  *not* filed as a separate story — decide here whether the shared ranker's callers are the two
  existing pickers or a palette that subsumes them, and widen the acceptance if so.
