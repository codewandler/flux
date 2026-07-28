---
id: C-150
title: More TUI themes — additional palettes and a high-contrast variant
pillar: Core
status: backlog
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
- (not started)

## Notes
- Deliberate non-goal: OSC 11 terminal-background detection for auto light/dark. It needs a
  terminal query round-trip and is out of proportion to the payoff; a high-contrast palette is the
  better spend.
- Seams: `theme.rs:136` (`names`), `theme.rs:142` (`by_name`), `TuiRunOptions.theme`.
