---
id: C-101
title: CLI/TUI boot splash + animated spinner effects
pillar: Core
status: done
areas: [cli, flux-tui]
---

# CLI/TUI boot splash + animated spinner effects

## Goal
Give flux's interactive front doors the polish of the coder prototype: an animated FLUX
boot splash (matrix rain → block-logo reveal → glow) on bare `flux` and `flux tui`, and a
catalog of full-width truecolor spinner effects that replaces the braille glyph during
model waits — both Rust ports of the Go originals (agentsdk coder splash,
codewandler/spinners).

## Acceptance
- [x] `flux_tui::splash`: deterministic 60×12 frame sequence (seeded PCG32), phases pinned
      by `splash::tests::phases_advance_and_finish_deterministically`; FLUX glyph geometry
      pinned by `glyphs_are_nine_columns_and_logo_fits`.
- [x] Bare `flux` / prompt-less `flux run` play the splash before the banner; any key skips;
      auto-dismiss after ~1.9 s of glow; fully suppressed under NO_COLOR / `--color never`,
      any piped stream, `FLUX_NO_SPLASH`, or a terminal < 64×14
      (`splash::tests::splash_requires_color_tty_room_and_no_optout` in flux-cli).
- [x] `flux tui` plays the splash via ratatui frames before the event loop, then repaints.
- [x] `flux_tui::spinners`: 8 effects (Knight Rider default, Comet, Tidal Wave, Matrix,
      Equalizer, Aurora, Thunderstrike, Binary Rain), each a pure `fn(tick,width)` producing
      exactly `width` single-column cells (`every_effect_fills_exactly_the_requested_width`,
      `frames_are_deterministic_and_single_column`).
- [x] CLI thinking spinner shows the animated bar on truecolor terminals, cycling one effect
      per model round-trip; braille fallback elsewhere; tool lines keep braille. TUI footer
      likewise, cycling per execute round.
- [x] Demo: `cargo run -p flux-tui --example spinners`.

## Progress
- 2026-07-28 — implemented, tests green, `cargo fmt --check` clean in both workspaces,
  `cargo test --workspace` green. Done in one session; no follow-ups filed.

## Notes
- Ports: `~/projects/agentsdk/examples/coder/tui/splash/splash.go` (splash; word changed
  CODER→FLUX, tagline `[ deterministic agent platform ]`, input bar dropped) and
  `github.com/codewandler/spinners@v0.1.1` (catalog, curated 21→8).
- Both live in flux-tui as modules (`splash`, `spinners`) per user direction — flux-cli
  already depends on flux-tui; no new crate.
- Randomness is an embedded PCG32 (`Splash::new(seed)`), so frames are testable; the Go
  originals used ambient `math/rand`.
- Truecolor gating: CLI `style::truecolor()` (COLORTERM), TUI `terminal_truecolor()`.
