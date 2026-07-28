---
id: C-104
title: TUI theme system — truecolor dark, light, mono + /theme
pillar: Core
status: done
priority: P2
design: tui-polish
epic: tui-polish
areas: [flux-tui, config]
note:
---

# TUI theme system — truecolor dark, light, mono + /theme

## Goal
The TUI has exactly one ANSI dark theme; light terminals get dark indexed greys and `NO_COLOR`
doesn't disable theme colors. Add truecolor dark, light, and mono palettes, a `/theme` command,
and persistence.

## Acceptance
- [x] `Theme::DARK_RGB`, `LIGHT`, `LIGHT_RGB`, `MONO` + `Theme::by_name(name, truecolor, no_color)`
      (NO_COLOR forces MONO; truecolor picks the `_RGB` variant) — unit-tested.
- [x] `/theme` lists themes + current; `/theme dark|light` switches live (transcript revision bump
      re-styles cached entries) — TestBackend test asserts a known cell's fg/bg changes.
- [x] Root background fill from `Theme.base_bg` so LIGHT is usable on dark terminals
      (Reset for DARK — no behavior change there).
- [x] Persisted: `flux_config::Config.theme: Option<String>` (additive, serde round-trip test) +
      `persist_user_theme` in flux-runtime following the `persist_allow_rules` read-merge-atomic-
      rename pattern; `TuiRunOptions.theme` threaded from `run_tui`.

## Progress
- Done 2026-07-28: DARK_RGB/LIGHT/LIGHT_RGB/MONO + Theme::by_name; /theme lists+switches+persists via flux_config::render_theme + flux_runtime::metadata::persist_user_theme (~/.flux/config.toml); Theme gained text + base_bg roles (root fill); TuiRunOptions.theme threaded from run_tui. Tests: theme_by_name_resolves_variants, theme_switch_restyles_screen, theme_field_round_trips_and_renders (flux-config).

## Notes
- Seams: `theme.rs`, truecolor gate `lib.rs:100`, `TuiRunOptions` `lib.rs:73`,
  `flux-runtime/src/metadata.rs:55`, `flux-cli/src/app_cmd.rs:596`.
- `TuiRunOptions.theme` pub field = MINOR break (batch with C-103).
- Known gap: flux_markdown hardcodes span colors — may clash with LIGHT; file follow-up if it bites.
