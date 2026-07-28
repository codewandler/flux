---
id: C-157
title: Render an empty-state card when the transcript has no entries
pillar: Core
status: done
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
- **2026-07-29:** Implemented all three Acceptance items:
  - New `ChatState.workspace_root: String` field (`state.rs`) — the surface's cwd, set once at
    launch in `run_with_options` (`lib.rs`) via `std::env::current_dir()` (same one-shot-at-startup
    posture as `session_id`/`model`, not re-read per frame). Empty for headless/test construction
    (`ChatState::new`/`for_session`), which the card treats as "omit the segment" rather than
    showing a placeholder.
  - `render_empty_state_card(frame, state, area)` (`rendering.rs`, right before
    `approval_tier_style`): a 3-line centered card — bold-accent "flux", a muted
    `<model>  ·  <workspace_root>` line (model via the same `model_spec.as_deref().unwrap_or(&model)`
    fallback the header bar uses), and a muted affordance line naming `/help`, `/` commands, and
    `@` files. `EMPTY_CARD_MIN_WIDTH = 44`: below that width (or height < 3) the function returns
    without rendering anything — the C-102 narrow posture (skip, don't wrap into noise).
  - The transcript render arm in `rendering.rs` (`pub fn render`) now branches on
    `state.entries.is_empty()`: the empty branch calls only `render_empty_state_card`; the
    non-empty branch is the untouched original `transcript_viewport` + C-106 scrollbar code. This
    is the mechanism behind "never participates in the layout cache/focus/scrolling" — when
    entries is empty, `ChatState::transcript_viewport`/`ensure_transcript_layout` (the cache,
    `last_max_scroll`, `last_page`) is never called at all, not just visually absent.
  - **Failing-first tests** (`lib.rs`, next to `renders_transcript_and_input`):
    `empty_transcript_shows_orientation_card_naming_model_workspace_and_affordances` (asserts
    model/workspace-root/`/help`/`@` text on an empty transcript, then asserts the card is gone
    after `push_user`), `empty_state_card_never_touches_transcript_layout_cache_or_scroll`
    (asserts `state.transcript_layout.borrow().is_none()` and `last_max_scroll == 0` after
    rendering an empty session), `narrow_terminal_skips_the_empty_state_card` (30-column terminal,
    asserts the workspace-root text is absent). All three failed to compile before this story
    (`workspace_root` didn't exist on `ChatState`) — confirmed the compile error, then implemented.
  - **Two pre-existing tests broke as a side effect and were fixed**: `composer_is_background_only_without_border_or_padding`
    and `theme_switch_restyles_screen` (`lib.rs`) both construct a `ChatState` with an empty
    transcript and then locate the composer's "d" (in "draft") via
    `buffer.content.iter().find(|c| c.symbol() == "d")` — the new card's affordance line contains
    "commands", whose "d" now sorts earlier in the buffer and was picked up instead. Fixed by
    adding `state.push_user("hi")` to both (a transcript entry with no "d" glyph) so the card
    doesn't render and the tests go back to exercising what they actually name — composer
    background and theme switching, not empty-transcript state. This is a real interaction any
    real session would also hit (every session opens on an empty transcript), not a test-only
    artifact.
  - Gate: `cargo test -p flux-tui --lib` 159/159 green, `cargo clippy -p flux-tui --all-targets --
    -D warnings` clean, `cargo fmt -p flux-tui -- --check` clean.

## Surprises / notes for follow-up
- The story's Seams list named `ChatState.entries` and the `centered` helper but not a new
  `workspace_root` field. Reading the surface's cwd directly inside `rendering.rs` (mirroring how
  the session-picker's relative-age already reads `SystemTime::now()` live) was considered and
  rejected: it would make the card's content depend on the test process's own working directory
  (fragile/non-deterministic under `cargo test`, and not thread-safe if another test ever
  `set_current_dir`s). A plain field set once at construction, builder-style like `with_cost`, was
  more testable and no riskier than the existing `session_id`/`model` fields — flagging this
  choice explicitly since the story didn't spell it out.

## Notes
- Seams: the transcript render arm in `rendering.rs:60-90`, `ChatState.entries` (`state.rs`),
  `centered` helper already used by the overlays (`rendering.rs:189`).
