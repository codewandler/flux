---
id: C-501
title: "Align Flux documentation with connector-native integrations"
pillar: Core
status: ready
priority: 1
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "reopened by C-508: remove the superseded local/hosted placement story and document Exchange as the only official integration executor"
---

# Align Flux documentation with connector-native integrations

## Goal

Make Flux's canonical and public documentation state the Exchange-only official-integration path and
the honest current compatibility gap, with a roadmap that points to every implementation and
retirement story.

## Acceptance

- [ ] `docs/vision.md`, `docs/ecosystem.md`, `docs/roadmap.md`, the root README and their website
      mirrors agree that Flux's embedded Exchange client is the only future official integration
      path; Flux itself executes no connector runtime and has no plugin fallback.
- [ ] Current plugin commands remain documented as temporary compatibility behavior without implying
      that their protocol, installer, signed pack, or release artifacts survive C-506.
- [ ] The roadmap links the cross-repository source of truth and names the corrected C-500…C-506
      sequence: embedded HTTP path, lifecycle later, per-adapter proof/deletion, then unconditional
      plugin-infrastructure removal.
- [ ] The docs distinguish Flux remaining usable without Exchange for core capabilities from official
      external integrations becoming unavailable when Exchange is unavailable.
- [ ] Engineering and customer changelogs record the correction without claiming the embedded client,
      an adapter migration, or zero-plugin release already ships; docs/website gates pass.

## Progress

- 2026-08-03: Done after cross-repository and worktree audit; contributor and public mirrors now
  distinguish the compatibility plugin fleet from the accepted connector destination.
- 2026-08-03: Reopened after a public-site review found that `plugins/using-plugins.md` still
  presented the pack without its migration status and the Direction sidebar exposed no page for the
  program. The broader Ecosystem, Vision, Roadmap, README and topology pages were already aligned.
- 2026-08-03: Closed the missed public-site gap: Plugins now labels the pack as the current
  compatibility path, Direction publishes ownership/current-state/cutover gates, the customer
  changelog mirror is current, and two deterministic embedded-site builds agree.
- 2026-08-03: Reopened by C-508 because flux-roadmap Decision 0001 superseded the local/hosted
  placement architecture this story previously documented.

## Notes

- Documentation-only; no failing-first behavioral test applies.
