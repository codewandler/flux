---
id: C-533
title: "Sanitize tool output at the transcript boundary"
pillar: Core
status: done
epic: tool-output-rendering
design: docs/designs/tool-output-rendering.md
areas: [flux-tui]
note: "Subprocess ESC/CR/BEL bytes reach ratatui cells verbatim while neighboring surfaces sanitize"
---

# Sanitize tool output at the transcript boundary

## Goal

Subprocess escape and control bytes can never reach a ratatui span from tool output: the transcript
gets the same sanitation posture as agent panes, approval prompts, and fleet names. Strip, do not
interpret — interpreted SGR would let payload bytes reach a `Style`, the primitive the trusted-chrome
boundary exists to deny.

## Acceptance

- [x] Failing-first TestBackend frame test: a finished bash card whose result contains `\x1b[31m`,
      `\r`, and `\x07` renders with no ESC/control cells anywhere in the frame.
- [x] Live-tail partial lines (C-158) are sanitized the same way.
- [x] Historical/resumed tool entries are sanitized on ingest.
- [x] The sanitizer is shared with `trust.rs`'s escape-consumption (one implementation), without
      applying the reserved-glyph replacement (that is chrome-forgery defense, not tool-text
      hygiene).
- [x] The plan-tree ANSI path (`plan.rs`, `ansi-to-tui`) is untouched.
- [x] `cargo test -p flux-tui` green.

## Progress

- 2026-08-05 — filed from the tool-output review; implementation started same session.
- 2026-08-05 — closed on the reconciled local `main`: live, completed, and resumed tool content all
  cross the shared escape/control sanitizer before transcript rendering; the TestBackend regression
  and the complete `flux-tui` package suite (292 tests across unit and loop-mock targets) pass.

## Notes

- Evidence: unsanitized spans built in `tool_lines`/`finish_tool`
  (`crates/flux-tui/src/lib.rs:1871-1914, 2436-2583`); neighboring sanitation at
  `crates/flux-tui/src/trust.rs:26-28`, approval prompts, fleet names.
- Design: [tool-output-rendering](../designs/tool-output-rendering.md) F-3.
