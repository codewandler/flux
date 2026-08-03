---
id: C-507
title: "A malformed compaction environment override is silent on served agents"
pillar: Core
status: ready
priority: 8
areas: [flux-app, flux-agent]
note: "spun out of C-466: the CLI warns when FLUX_COMPACT_CHARS is malformed, but compact_threshold_for_decl silently drops the same typo on served/agentic agents and uses the default"
---

# One typo, two operator experiences

## Goal

Make a malformed `FLUX_COMPACT_CHARS` visible on the served/agentic path, so an operator is never
told that one override contract exists while a long-running host silently ignores it.

## The finding

The CLI's compaction resolver warns and falls back when `FLUX_COMPACT_CHARS` is present but not a
number. `crates/flux-app/src/app.rs::compact_threshold_for_decl` instead calls
`.ok().and_then(|s| s.parse().ok())`, collapsing an absent variable and an invalid explicit value
into the same silent default. Thus `FLUX_COMPACT_CHARS=48k` is observable on `flux run` but invisible
on a served agent, even though both surfaces document the same knob and precedence.

## Acceptance

- [ ] A failing-first test captures diagnostics from the served resolver and proves that a malformed
      explicit environment value emits one warning naming `FLUX_COMPACT_CHARS`, the rejected value,
      and the fallback default.
- [ ] Missing, valid, and `0` values remain quiet; `0` continues to disable compaction.
- [ ] Per-agent `settings.compact_threshold_chars` still wins over the environment, including `0`,
      and does not warn about an environment value it never consults.
- [ ] CLI and served paths share one parse/outcome contract or carry a test that makes any remaining
      diagnostic difference deliberate rather than accidental.

## Progress

- Filed 2026-08-03 from C-466's required adjacent-divergence disposition. No implementation yet.

## Notes

- The fallback value itself no longer drifts: C-466 made the CLI read
  `flux_agent::DEFAULT_COMPACT_THRESHOLD_CHARS`, while this served path already did.
- Related: [C-441](C-441-context-management-doc.md),
  [C-466](C-466-compact-threshold-default-drifts.md).
