---
id: C-537
title: "Code-block gutter in flux-markdown"
pillar: Core
status: ready
epic: tool-output-rendering
design: docs/designs/tool-output-rendering.md
areas: [flux-markdown]
note: "V-8 — the only surviving item of the prior TUI UX review (tui-ux-ui-epic) never filed"
---

# Code-block gutter in flux-markdown

## Goal

Fenced code blocks in rendered Markdown carry a `▎ ` left gutter on every row, so code reads as a
block in every Markdown surface (TUI transcript, CLI, export) and survives mono by glyph. This is
V-8 from `docs/designs/tui-ux-ui-epic.md`, the one surviving item C-341 did not ship.

## Acceptance

- [ ] Failing-first flux-markdown unit test: every rendered code-block row carries the gutter
      prefix; inline code is unchanged.
- [ ] Implemented in `Block::CodeBlock` reusing the existing `BlockQuote` prefix machinery
      (`crates/flux-markdown/src/render/layout.rs`), so both the TUI and ANSI renderers get it.
- [ ] The glyph survives `NO_COLOR`/mono (it is structure, not tint); TUI snapshot deltas reviewed.
- [ ] `cargo test -p flux-markdown` and `cargo test -p flux-tui` green.

## Progress

- 2026-08-05 — filed from the tool-output review (folded in as the prior review's unshipped
  survivor).

## Notes

- Evidence: `Block::CodeBlock` emits bare lines while `BlockQuote` pushes a muted `│ ` prefix
  (`crates/flux-markdown/src/render/layout.rs`, prefix machinery ~`:143-163`).
- Origin: [tui-ux-ui-epic](../designs/tui-ux-ui-epic.md) V-8;
  design: [tool-output-rendering](../designs/tool-output-rendering.md) F-7.
