---
id: C-539
title: "One truncation policy module for both surfaces"
pillar: Core
status: in-progress
epic: tool-output-rendering
design: docs/designs/tool-output-rendering.md
areas: [flux-tui, flux-cli]
note: "TUI caps at 30 lines, CLI at 40/500 chars with per-tool heads — no shared declaration"
---

# One truncation policy module for both surfaces

## Goal

The TUI and CLI read their tool-output elision budgets from one policy declaration in `toolview`.
Surfaces may budget differently on purpose — the CLI cannot expand, so it shows more up front — but
the numbers and per-tool semantics are declared side by side and cannot drift silently.

## Acceptance

- [ ] A `toolview` policy module declares the per-surface budgets (TUI detail cap; CLI preview
      line/char caps) with the rationale for any intentional difference.
- [ ] `crates/flux-tui/src/lib.rs` (`MAX_DETAIL`) and `crates/flux-cli/src/rendering.rs`
      (`tool_preview` caps) consume the shared declaration; no local literals remain.
- [ ] A drift-pinning test on each side fails if a surface stops consuming the shared policy.
- [ ] `-v`/verbose behavior unchanged on both surfaces.
- [ ] `cargo test -p flux-tui` and `cargo test -p flux-cli --lib` green.

## Progress

- 2026-08-05 — filed from the tool-output review; implementation started same session (batched with
  C-534, which edits the same module).

## Notes

- Evidence: TUI `MAX_DETAIL = 30` (`crates/flux-tui/src/lib.rs:203`); CLI `MAX_LINES = 40`,
  `MAX_LINE_CHARS = 500` plus per-tool head counts (`crates/flux-cli/src/rendering.rs:20-56,
  292-364`).
- Design: [tool-output-rendering](../designs/tool-output-rendering.md) F-9.
