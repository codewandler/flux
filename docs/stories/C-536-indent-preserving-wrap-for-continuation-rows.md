---
id: C-536
title: "Indent-preserving wrap for transcript continuation rows"
pillar: Core
status: in-progress
epic: tool-output-rendering
design: docs/designs/tool-output-rendering.md
areas: [flux-tui]
note: "Over-width detail lines wrap to column 0, outside the gutter rail and the card"
---

# Indent-preserving wrap for transcript continuation rows

## Goal

A wrapped transcript row keeps its left edge: continuation rows repeat the gutter rail and the
logical line's leading indent, so long tool output (and any long entry line) stays visually inside
its card instead of dissolving to column 0.

## Acceptance

- [ ] Failing-first narrow-width TestBackend frame test: a long bash detail line's continuation rows
      start with the gutter + card indent (today they start at column 0).
- [ ] `wrap_styled_lines` derives a hanging prefix per logical line (leading gutter span + leading
      whitespace) and budgets continuation rows at `width - prefix`, guarding degenerate
      `prefix ≥ width`.
- [ ] C-109 running-badge pairing is unaffected (header rows are width-fitted and never wrap) and
      `entry_rows` spans remain exact — existing frame tests stay green.
- [ ] `cargo test -p flux-tui` green.

## Progress

- 2026-08-05 — filed from the tool-output review; implementation started same session.

## Notes

- Evidence: prefix-unaware wrapper at `crates/flux-tui/src/lib.rs:3190-3243`; gutter prepended per
  logical line at `lib.rs:2069`; detail rows pushed untruncated at `lib.rs:2548-2551`.
- Design: [tool-output-rendering](../designs/tool-output-rendering.md) F-6.
