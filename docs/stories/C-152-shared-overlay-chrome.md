---
id: C-152
title: One shared overlay chrome for the queue, session, and help panels
pillar: Core
status: done
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
- **2026-07-29:** Implemented all three Acceptance items in `crates/flux-tui/src/rendering.rs`:
  - `overlay_window(selected, total, cap) -> (start, count)` (`rendering.rs`, right after
    `centered`) is the one place the scroll-window math now lives — replaces three hand-rolled
    `saturating_sub` copies (queue/session picker; help never scrolled). Unit-tested directly and
    exhaustively in `rendering::tests` (a new `#[cfg(test)] mod tests` at the bottom of the file):
    `overlay_window_keeps_selection_visible_and_never_panics` brute-forces every
    `(selected, total, cap)` combination up to 6/6/8 and asserts the clamped selection is always
    inside the window and the window never exceeds bounds; `overlay_window_at_len_zero_and_one_never_panics`
    pins `total == 0`/`total == 1`; `overlay_window_scrolls_to_keep_a_far_selection_visible` pins
    the scroll-to-tail behavior. These are genuinely failing-first — the function didn't exist
    before this story (compile error), not just an unasserted behavior.
  - `render_overlay_panel(frame, theme, header, body, counter, max_width)` (same file) is the
    single helper: builds the accent-on-`panel_bg` header row, extends with the caller's
    already-styled/windowed body rows, optionally appends the ` n/m ` counter row, then does the
    one `centered` + `Clear` + `Paragraph` — sized EXACTLY to its content (`lines.len()`), not
    reserved-with-padding. All three call sites (queue-open modal, session picker, help) now build
    their rows via `overlay_window` + their own per-row styling (selection highlight, active marker,
    C-151 age, C-153 query/fuzzy-filtered header) and hand the result to `render_overlay_panel` —
    each site lost its own header/`Clear`/sizing/counter code.
  - **Failing-first TestBackend test**: `queue_session_and_help_overlays_size_exactly_to_their_content`
    (`lib.rs`, in the main `tests` module, next to the other overlay tests). It is a genuine
    behavior pin, not just a visual-equivalence check — before this story, queue/session always
    reserved `visible + 2` rows (header + body + a counter row EVEN WHEN NOT SHOWN), so a
    single-item queue/session panel was 3 rows tall with a wasted blank `panel_bg` row at the
    bottom; help was already exact-fit. The test opens each overlay with one item/no overflow and
    counts `panel_bg`-styled buffer rows, asserting exactly 2 (header + 1 row, no waste) for queue
    and session, and exactly `header + HELP_KEYS + "commands" + command-chunk-rows` for help —
    proving all three now share one waste-free sizing rule instead of two different ones. It fails
    to compile against the pre-refactor code (`overlay_window`/`render_overlay_panel` didn't
    exist) and would fail its row-count assertion against the OLD queue/session math even if
    those functions were stubbed in by hand.
  - Behavior-preserving: selection highlight (`fg(accent).bg(sel_bg)` vs `panel_style()`), the
    session picker's `●` active marker, C-151's per-row relative age, C-153's fuzzy-filtered
    header text and ` n/m ` overflow counter, and every existing overlay test (session picker,
    slash menu, help-with-file-commands, etc.) are unchanged and still green — the only chrome
    delta is the sizing simplification described above (documented, not hidden).
  - Left the approval sheet (`rendering.rs`, `if let Some(view) = &state.approval`) completely
    untouched per the story's note — it keeps its own `Block::bordered()` layout, which a
    concurrent session (C-154) was actively editing (risk-tiered border/title) in the same file
    at the same time; my edits landed in the queue/session/help/`overlay_window` regions only and
    never touched the approval block.
  - Gate: `cargo test -p flux-tui --lib` 159/159 green (includes 3 new `rendering::tests` +
    1 new chrome test — the extra count above 152+ also includes C-157's tests, implemented in
    the same session immediately after), `cargo clippy -p flux-tui --all-targets -- -D warnings`
    clean, `cargo fmt -p flux-tui -- --check` clean.

## Notes
- Sequence before C-153 (fuzzy filtering) so the query row and overflow indicator land in one
  helper instead of three.
- The approval sheet keeps its own bordered layout (it is positioned against the composer,
  `rendering.rs:348-353`); the helper only has to accommodate it if that comes for free.
