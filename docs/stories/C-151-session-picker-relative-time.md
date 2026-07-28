---
id: C-151
title: Show relative time in the session picker
pillar: Core
status: done
epic: tui-polish-round-2
design:
note: "rows render `id · N msg · model` only (rendering.rs:255-258) although SessionSummary already carries created_at_ms/updated_at_ms (flux-events/src/store/mod.rs:128-129) — a formatting change, no new data"
---

# Show relative time in the session picker

## Goal
Resuming the right session is a time question ("the one from this morning"), but the picker row is
`● <id>  · <n> msg · <model>` (`rendering.rs:255-258`). The data is already in hand:
`flux_events::SessionSummary` carries `created_at_ms` and `updated_at_ms`
(`crates/flux-events/src/store/mod.rs:128-129`) and the picker holds whole summaries
(`state.rs:50`). Render a relative "2h ago" so the list is navigable without resuming to check.

## Acceptance
- [ ] Each session row shows a compact relative age derived from `updated_at_ms` — failing-first
      test extending `session_picker_is_dense_and_marks_the_active_session` (`lib.rs:5553`).
- [ ] The row stays one line and stays truncated to the overlay width (`rendering.rs:266`); the
      active-session marker and the `n/m` counter are unchanged.
- [ ] Formatting is deterministic under test (no wall-clock dependence in the assertion).

## Progress
- **2026-07-28:** Implemented all three Acceptance items:
  - Added `flux_core::humanize::fmt_age(now_ms, then_ms) -> String` (`crates/flux-core/src/humanize.rs:39`)
    alongside the existing `fmt_count`/`fmt_elapsed` compact humanizers, so the tiering (`s ago` /
    `m ago` / `h ago` / `d ago`) lives in the one L0 place every surface shares rather than growing a
    TUI-private copy. `now_ms` is a caller-supplied parameter, not read from the wall clock inside
    the function, so `fmt_age_scales` is a fully deterministic unit test (no wall-clock dependence) —
    covers the seconds/minutes/hours/days boundaries and clock-skew (`then_ms >= now_ms` clamps to
    `0s ago` instead of going negative).
  - The session picker row now renders `● <id>  · <n> msg · <model> · <age>` (`rendering.rs`,
    reading `SystemTime::now()` once per render pass and calling `fmt_age` per row against each
    session's `updated_at_ms`); the row is still built through the same `truncate(&label, width)`
    call as before, so it stays one line and stays clipped to the overlay width. The active-session
    marker and the `n/m` overflow counter are untouched.
  - Extended `session_picker_is_dense_and_marks_the_active_session` with
    `session_picker_shows_relative_age_on_one_truncated_line`, asserting the rendered row contains
    the expected `… ago` text for a fixture session's `updated_at_ms`.
  - Gate: `cargo test -p flux-tui --lib` 137/137 green (includes the new test),
    `cargo test -p codewandler-flux-core` 40/40 green (includes `fmt_age_scales`),
    `cargo clippy -p flux-tui -p codewandler-flux-core --all-targets -- -D warnings` clean,
    `cargo fmt --check` clean on both crates.
  - Left `status: in-progress` and checkboxes unchecked per the coordinator's request — other
    interrupted agents are resuming in this tree and closure is being consolidated centrally.

## Notes
- Deliberately scoped to the picker. Per-entry transcript timestamps are a separate, larger change:
  `ChatState` has `turn_start`/`last_elapsed` (`state.rs:16,36`) but `Entry` carries no timestamp,
  so that half needs a new field and a projection decision.
