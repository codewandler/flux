---
id: L-75
title: render_styled_spans — span form of the plan-tree renderer (ANSI refactored on top)
pillar: Language
status: done
epic: flux-render
design: docs/designs/flux-render.md
note: "one tree walk, two presentations: render.rs gains render_styled_spans (lines of (text, Role)); render_styled becomes the ANSI stringifier over it — flux-tui output byte-identical"
---

# render_styled_spans — span form of the plan-tree renderer (ANSI refactored on top)

## Goal
Extend `flux_lang::render` with `pub fn render_styled_spans(ast: &DraftAst) -> Vec<Vec<(String, Role)>>`
(lines of `(text, Role)` spans) and refactor the existing `render_styled` to be the ANSI stringifier
over those spans. `Role` mirrors the current `Palette` fields (`keyword`/`op`/`symbol`/`string`/
`lit`/`effect`/`connector`/`thing`, `render.rs:13`). Both `flux-tui` (ANSI) and `flow_render` (SVG,
L-76) then build on **one** tree walk — no "render to ANSI then parse it back" round-trip.

## Acceptance
- [x] `crates/flux-lang/src/render.rs` gains `Role` + `render_styled_spans`; `render_styled` is
  reimplemented as the ANSI palette applied to those spans.
- [x] Failing-first test: for a representative flow AST, joining each span line's `text` fragments
  reproduces the plain (uncoloured) render line-for-line, and connector glyphs (`├─`/`└─`/`│`)
  carry `Role::Connector`.
- [x] Behavior-preserving: all existing `render.rs` / `flux-tui` tests pass unchanged — the ANSI
  output of `render_styled` stays byte-identical (pin one snapshot if none exists).
- [x] Gate green: `cargo build/test/clippy -D warnings/fmt` + `cargo test -p flux-codegate`.

## Progress
- 2026-07-09 — DONE. `Role` (the 8 palette roles + `Text` for structural glue) +
  `render_styled_spans` landed; `head`/`expr`/`lit`/`eff`/`thing_span` now emit spans and
  `render_styled` is the palette stringifier over them. Byte-identity guarded by a marker-palette
  snapshot test (`styled_ansi_output_is_pinned_byte_exact`) written and pinned against the
  pre-refactor renderer, incl. the connector-wrapped indent-only runs (`"   "`/`"│  "`).
  Failing-first span test: `spans_join_to_plain_render_and_connectors_carry_role`. Full gate green.

## Notes
- Design: [flux-render.md](../designs/flux-render.md) § "2. `flux_lang::render`".
- `flux-tui` drives `render_styled` with an ANSI palette (`crates/flux-tui/src/plan.rs`) — keep it green.
- `sink.rs` is unrelated (interpreter observation sink).
- Consumed by [[L-76]] for the `tree` view.
