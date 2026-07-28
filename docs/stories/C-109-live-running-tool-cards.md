---
id: C-109
title: Live elapsed + animated badge on running tool cards
pillar: Core
status: ready
priority: P3
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# Live elapsed + animated badge on running tool cards

## Goal
Running tool cards show a static `◌` badge (deliberately, to protect the layout cache). Give them
a live elapsed timer and animated glyph without invalidating the cache per tick.

## Acceptance
- [ ] Running tool header rows show `⠋ running · 2s`-style live badge, animated on the existing
      62 ms running tick; done cards unchanged (`✓/✗` + elapsed).
- [ ] Implementation patches only visible running header lines in `transcript_viewport` (already
      cloned), via `running_rows` recorded on `TranscriptLayout` at build time and a shared
      `tool_header_line()` helper extracted from `tool_lines` (pad math cannot drift).
- [ ] Test pins that the transcript layout cache revision is UNCHANGED across animation frames,
      and that the patched badge disappears once the result lands.

## Progress
-

## Notes
- Seams: `tool_lines` `lib.rs:1032` (static-badge comment at `:1036-1038` states the cache
  constraint), `ToolEntry.started` `lib.rs:283`, `transcript_viewport` `lib.rs:1005`.
- Riskiest story in the epic; isolated, no dependents. Fallback: revision-bump per tick only while
  a tool runs (correct but O(transcript) per frame) — first cut only.
