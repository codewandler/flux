---
id: C-197
title: "The public site follows the brand spec — favicon, logo, social card, accent"
pillar: Core
status: done
priority: 9
epic: website-truth-and-identity
design: docs/designs/website-truth-and-identity.md
note: "docusaurus.config.js has no favicon key at all, no navbar logo and no social card, so every shared link renders blank; the accent #275a68 appears in no brand asset while assets/README.md fixes the palette at #0bbf83/#2be6a5"
---

# The public site follows the brand spec — favicon, logo, social card, accent

## Goal
[`assets/README.md`](../../assets/README.md) is a written brand spec — one visual idea (*plans
cross an explicit execution gate*), named production assets, a fixed palette, and an explicit list
of forbidden treatments. The README hero and its badges follow it; the website follows none of it.
Make the public site the same product as the README, without regressing text contrast.

## Acceptance
- [x] `website/docusaurus.config.js` sets `favicon`, `themeConfig.navbar.logo`,
      `themeConfig.image` (social card) and `themeConfig.metadata` — none of which existed before.
- [x] Assets are in `website/static/img/`, derived from `assets/`. The legacy `logo-*.png`,
      `icon.png` and `avatar.png` explorations are **not** used (`assets/README.md` says so).
- [x] The social card is a **1200×630 raster** derived from `readme-hero.svg`.
- [x] `--ifm-color-primary` is the brand signal in both themes: `#2be6a5` verbatim in dark; a
      darkened, WCAG-AA-verified ramp in light (`#08855b`, measured **4.65:1** on white). Raw
      `#0bbf83` survives as `--flux-signal` for non-text accents only.
- [x] Contrast checked against rendered surfaces — link text, both button styles, the active
      sidebar item — in a real browser, not just the swatch.
- [x] The `Inter` question is resolved: **removed** from the stack (see Progress for why).
- [x] `themeConfig.prism.theme` and `darkTheme` are both set explicitly.
- [x] `website/src/pages/index.js` leads with the brand lockup instead of the bare word "flux",
      with the accessible name preserved via `alt`.
- [x] The landing page no longer presents the Improvement pillar as co-equal and shipping.
- [x] No gradient, drop shadow, glow, mascot or orbital decoration on the mark or wordmark. The
      pre-existing shadow on the hero *code block* is not the identity and was left alone.
- [x] `npm run build` clean; verified by eye in both themes.

## Progress
- **Palette derived, not eyeballed.** Scripted the ramp: took the lightness deltas of the outgoing
  petrol ramp (so the new one keeps the same visual rhythm), re-anchored them on the brand hue, and
  searched down the lightness axis for the first value clearing 4.5:1 on white. Result `#08855b` at
  **4.65:1**; raw `#0bbf83` measures **2.39:1** and would have failed AA as link text. Dark mode
  uses `#2be6a5` verbatim — **10.62:1** on the Docusaurus dark background. Kept the neutrals
  untouched: they were already brand ink, which is the evidence the teal accent was drift.
- **Two variants per logo instead of the adaptive SVG.** `assets/flux-mark.svg` and `flux-logo.svg`
  switch on `@media (prefers-color-scheme: dark)`, which follows the **OS**, not the site's theme
  toggle — a visitor on a light OS who toggles the site to dark would get dark ink on a dark field.
  Generated explicit `flux-{mark,logo}-{light,dark}.svg` from the same paths and used Docusaurus's
  `srcDark` / `ThemedImage`, which key off the site theme. `favicon.svg` deliberately keeps the
  `prefers-color-scheme` form — it renders in browser chrome, where the OS scheme *is* the right
  signal.
- **Favicon.** `favicon.svg` is a square 256×256 framing of the mark (the mark is 216×113, so it
  needed centring, not cropping). `favicon.ico` carries 16/32/48. `rsvg-convert` does not evaluate
  `prefers-color-scheme`, so the raster baked the light palette — which is exactly what
  `assets/README.md` prescribes ("the PNGs contain the light-mode palette"). Verified by compositing
  the 32px layer over light and dark fields: it reads well on light chrome and is low-contrast on
  dark. Accepted, because the ICO is only the fallback — `headTags` lists the adaptive SVG *after*
  it, and every browser with a dark tab strip supports SVG favicons. `apple-touch-icon.png` gets a
  solid `#141a18` field because iOS composites transparency to black.
- **Inter: removed, not self-hosted.** It was declared at `custom.css:9` with no `@font-face` and no
  stylesheet link, so it had never loaded — the site has always rendered in the system stack. The
  deciding argument for dropping rather than adding it is in the brand asset itself:
  `flux-logo.svg` draws the wordmark as paths and comments *"no webfont dependency."* Adding a
  ~100KB download to match a name that was never honoured would move the site away from the brand's
  own stance, and away from the local-first reasoning that picked offline search over Algolia.
- **Landing page cards.** Replaced the "Improvement" card with **Security** rather than rewording
  it. A landing card that says "paused" is worse than not having one, and the four cards now name
  four surfaces that are all shipped and documented (Security has seven complete pages). The
  Improvement pillar stays reachable from the sidebar and the footer, and C-203 makes the page
  itself state its status.
- **Bug found and fixed during visual verification.** The first dark/light pass rendered the
  wordmark **twice**, ghosted. Cause: `ThemedImage` hides the inactive variant with
  `.themedComponent{display:none}` (specificity 0,1,0) and the new `.home-wordmark img` rule
  (0,1,1) set `display:block`, overriding it. Dropped `display` from the rule; the reason is now a
  comment there so it does not get re-added.
- **Prism verified, not assumed.** The dark-theme screenshot showed a *light* code block, which
  looked like a wiring bug. It was not: `usePrismTheme` resolves from `colorMode` at render time, so
  the server-rendered HTML always carries the light theme and hydration corrects it — the headless
  capture was pre-hydration. Confirmed by temporarily forcing `defaultMode: 'dark'` and re-checking
  the built HTML, which then emitted palenight's `#292d3e`; the probe was reverted. This is ordinary
  Docusaurus behaviour and needs no workaround.
- **Two changes beyond the original scope**, both small and both flagged here rather than done
  silently: `colorMode.respectPrefersColorScheme: true` (the site had always forced light on first
  visit), and `onBrokenAnchors: 'throw'`. The latter immediately caught a **real pre-existing
  break** — `sdk/flow-client.md:163` pointed at `tooling.md#flux-flow-list--run--discover-and-execute-saved-flows`,
  an anchor that stopped existing when that heading was reworded to `## flux flow list / run`.
  Fixed, and the build is now clean with zero warnings.
- Gate: `npm run build` clean under both `onBrokenLinks: 'throw'` and `onBrokenAnchors: 'throw'`.
  Visually verified in Firefox at 1280px in light and dark, landing page and a docs page. No Rust
  files touched, so no cargo gate was required for this story.

## Notes
- The neutrals are already correct and should not be touched: `--flux-hero-heading: #18201d` and
  the dark-mode `#141a18` are brand ink. Only the primary ramp is off-brand.
- Verification here is visual and cannot be fully automated — `npm run serve`, both themes, a
  narrow viewport. Check the emitted `<meta property="og:image">` for the card.
- Related: `website/static/img/architecture_v0.png` is already a copy of the file in `assets/`, so
  copying rather than symlinking is the established convention.
