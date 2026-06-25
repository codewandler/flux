# Flux brand assets

Logo, icon, and mascot artwork for the **Flux** / *flux-flow* project. The six logo/icon PNGs have a
**transparent** background so they embed cleanly on any surface. `avatar.png` is a full dark
illustration (its background is part of the artwork, not transparency).

The shared motif is the **flow graph** — input nodes flowing through a transform (a `λ` / arrow)
into an output square — representing Flux's typed-AST, LLM-as-compiler execution model.

## Images

| Preview | File | Dimensions | Background | Purpose / when to use |
| --- | --- | --- | --- | --- |
| <img src="logo-neon.png" alt="logo-neon" width="150"> | `logo-neon.png` | 1254×1254 | Transparent | **Primary hero** (used in the project `README.md`). Icon-only glowing neon flow mark with a lambda. Reads best on **dark** surfaces — the neon strokes are light, so it can look faint on white. |
| <img src="logo-horizontal.png" alt="logo-horizontal" width="150"> | `logo-horizontal.png` | 1448×1086 | Transparent | Default horizontal logo: circular flow icon (3 nodes + dotted arrow → square) left of the "Flux / flux-flow" wordmark. Use in docs headers, README badge rows, light backgrounds where the full name should read. |
| <img src="logo-horizontal-dashed.png" alt="logo-horizontal-dashed" width="150"> | `logo-horizontal-dashed.png` | 1448×1086 | Transparent | Same horizontal lockup with an open **dashed-arc** flow icon. Airier, lighter feel — alternative when the solid-circle icon feels too heavy. |
| <img src="logo-horizontal-bold.png" alt="logo-horizontal-bold" width="150"> | `logo-horizontal-bold.png` | 1448×1086 | Transparent | Horizontal lockup with a **bold solid-circle** icon and straight arrow. Use where a stronger, simpler mark is wanted (small sizes, favicons-with-text, print). |
| <img src="logo-stacked.png" alt="logo-stacked" width="150"> | `logo-stacked.png` | 1254×1254 | Transparent | Flow icon centered **above** the wordmark. Use in square/portrait spaces — app stores, centered hero blocks, avatars that still need the name. |
| <img src="icon.png" alt="icon" width="150"> | `icon.png` | 1254×1254 | Transparent | Clean cyan→purple flow mark with no wordmark. Source for favicons, app icons, and inline marks on any background. |
| <img src="avatar.png" alt="avatar" width="150"> | `avatar.png` | 1254×1254 | Dark (scene) | Friendly Flux assistant/agent character (robot with headphones + flow accents). Use as the agent persona avatar in chat UIs, social profiles, and "meet the agent" sections — **not** as the project logo. |

> The six logo/icon files were delivered flattened (a fake checkerboard painted in place of a real
> alpha layer); transparency here was recovered by color-keying the background out. For pixel-perfect
> edges, re-export them with a true alpha channel from the original design source.

## Choosing one

- **Logo on a dark surface →** `logo-neon.png`
- **Logo on a light surface, name visible →** `logo-horizontal.png` (or the `-dashed` / `-bold` variants)
- **Square / centered slot →** `logo-stacked.png`
- **Favicon / app icon / inline mark →** `icon.png`
- **Representing the agent itself →** `avatar.png`
