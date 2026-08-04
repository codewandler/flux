---
id: C-518
title: "Close Usage Observatory accessibility, performance, and entry points"
pillar: Core
status: backlog
epic: usage-observatory
note: "Wire navigation and help, preserve live /usage, and prove the complete observatory accessible and responsive"
---

# Close Usage Observatory accessibility, performance, and entry points

## Goal

Make the complete observatory discoverable, accessible, and release-ready. Preserve C-140's live
session usage surface, expose historical replay as an explicit mode, and prove the integrated seven-day
experience remains truthful and responsive.

## Acceptance

- [ ] A failing-first navigation test named `usage_help_reaches_live_and_historical_modes` proves the
      shipped TUI and help expose both C-140's live per-session `/usage` behavior and the historical
      Usage Observatory as clearly labelled modes; neither silently replaces the other.
- [ ] Keyboard-only operation covers entry, exit, window selection, play/pause, restart, seek, speed,
      grouping, filtering, sorting, comparison, and inspection with visible focus and documented keys.
- [ ] The integrated observatory uses the existing theme, remains understandable in monochrome, and
      honors reduced-motion/no-animation operation. A state/snapshot matrix covers wide, medium,
      compact, empty, unknown-provider, partially-priced, burst-heavy, paused, backward-seek, and
      range-change states.
- [ ] A failing-first end-to-end fixture named `seven_day_observatory_stays_bounded_and_responsive`
      covers all four harnesses and proves bounded memory, buckets, visible pulses, and redraw work while
      interaction remains responsive under the repository's documented test threshold.
- [ ] Integrated totals and previous-period comparisons remain byte-for-byte consistent with C-513
      through C-515 projections across pause, seek, speed, layout, and reduced-motion changes; reported,
      estimated/subscription-equivalent, and unpriced coverage is visible in every dollar surface.
- [ ] A metadata-only sentinel test proves replay and inspection never load prompt, assistant, tool-
      argument, or transcript-body content.
- [ ] User-facing help/documentation explains windows, controls, attribution limits, timestamp precision,
      cost provenance, unknown/unpriced states, coalesced `×N` pulses, and historical pricing basis.
- [ ] The standard workspace build, test, clippy, fmt, and `flux-codegate` gates are green before C-512
      closes, with any additional TUI snapshot checks included in the normal test path.

## Progress

- (not started)

## Notes

- Depends on [C-517](C-517-deterministic-usage-replay-and-animation.md) and closes
  [C-512](C-512-usage-observatory-epic.md) only after every child is done or explicitly retired with a
  recorded reason.
