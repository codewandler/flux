---
id: C-150
title: More TUI themes — additional palettes and a high-contrast variant
pillar: Core
status: done
epic: tui-polish-round-2
design:
note: "Theme::names() is exactly [dark, light, mono] (theme.rs:136-138) while every role is a plain Color field and by_name already fans out truecolor/NO_COLOR — extra palettes are data plus one array entry"
---

# More TUI themes — additional palettes and a high-contrast variant

## Goal
`Theme` is fully parameterized (13 `Color` roles) and `Theme::by_name` already handles the
truecolor and `NO_COLOR` fan-out (`theme.rs:140-163`), yet only three names exist
(`theme.rs:136-138`). Adding two or three palettes plus a high-contrast variant is nearly pure data
and is the cheapest appeal win in the TUI — high-contrast in particular, because `MONO` sets every
role to `Color::Reset` (`theme.rs:119-133`) and leans entirely on modifiers and glyphs.

## Acceptance
- [ ] At least two new named palettes plus a `high-contrast` variant are selectable, resolved
      through `Theme::by_name` with the existing truecolor/`NO_COLOR` precedence — failing-first
      unit test in the `theme_by_name_resolves_variants` family.
- [ ] `/theme` lists the new names and switching persists through the existing
      `flux_config::render_theme` + `persist_user_theme` path (C-104) — no new persistence code.
- [ ] Every new palette defines all `Theme` roles including `text`/`base_bg`, so the root fill
      stays correct on a mismatched terminal background.
- [ ] `NO_COLOR` still forces MONO for every new name.

## Progress
- 2026-07-29: Implemented. Added three new named palettes to `crates/flux-tui/src/theme.rs`:
  `dracula` and `nord` (each with an ANSI/256-color const and a `_RGB` truecolor tuning, mirroring
  `DARK`/`DARK_RGB` and `LIGHT`/`LIGHT_RGB`), plus `high-contrast` (`HIGH_CONTRAST` /
  `HIGH_CONTRAST_RGB`) — an accessibility palette distinct from `MONO`: it keeps maximal-saturation
  color rather than dropping to no color, with a forced pure-black `base_bg` so contrast doesn't
  depend on the terminal's own background or a remapped ANSI palette. Unlike `DARK`/`DARK_RGB`
  (which leave `base_bg = Reset`), all three new palettes paint an explicit non-`Reset` `base_bg`
  so they read correctly on a mismatched terminal background, per Acceptance. `Theme::names()` now
  returns `[dark, light, dracula, nord, high-contrast, mono]`; `Theme::by_name`'s `NO_COLOR` branch
  was refactored from a hardcoded name list to `Self::names().contains(&name)` so future palettes
  can't forget the MONO fallback. No changes were needed to `/theme`, `flux_config::render_theme`,
  or `flux_runtime::metadata::persist_user_theme` — both are already name-agnostic (C-104).
  Failing-first test `theme_by_name_resolves_variants_new_palettes` added in
  `crates/flux-tui/src/lib.rs` (verified it failed on `names() missing dracula` before the
  palettes existed). Checked `website/docs` and `docs/roadmap.md` for a user-facing theme list to
  update; the only enumeration found is a dated `whats-new.md` changelog entry for an already-
  shipped release (historical, left as-is per the non-goal in this story's Notes).
  Gate: `cargo test -p flux-tui` (148 passed), `cargo clippy -p flux-tui --all-targets -- -D
  warnings` (clean), `cargo fmt -p flux-tui -- --check` (clean) — all green. Note: this shared
  tree had five other agents editing `flux-tui/src/lib.rs` concurrently during this work, so
  several intermediate gate runs failed on unrelated in-flight code (missing `gutter_style`/
  `prepend_gutter`, a transient `Assistant::done` miss, a `bool::then` clippy lint) — none in this
  story's diff; retried until the tree stabilized.

## Notes
- Deliberate non-goal: OSC 11 terminal-background detection for auto light/dark. It needs a
  terminal query round-trip and is out of proportion to the payoff; a high-contrast palette is the
  better spend.
- Seams: `theme.rs:136` (`names`), `theme.rs:142` (`by_name`), `TuiRunOptions.theme`.
