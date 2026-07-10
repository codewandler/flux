# Flux brand assets

Flux uses one visual idea: **plans cross an explicit execution gate**. Three input rails represent a
typed plan, the signal-green bar is the runtime boundary, and one resolved output leaves it. The mark
is deliberately flat and mechanical—no robot, orbit, lambda, glow, or generic “AI” decoration.

## Production assets

| Preview | File | Dimensions | Use |
| --- | --- | --- | --- |
| <img src="readme-hero.svg" alt="flux README hero" width="280"> | `readme-hero.svg` | Vector, 1200×360 viewBox | **README hero.** Fixed dark field with the positioning line and a labeled plan → gate → effect visual. |
| <img src="flux-logo.svg" alt="flux wordmark" width="220"> | `flux-logo.svg` | Vector | **Primary lockup and source of truth.** Uses vector paths only and adapts its ink/signal colors to light and dark mode. |
| <img src="flux-logo.png" alt="flux wordmark" width="220"> | `flux-logo.png` | 1240×360 | Light-background raster fallback. |
| <img src="flux-mark.svg" alt="flux execution-gate mark" width="140"> | `flux-mark.svg` | Vector | Standalone execution-gate mark for compact placements. |
| <img src="flux-mark.png" alt="flux execution-gate mark" width="140"> | `flux-mark.png` | 1008×656 | Light-background raster fallback for the standalone mark. |

The original generated exploration that informed the gate geometry is retained at
`source/flux-mark-concept.png`; it is design provenance, not a production asset.

## Usage

- Use `readme-hero.svg` for the repository front page and `flux-logo.svg` for other horizontal placements.
- Use `flux-mark.svg` when the wordmark would be unreadable or redundant.
- Keep clear space around the mark of at least half the signal bar's height.
- Prefer the adaptive SVGs. The PNGs contain the light-mode palette.
- Do not add gradients, shadows, glow, mascots, or orbital graph decoration to the core identity.

The older `logo-*`, `icon.png`, and `avatar.png` files are legacy generated explorations. They are
kept only so downstream consumers do not break without notice; do not use them for new work.

## Exporting raster fallbacks

```bash
rsvg-convert -w 1240 -h 360 flux-logo.svg -o flux-logo.png
rsvg-convert -w 1008 -h 656 flux-mark.svg -o flux-mark.png
```
