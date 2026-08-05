---
id: C-537
title: "Code-block gutter in flux-markdown"
pillar: Core
status: done
priority: 36
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

- [x] Failing-first flux-markdown unit test: every rendered code-block row carries the gutter
      prefix; inline code is unchanged.
- [x] Implemented in `Block::CodeBlock` reusing the existing `BlockQuote` prefix machinery
      (`crates/flux-markdown/src/render/layout.rs`), so both the TUI and ANSI renderers get it.
- [x] The glyph survives `NO_COLOR`/mono (it is structure, not tint); TUI snapshot deltas reviewed.
- [x] `cargo test -p flux-markdown` and `cargo test -p flux-tui` green.

## Progress

- 2026-08-05 — filed from the tool-output review (folded in as the prior review's unshipped
  survivor).
- 2026-08-05 — implementation started on dispatched source
  `9c851bd716e238014b3e10d80a591e7cec5a945a`; validating the deliberate parity divergence before
  changing shared layout.
- 2026-08-05 — **done**: `Block::CodeBlock` now pushes a repeating muted `▎ ` structural prefix
  through the same `Prefix` machinery as blockquotes; ANSI and ratatui output retain the existing
  code inset, including on blank rows and code nested in lists. The parity suite now asserts exact
  expected output for fenced snippets and screens fenced blocks (only) from corpus-oracle parity,
  leaving surrounding Markdown on the exact path. Failing-first evidence: targeted
  `code_block_gutter_is_a_documented_divergence` failed with bare `  alpha`, then passed with
  `▎   alpha`. `cargo test -p codewandler-flux-markdown`, `cargo test -p flux-tui`, targeted clippy
  for both crates, and workspace fmt check are green. (`codewandler-flux-markdown` is the actual
  Cargo package id; the story's `-p flux-markdown` selector is not registered.) The committed TUI
  loop-mock snapshot check passed unchanged, so there are no snapshot-file deltas.
- 2026-08-05 — **scope finding**: this is not the one-liner the prior review assumed. The L-02
  parity suite (`crates/flux-markdown/tests/parity.rs`) asserts *exact per-line parity* against the
  old `markdown-terminal`/`markdown-ratatui` oracles for the snippet suite (`fenced_code`,
  `code_no_lang`) **and the whole committed corpus** (`tests/corpus/*.md`, which is full of fenced
  blocks). A gutter is a deliberate divergence and needs the suite's tier-2 "documented
  divergence" treatment (see `nested_list_fix_over_oracle`) — snippet cases asserted against
  correct expected output, and a decision for the corpus tier (screen fenced blocks out, or
  post-process the oracle's code insets). Do not land the layout change without reworking the
  parity harness in the same pass.

## Notes

- Evidence: `Block::CodeBlock` emits bare lines while `BlockQuote` pushes a muted `│ ` prefix
  (`crates/flux-markdown/src/render/layout.rs`, prefix machinery ~`:143-163`).
- Origin: [tui-ux-ui-epic](../designs/tui-ux-ui-epic.md) V-8;
  design: [tool-output-rendering](../designs/tool-output-rendering.md) F-7.
