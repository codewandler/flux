---
id: L-78
title: flux render PNG output — resvg rasterization + embedded font + write_file_bytes
pillar: Language
status: backlog
priority:
epic: flux-render
design: docs/designs/flux-render.md
note: "Phase 2, opt-in: flux-system gains write_file_bytes; flux-tools gains resvg/usvg/tiny-skia/fontdb + an embedded monospace font (headless text layout); CLI -o out.png rasterizes L-77's SVG — model-facing tool stays SVG-only; ⚠ confirm flux-codegate accepts the new deps"
---

# flux render PNG output — resvg rasterization + embedded font + write_file_bytes

## Goal
Opt-in PNG output for `flux render` (CLI only — `ToolResult` is text-only, so the model-facing
`flow_render` stays SVG): rasterize the L-76 SVG with `resvg`/`usvg`/`tiny-skia`/`fontdb`, embedding
a monospace font (e.g. JetBrains Mono) so text lays out headlessly, and write the bytes through a
new `flux-system` bytes writer.

## Acceptance
- [x] `crates/flux-system/src/lib.rs` gains `write_file_bytes(path, &[u8])` (parallels
  `read_file_bytes` at :1166), with a unit test.
- [x] `flux render <file.flux> -o out.png` rasterizes SVG → PNG bytes → the file; opening the PNG
  shows rendered text (not tofu) — the embedded font resolves with no system font dependency.
- [x] New deps (`resvg`, `usvg`, `tiny-skia`, `fontdb`) land on `flux-tools` only;
  `cargo test -p flux-codegate` confirms the layering lint accepts them (or the story records the
  codegate adjustment).
- [x] Gate green: `cargo build/test/clippy -D warnings/fmt`.

## Progress
- 2026-07-27 (branch `feat/l78-render-png`, worktree): `write_file_bytes` shipped at
  `crates/flux-system/src/lib.rs:1103` with 4 tests (NUL/invalid-UTF-8 roundtrip, nested parents,
  `../` escape, read-only-root rejection).
- Rasterizer: `render_flux_png` in `crates/flux-tools/src/render.rs` behind a new **`png` cargo
  feature** (decision: feature-gated so the 8 flux-tools consumers skip the ~30-crate stack;
  flux-cli enables it in `default`, so the stock binary + `task install` include it).
  `default-features = false` + `text` drops system fonts and raster-image decoders — hermetic by
  construction. Embedded JetBrains Mono v2.304 (`crates/flux-tools/assets/fonts/`, sha256 +
  OFL-1.1 recorded in `assets/README.md`); ~16.7M-pixel canvas budget rejects oversized renders
  pre-allocation. 6 tests: magic+IHDR dims tied to the 214×106 SVG geometry pin, theme-colour
  pixel counts, tree-view connectors, same-process determinism, ttf-parser cmap pin
  (`├─└│ é — €`), budget error.
- CLI: `-o` extension dispatch in `run_render_in` (`.png` case-insensitive → `write_file_bytes`;
  else SVG text; stdout stays SVG). `run_render_writes_png_bytes_and_keeps_the_jail` covers
  magic/byte-identity/IHDR dims/case/other-extension/escape.
- Codegate: **no adjustment needed** — the layering lint only sees workspace crates
  (`flux-codegate/src/lib.rs:671`), so external deps pass trivially. Note: codegate runs
  `cargo metadata --locked --offline`, so the regenerated `Cargo.lock` must be committed with
  this branch.
- MSRV: workspace `rust-version` bumped 1.85 → **1.87** (resvg/usvg 0.47 declare it; user call).

## Notes
- Design: [flux-render.md](../designs/flux-render.md) § "Phase 2 — PNG".
- Deliberately deferred: Phase 1 ([[L-76]]/[[L-77]]) is self-contained string generation with zero
  new deps; this story is the only one that adds a rasterizer + binary-write surface.
- Depends on [[L-77]].
