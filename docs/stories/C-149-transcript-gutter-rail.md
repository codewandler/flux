---
id: C-149
title: Transcript gutter rail — give turn boundaries visual rhythm
pillar: Core
status: backlog
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
- (not started)

## Notes
- Seams: `entry_lines` (`lib.rs:1387`), `ensure_transcript_layout` (`lib.rs:1488`),
  `tool_header_line` (`lib.rs:647`) for the tool-card variant.
