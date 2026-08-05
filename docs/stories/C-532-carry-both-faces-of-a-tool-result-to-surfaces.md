---
id: C-532
title: "Carry both faces of a ToolResult to surfaces; yank copies canonical content"
pillar: Core
status: ready
epic: tool-output-rendering
design: docs/designs/tool-output-rendering.md
areas: [flux-lang, flux-tui, flux-cli]
note: "run_call flattens view into content at the sink boundary; yank copies line-numbered views"
---

# Carry both faces of a ToolResult to surfaces; yank copies canonical content

## Goal

Live surfaces see both faces of a tool result — the model-facing `view` for display and the
canonical `content` for copy/machine paths — matching what the event store already persists. The
focused-entry yank copies the canonical content, not a line-numbered rendering.

## Acceptance

- [ ] `run_call` (`crates/flux-lang/src/runtime.rs:3760-3771`) stops flattening: it emits
      `content = outcome.content` and `view = Some(outcome.view)` when they differ. No trait
      signature change (`OpOutcome` already carries both fields).
- [ ] The TUI's `ToolOutcome` stores both; display uses view-else-content (existing frame tests for
      a numbered `read` card stay unchanged); a failing-first test on `focused_entry_text` proves
      `y` yanks the un-numbered canonical content.
- [ ] The resume path stores both faces instead of discarding content when a view exists
      (`crates/flux-tui/src/lib.rs:2983-2989`), so historical yank matches live yank.
- [ ] CLI, SDK, server, and stream-json sinks choose a face explicitly; stream-json emits both
      fields, called out in release notes.
- [ ] whatif cassette hashing is inventoried before landing; any recorded-baseline consequence is
      stated in this story.
- [ ] Sequenced with C-526 so its PTY-level test pins canonical-content yanks.
- [ ] Full repository gate green.

## Progress

- 2026-08-05 — filed from the tool-output review, which initially mis-read the defect as "live shows
  content, resume shows view"; source-walking `run_call` inverted it: live display is already the
  view — the yank payload and the lost canonical face are the real defects.

## Notes

- Evidence: `ToolResult` two-face contract at `crates/flux-runtime/src/lib.rs:70-80` ("shown to the
  model (and the user)"); flattening with intent comment at `crates/flux-lang/src/runtime.rs:3760`;
  yank path `crates/flux-tui/src/lib.rs:2397-2417` (OSC-52, 72 KiB cap).
- Design: [tool-output-rendering](../designs/tool-output-rendering.md) F-2.
