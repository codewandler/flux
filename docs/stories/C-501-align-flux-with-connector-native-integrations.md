---
id: C-501
title: "Align Flux documentation with connector-native integrations"
pillar: Core
status: done
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "public Plugins and Direction sections now join the canonical docs in naming the signed pack as compatibility and connectors as the gated destination"
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
- [x] The public Plugins landing page labels the signed first-party pack as a compatibility path,
      and Direction has a dedicated page stating the destination, migration gates and honest current
      boundary.

## Progress

- 2026-08-03: Done after cross-repository and worktree audit; contributor and public mirrors now
  distinguish the compatibility plugin fleet from the accepted connector destination.
- 2026-08-03: Reopened after a public-site review found that `plugins/using-plugins.md` still
  presented the pack without its migration status and the Direction sidebar exposed no page for the
  program. The broader Ecosystem, Vision, Roadmap, README and topology pages were already aligned.
- 2026-08-03: Closed the missed public-site gap: Plugins now labels the pack as the current
  compatibility path, Direction publishes ownership/current-state/cutover gates, the customer
  changelog mirror is current, and two deterministic embedded-site builds agree.

## Notes

- Documentation-only; no failing-first behavioral test applies.
