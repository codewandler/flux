---
id: L-78
title: flux render PNG output — resvg rasterization + embedded font + write_file_bytes
pillar: Language
status: in-progress
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
- [ ] `crates/flux-system/src/lib.rs` gains `write_file_bytes(path, &[u8])` (parallels
  `read_file_bytes` at :481), with a unit test.
- [ ] `flux render <file.flux> -o out.png` rasterizes SVG → PNG bytes → the file; opening the PNG
  shows rendered text (not tofu) — the embedded font resolves with no system font dependency.
- [ ] New deps (`resvg`, `usvg`, `tiny-skia`, `fontdb`) land on `flux-tools` only;
  `cargo test -p flux-codegate` confirms the layering lint accepts them (or the story records the
  codegate adjustment).
- [ ] Gate green: `cargo build/test/clippy -D warnings/fmt`.

## Progress
- (not started)

## Notes
- Design: [flux-render.md](../designs/flux-render.md) § "Phase 2 — PNG".
- Deliberately deferred: Phase 1 ([[L-76]]/[[L-77]]) is self-contained string generation with zero
  new deps; this story is the only one that adds a rasterizer + binary-write surface.
- Depends on [[L-77]].
