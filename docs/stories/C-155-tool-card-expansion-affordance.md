---
id: C-155
title: Advertise tool-card expansion with a marker in the header
pillar: Core
status: backlog
epic: tui-polish-round-2
design:
note: "per-card expansion ALREADY exists (tool.expanded, lib.rs:1831; toggle at lib.rs:1741) — the gap is purely discoverability: the header is `→ verb arg … badge` with no marker (lib.rs:647-672), while Thinking advertises `Ctrl-E details` (lib.rs:1408)"
---

# Advertise tool-card expansion with a marker in the header

## Goal
Tool cards can be expanded per card (`tool.expanded`, toggled at `lib.rs:1741`, honored at
`lib.rs:1831`) and globally via Ctrl-E, but nothing on the card says so: `tool_header_line` renders
`→ verb  arg … badge` (`lib.rs:647-672`) and the summary line is a bare truncated string
(`lib.rs:1823-1826`). By contrast the `Thinking` entry explicitly renders
`thinking · N lines · Ctrl-E details` (`lib.rs:1408`). Add a collapse/expand marker so the detail
is discoverable without reading the help overlay.

## Acceptance
- [ ] A collapsed card with detail available shows a marker (e.g. `▸`, `▾` when expanded), rendered
      through `tool_header_line` so the pad/width math stays shared with the C-109 per-tick badge
      patch — failing-first `transcript_lines` test on a collapsed and an expanded card.
- [ ] The marker is never the header row's last span: the running-badge pairing matches the last
      span against `RUNNING_BADGE` (`lib.rs:1554-1567`), and a test asserts running cards still pair.
- [ ] A card with no expandable detail shows no marker (no false affordance).

## Progress
- (not started)

## Notes
- Correction recorded during review: the original suggestion claimed expansion did not exist. It
  does (C-111); this story is discoverability only and adds no new key binding.
