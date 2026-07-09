---
id: L-77
title: flux render CLI subcommand + retire the tree-sitter doc-image Node script
pillar: Language
status: ready
priority: 16
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
- [ ] `flux render` subcommand in `crates/flux-cli/src/main.rs` (explicit subcommand per the CLI
  convention): positional `.flux` file, `--view source|tree` (default `source`), `-o <out.svg>`
  writes via `system.write_file`; without `-o` the SVG prints to stdout.
- [ ] End-to-end: `flux render examples/<one>.flux --view tree -o out.svg` produces an SVG that
  opens and shows the coloured plan tree; `--view source` eyeballs highlighting matching the theme.
- [ ] A hard parse error in `tree` view exits non-zero with the parser's message; `source` view
  still renders malformed input.
- [ ] The `codewandler/flux-tree-sitter` repo's README/AGENTS are updated to point doc-image
  regeneration at `flux render`, retiring `scripts/render-example.mjs` (separate repo — done as a
  companion change there).
- [ ] Gate green: `cargo build/test/clippy -D warnings/fmt` + `cargo test -p flux-codegate`.

## Progress
- (not started)

## Notes
- Design: [flux-render.md](../designs/flux-render.md) § "Phase 1 bonus — CLI subcommand".
- Depends on [[L-76]] (`render_flux_svg`). PNG output (`-o out.png`) is [[L-78]], not this story.
- CLI rule: explicit subcommands only ([[cli-explicit-subcommands]] convention).
