---
id: C-501
title: "Align Flux documentation with connector-native integrations"
pillar: Core
status: done
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "make the accepted runtime-axis destination explicit: Flux owns generic execution, not official vendor adapters; local and Exchange placements serve the same connector"
---

# Align Flux documentation with connector-native integrations

## Goal

Make Flux's canonical and public documentation state the accepted destination and the honest current
gap, with a roadmap that points to every implementation and retirement story.

## Acceptance

- [x] `docs/vision.md`, `docs/designs/ecosystem.md`, `docs/ecosystem.md`, `docs/roadmap.md`, the root
      README and their website mirrors agree on repository ownership and local/hosted placement.
- [x] Current plugin commands remain documented as compatibility behavior, while the future official
      integration source is unambiguously flux-connectors.
- [x] The docs distinguish generic plugin protocol support—which may remain—from vendor-specific
      official plugin crates—which migrate and are removed.
- [x] The roadmap names C-500 and C-502…C-506 plus the existing/pending substrate, channel and
      approval work it consumes.
- [x] Engineering and customer changelogs record the direction without claiming any new executor or
      migrated connector ships in this change; docs/website gates pass.

## Progress

- 2026-08-03: Done after cross-repository and worktree audit; contributor and public mirrors now
  distinguish the compatibility plugin fleet from the accepted connector destination.

## Notes

- Documentation-only; no failing-first behavioral test applies.
