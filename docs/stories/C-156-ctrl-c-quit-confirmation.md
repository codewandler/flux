---
id: C-156
title: Confirm the Ctrl-C quit instead of exiting on the first blank-line press
pillar: Core
status: done
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
- 2026-07-29: Implemented. `event_loop`'s async `KeyCode::Char('c') if ctrl` arm can't be driven
  directly by a unit test (`Tui`/`EventStream` are hardwired to a real terminal — the whole crate
  has no test that drives synthetic key events through `event_loop`), so the arm/confirm decision
  was pulled into a pure, directly-testable `ChatState` method pair instead, mirroring how the rest
  of the crate tests key-driven state (`queue_cancel_edit`, `toggle_focused_card`, etc. — all called
  directly, never through the event loop):
  - `ChatState::arm_or_confirm_quit(&mut self) -> CtrlCQuit` (new enum `Armed`/`Quit`): first call
    arms `ctrl_c_armed_at = Some(Instant::now())` and returns `Armed`; a second call while the prior
    arm is still within `CTRL_C_QUIT_WINDOW` (2s constant) returns `Quit`. A stale arm (window
    elapsed) is treated as unarmed, so a far-apart pair re-arms instead of quitting on a forgotten
    press.
  - `ChatState::clear_ctrl_c_arm(&mut self)` / `ChatState::ctrl_c_armed(&self) -> bool` (the latter
    also re-checks the window, so the footer naturally stops showing the hint after a timeout even
    without an explicit clear).
  - The event loop's blank-composer/idle Ctrl-C branch (previously an unconditional `break`) now
    calls `arm_or_confirm_quit()` and only `break`s on `Quit`.
  - A new guard at the very top of `Event::Key(key) => { ... }` (right after the `KeyEventKind::Press`
    check, before any overlay handling) calls `state.clear_ctrl_c_arm()` for every key that is not a
    literal Ctrl-C — this is what satisfies "clears on any other input" uniformly regardless of which
    mode (search, history-search, slash menu, focus, composer, …) ends up consuming the key.
  - `footer_line`'s idle-left `match` gained one arm, `Phase::Idle if self.ctrl_c_armed()`, placed
    AFTER the unread (`self.unread > 0`) and C-105 mouse-off (`!self.mouse_capture`) arms and before
    the default idle hint — so both existing hints keep priority over the new one, per the
    acceptance's precedence requirement.
  - The running-turn interrupt arm and the non-blank-composer clear arm each also now call
    `state.clear_ctrl_c_arm()` (defensive/explicit — an arm can only be set from the blank+idle
    branch, so in practice a prior non-Ctrl-C key would already have cleared it via the top-level
    guard) but are otherwise UNCHANGED: same `cancel.cancel()` + `(interrupting…)` notice, same
    `queue_cancel_edit`/`fresh_textarea` clear-line logic.
  Failing-first: wrote the four new tests against the implementation as it was being built (not a
  strict red-then-green sequence for this story, unlike C-155), so as an explicit correctness check
  afterward, `arm_or_confirm_quit` was temporarily stubbed to always return `Quit` (i.e. the old
  immediate-quit behavior) and the four new tests were confirmed to fail for the right reasons, then
  the real implementation was restored and the suite re-verified green.
  Tests added (`crates/flux-tui/src/lib.rs`, `mod tests`):
  `ctrl_c_on_blank_composer_arms_before_it_quits`, `ctrl_c_arm_clears_on_other_input_and_shows_in_the_footer`,
  `ctrl_c_arm_expires_after_the_window`, `ctrl_c_armed_hint_does_not_displace_unread_or_mouse_off`.
  Gate (crate-scoped): `cargo test -p flux-tui` 159 passed / 0 failed; `cargo clippy -p flux-tui
  --all-targets -- -D warnings` clean; `cargo fmt -p flux-tui -- --check` clean.
  New `pub(super)` field on `ChatState` (`crates/flux-tui/src/state.rs`): `ctrl_c_armed_at:
  Option<Instant>`. New crate-private items in `lib.rs`: `const CTRL_C_QUIT_WINDOW: Duration`,
  `enum CtrlCQuit { Armed, Quit }`. None of this is `pub` outside the crate.
  Note: shared-tree coordination — one poll cycle hit a transient whole-crate compile break from a
  concurrent agent's in-flight `ApprovalRequest.mutating` field addition (unrelated to this story;
  resolved itself within ~90s of retrying), and separately a transient clippy `unused_mut` and a
  transient `cargo fmt` diff in another concurrent story's (C-157) test code, both of which cleared
  on retry without any edit from this session. Also: mid-verification, an experiment that temporarily
  stubbed `arm_or_confirm_quit` used a full-file `cp` backup/restore of `lib.rs` rather than the
  Edit tool — in the few seconds that took, a concurrent agent (C-157) landed a `workspace_root`
  field + constructor initializer in `state.rs`/`lib.rs`, and the restore briefly clobbered the
  lib.rs half of that (a compile error: "no field `workspace_root`"). It resolved itself once the
  other agent's edit landed again; no data was permanently lost, but a full-file `cp` on a
  concurrently-edited shared file is a race the Edit tool's match-and-replace does not have, so it
  should not be repeated as a technique.

## Notes
- Correction recorded during review: this is not a three-step Ctrl-C ladder; the real gap is the
  unconfirmed instant quit on a blank line.
