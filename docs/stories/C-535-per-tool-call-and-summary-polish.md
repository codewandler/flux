---
id: C-535
title: "Per-tool call/summary polish: web.fetch, proc.run, task, grep/glob rows"
pillar: Core
status: ready
epic: tool-output-rendering
design: docs/designs/tool-output-rendering.md
areas: [flux-tui]
note: "No common op should fall to the k=v dump or a raw first-line summary"
---

# Per-tool call/summary polish: web.fetch, proc.run, task, grep/glob rows

## Goal

Every common op gets a purposeful header arg and one-line summary, and `grep`/`glob` expanded rows
are structurally readable: no `k=v` dumps, no raw first-body-line summaries, match locations
scannable.

## Acceptance

- [ ] `toolview::format_call` arm for `proc.run` (field names verified against the flux-tools
      registration); unit test.
- [ ] `toolview::format_result` arms for `web.fetch` (size/line summary, never the raw first body
      line) and `task` (first line of the sub-agent's answer); unit tests.
- [ ] `grep`/`glob` expanded detail rows are classified color-free (path:line prefix + match
      emphasis derived from the input pattern); the TUI styles them glyph-safe under mono; one frame
      or buffer test.
- [ ] Summaries stay one line and truncate width-aware like existing summaries.
- [ ] `cargo test -p flux-tui` green.

## Progress

- 2026-08-05 — filed from the tool-output review.

## Notes

- Evidence: `format_call`/`format_result` dispatch tables at `crates/flux-tui/src/toolview.rs:20-83,
  117-148`; the CLI's richer semantic previews at `crates/flux-cli/src/rendering.rs:292-364` show
  the direction (grep first matches, bash last line).
- Web-fetch body *rendering* (markdown) is explicitly deferred to the A-142 detail pane — this story
  is summaries and row classification only.
- Design: [tool-output-rendering](../designs/tool-output-rendering.md) F-5.
