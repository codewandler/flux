# flux-tools vendored assets

Binary assets compiled into the crate (via `include_bytes!`), with their provenance recorded here
— the repo has no other convention for vendored third-party assets, so this file is it.

## fonts/JetBrainsMono-Regular.ttf

| | |
| --- | --- |
| **What** | JetBrains Mono, Regular weight (TTF) |
| **Version** | v2.304 (upstream release, 2023-01-14) |
| **Upstream** | https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip (`fonts/ttf/JetBrainsMono-Regular.ttf` inside the zip) |
| **sha256** | `a0bf60ef0f83c5ed4d7a75d45838548b1f6873372dfac88f71804491898d138f` |
| **License** | SIL Open Font License 1.1 — full text in `JetBrainsMono-OFL.txt` (OFL-1.1 requires the license to accompany the font; the file ships unmodified, satisfying the Reserved Font Name terms) |
| **Why** | The `png` feature's rasterizer (`render_flux_png`) lays out text headlessly. This is the *only* font the rasterizer's `fontdb` sees — no system-font dependency, so `flux render -o out.png` produces identical output on a bare CI container and a desktop. "JetBrains Mono" is the third family in the SVG's CSS font stack (`render.rs` `FONT`), so usvg resolves it from the stack itself; the db's generic `monospace` family is also pinned to it. |

To update: download the new pinned release, replace the TTF and OFL text, re-record the sha256
above, and bump the version. The cmap coverage test (`embedded_font_covers_rendered_glyphs` in
`render.rs`) fails if the replacement lacks a glyph the renderer emits (box-drawing `├─└│`,
`é`, `—`, `€`).
