---
id: C-491
title: Present the Flux ecosystem from the release-matched local docs
pillar: Core
status: in-progress
design: docs/designs/interactive-flux-presentation.md
areas: [docs, website]
note: "A 15-minute dev+SRE deck: Flux's runtime boundary, a guarded local demo, connectors, Exchange, and honest shipped-vs-direction status."
---

# Present the Flux ecosystem from the release-matched local docs

## Goal

Give a developer and SRE engineering team a speaker-ready, self-contained explanation of Flux,
flux-connectors, and flux-exchange from the same hosted and release-matched documentation artifact.
Let a local `flux docs` audience run one already-declared scratch example without adding any new
runtime authority.

## Acceptance

- [x] Failing-first `public_docs::version_and_embedded_entry_points_ship_together` proves the
      presentation ships in the binary's embedded documentation bundle.
- [x] `/flux/presentation/` is a keyboard- and pointer-navigable 15-minute deck with progress,
      fullscreen, URL-restored slide position, responsive layout, and reduced-motion behavior.
- [x] The deck explains the mandatory authorization → approval → guarded-IO path, reuses the
      existing `rust-files` workbench fixture, and adds no CLI, server, fixture, or runtime authority.
- [x] Connector and Exchange slides distinguish shipped behavior from direction, name the dated
      upstream snapshots, and state the missing Flux-to-Exchange and inbound Exchange paths.
- [x] `/console/` links to the deck; hosted mode remains editor-only while loopback `flux docs`
      retains the existing scratch execution posture.
- [ ] C-455's ecosystem summary/design drift is corrected, its website mirror and both changelogs
      are synchronized, and the embedded docs bundle plus the full repository gate pass.

## Progress

- 2026-08-03: Story and design opened from the owner-approved implementation plan.
- 2026-08-03: Captured the embedded route failing first, shipped the ten-slide deck and console
  entry point, refreshed the ecosystem source/design, regenerated both mirrors and the binary
  bundle, and passed the product smoke plus every repository check except workspace clippy.
  Workspace clippy's only failure is an unrelated concurrent A-147 change in `flux-agent`; both
  crates touched here pass clippy. Keep the story open until that shared-worktree gate is green.

## Notes

- This is presentation and documentation work over L-127/L-128's shipped surface. It must not add a
  second docs server, presentation dependency, or executable example manifest.
