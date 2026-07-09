---
id: L-77
title: flux render CLI subcommand + retire the tree-sitter doc-image Node script
pillar: Language
status: done
epic: flux-render
design: docs/designs/flux-render.md
note: "flux render <file.flux> [--view source|tree] [-o out.svg] over render_flux_svg (SVG is text → system.write_file); becomes the doc-image generator — flux-tree-sitter's scripts/render-example.mjs retires, its README/AGENTS point at flux render"
---

# flux render CLI subcommand + retire the tree-sitter doc-image Node script

## Goal
A non-model entry point for L-76's renderer: `flux render <file.flux> [--view source|tree]
[-o out.svg]` in `flux-cli`, calling `render_flux_svg` and writing through `system.write_file`
(SVG is text). This is the non-gated way to use and verify the renderer, and it **replaces**
`flux-tree-sitter/scripts/render-example.mjs` (external, brittle, shells out to the `tree-sitter`
CLI) as the way flux regenerates its own doc images.

## Acceptance
- [x] `flux render` subcommand in `crates/flux-cli/src/main.rs` (explicit subcommand per the CLI
  convention): positional `.flux` file, `--view source|tree` (default `source`), `-o <out.svg>`
  writes via `system.write_file`; without `-o` the SVG prints to stdout.
- [x] End-to-end: `flux render examples/<one>.flux --view tree -o out.svg` produces an SVG that
  opens and shows the coloured plan tree; `--view source` eyeballs highlighting matching the theme.
- [x] A hard parse error in `tree` view exits non-zero with the parser's message; `source` view
  still renders malformed input.
- [x] The `codewandler/flux-tree-sitter` repo's README/AGENTS are updated to point doc-image
  regeneration at `flux render`, retiring `scripts/render-example.mjs` (separate repo — done as a
  companion change there).
- [x] Gate green: `cargo build/test/clippy -D warnings/fmt` + `cargo test -p flux-codegate`.

## Progress
- 2026-07-09 — Implemented end-to-end. `Render` subcommand (positional file, `--view source|tree`
  via a `RenderView` ValueEnum mirroring `flux_tools::render::View`, `-o` optional) dispatching to
  `run_render` → testable `run_render_in(&System, …)`: reads the file and writes the SVG through
  the workspace-confined `System` (`read_file`/`write_file`), stdout without `-o`. Tests:
  `render_subcommand_parses` (clap wiring + defaults) and
  `run_render_writes_svg_and_propagates_tree_parse_errors` (writes `<svg`, tree view surfaces the
  parser's message as a non-zero exit, source view total on malformed input). Verified live:
  `flux render examples/data-transforms.flux --view tree/source`, broken input exits 1 (tree) /
  renders (source); eyeballed the rasterized output (One-Dark, correct spans).
- 2026-07-09 — Companion change in `~/projects/flux-tree-sitter` (uncommitted there):
  `scripts/render-example.mjs` deleted, AGENTS.md regeneration section + repo map now point at
  `flux render examples/readme-example.flux -o assets/example.svg`, README caption updated (no
  longer claims grammar-generated), CHANGELOG note added, and `assets/example.svg` regenerated
  with `flux render` (same 768px layout; filename caption gone — the L-76 renderer has no title
  chrome; a few token classes shift hue because flux-lang's CST highlighter is the colour source
  now).

## Notes
- Design: [flux-render.md](../designs/flux-render.md) § "Phase 1 bonus — CLI subcommand".
- Depends on [[L-76]] (`render_flux_svg`). PNG output (`-o out.png`) is [[L-78]], not this story.
- CLI rule: explicit subcommands only ([[cli-explicit-subcommands]] convention).
