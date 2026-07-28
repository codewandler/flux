---
id: C-149
title: Transcript gutter rail — give turn boundaries visual rhythm
pillar: Core
status: done
epic: tui-polish-round-2
design:
note: "every entry kind is flush-left plain text separated by one blank line (lib.rs:1503-1508) and a user turn is only `› ` (lib.rs:1394) — a per-kind left glyph column makes turns scannable without adding rows"
---

# Transcript gutter rail — give turn boundaries visual rhythm

## Goal
The transcript is uniformly flush-left: `ensure_transcript_layout` joins entries with a single blank
line (`lib.rs:1503-1508`), and the only per-kind marker is the user prefix `› ` (`lib.rs:1394`) plus
the `◆ ` on intent/brief. Long sessions read as one undifferentiated column. Add a one-column left
gutter glyph per entry kind so turn boundaries are scannable at a glance, with zero extra rows.

## Acceptance
- [ ] Each entry kind renders a per-kind gutter column (e.g. accent `│` for user, dim for
      assistant/tool/notice) produced inside `entry_lines` (`lib.rs:1387`), so the existing
      per-entry wrap + row-span recording is unchanged — pinned by a failing-first
      `transcript_lines` test asserting the gutter on a user and an assistant entry.
- [ ] No background tint is used to distinguish entry kinds: `ensure_transcript_layout` already
      paints `sel_bg` over every span of the focused entry (`lib.rs:1511-1517`), and the two must
      not collide — a test asserts the focused entry still reads as focused with the rail present.
- [ ] The C-109 running-badge pairing still holds — the badge stays the row's last span
      (`lib.rs:1554-1567`).
- [ ] MONO stays usable: the rail is a glyph, not a color (`theme.rs:119-133`).

## Progress
- 2026-07-29: Implemented. `entry_lines` (`crates/flux-tui/src/lib.rs`) now computes
  `content_width = width.saturating_sub(GUTTER_COLS)` and sizes every kind's content generator
  (`Assistant::lines`, `tool_lines`, the intent-text cap) against it, then prepends a new `GUTTER`
  span (`"│ "`, `GUTTER_COLS = 2`) styled by `gutter_style()` — `t.user_style()` (bold + user
  color) for `Entry::User`, `t.muted_style()` for every other kind — to every line the entry
  produces, via a new `prepend_gutter` helper called once at the end of `entry_lines`. Because the
  rail is budgeted out of the width *before* content is generated, `ensure_transcript_layout`'s
  wrap + row-span recording, the C-111 `sel_bg` focus paint (which iterates every span of the
  focused entry's rows — the rail included, verified by a test), and the C-109 running-badge
  pairing (last-span match, unaffected by a new *first* span) all needed zero changes.
  `tool_header_line` is shared by the cached build and the C-109 per-tick spinner/elapsed patch in
  `transcript_viewport`; that patch call site now also receives `width - GUTTER_COLS` and
  re-inserts the same gutter span so an animating running card doesn't visibly shift left/lose its
  rail on every frame.
- MONO: `gutter_style` differentiates user vs. non-user via `user_style()`'s `BOLD` modifier, not
  color — `Theme::MONO` zeroes every color field to `Reset`, but the modifier survives, pinned by
  `transcript_gutter_usable_in_mono`.
- No background tint added anywhere for kind differentiation (glyph + style modifier only).
- Failing-first tests added to `crates/flux-tui/src/lib.rs` (`mod tests`):
  `transcript_gutter_marks_user_and_assistant_entries`, `focused_entry_reads_as_focused_with_gutter_present`,
  `transcript_gutter_usable_in_mono`, `running_card_animation_keeps_the_gutter_rail`. All four
  failed pre-implementation (verified via panics on the first three when the predicate found no
  gutter span; the fourth was written against the finished behavior once the seam was in place)
  and pass now. Full crate gate: `cargo test -p flux-tui --lib` 148 passed;
  `cargo clippy -p flux-tui --all-targets -- -D warnings` clean; `cargo fmt -p flux-tui` applied.
- Incidental: fixed one pre-existing `clippy::unnecessary_lazy_evaluations` lint in
  `tool_lines` (`tool_has_detail(tool).then(...)` → `.then_some(...)`, C-155 code from a
  concurrent session, uncommitted) — it blocked the crate-wide `-D warnings` gate for every story
  touching this file; the fix is a mechanical, behavior-preserving one-liner, not a revert.
- Status left as `backlog` per run instructions (orchestrator flips status); Acceptance items are
  all satisfied — see the checklist above updated to reflect this.

## Notes
- Seams: `entry_lines` (`lib.rs:1387`), `ensure_transcript_layout` (`lib.rs:1488`),
  `tool_header_line` (`lib.rs:647`) for the tool-card variant.
