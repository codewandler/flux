---
id: C-507
title: "A malformed compaction environment override is silent on served agents"
pillar: Core
status: done
priority: 8
areas: [flux-app, flux-agent, flux-cli]
note: "CLI and served agents now consume one flux-agent parse/outcome contract; malformed explicit values warn and fall back, while per-agent settings bypass the environment"
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

- [x] A failing-first test captures diagnostics from the served resolver and proves that a malformed
      explicit environment value emits one warning naming `FLUX_COMPACT_CHARS`, the rejected value,
      and the fallback default.
- [x] Missing, valid, and `0` values remain quiet; `0` continues to disable compaction.
- [x] Per-agent `settings.compact_threshold_chars` still wins over the environment, including `0`,
      and does not warn about an environment value it never consults.
- [x] CLI and served paths share one parse/outcome contract or carry a test that makes any remaining
      diagnostic difference deliberate rather than accidental.

## Progress

- 2026-08-03: added a failing-first served resolver test. It initially failed to compile because the
  injected environment/diagnostic seam did not exist.
- 2026-08-03: `flux-agent::resolve_compact_threshold_env` now owns the parse, fallback, and
  surface-neutral warning outcome. CLI and app render it in their own style.
- 2026-08-03: the served resolver takes a lazy environment callback internally, proving per-agent
  values (including `0`) return before the environment is read or a warning can be emitted.
- 2026-08-03: focused resolver/CLI/app tests and the full repository build, test, clippy, fmt, and
  codegate checks pass.

## Notes

- The fallback value itself no longer drifts: C-466 made the CLI read
  `flux_agent::DEFAULT_COMPACT_THRESHOLD_CHARS`, while this served path already did.
- The warning is one diagnostic per resolution. Agent engines are cached after construction; this
  story does not introduce process-global warning deduplication.
- Related: [C-441](C-441-context-management-doc.md),
  [C-466](C-466-compact-threshold-default-drifts.md).
