---
id: C-491
title: Present the Flux ecosystem from the release-matched local docs
pillar: Core
status: done
design: docs/designs/interactive-flux-presentation.md
areas: [docs, website]
note: "A dev+SRE deck: Flux's runtime boundary, a guarded local demo, connectors, Exchange, and honest shipped-vs-direction status. Shipped as 15 minutes/ten chapters in 0.53.0; C-540 grew it to ~20 minutes/thirteen."
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
- [x] Connector and Exchange slides distinguish shipped behavior from direction and name the dated
      upstream snapshots. (Amended 2026-08-05: the original text also demanded the then-missing
      Flux-to-Exchange wiring be stated as missing; C-503 shipped that seam, so the deck now states
      it as shipped — the enduring requirement is shipped-vs-direction honesty, not the 2026-08-03
      gap list.)
- [x] `/console/` links to the deck; hosted mode remains editor-only while loopback `flux docs`
      retains the existing scratch execution posture.
- [x] C-455's ecosystem summary/design drift is corrected, its website mirror and both changelogs
      are synchronized, and the embedded docs bundle regenerates deterministically. (Amended
      2026-08-05: the gate is green on this story's footprint — website contract, embedded route,
      codegate, and the workspace minus one provably foreign in-flight C-518 test target in
      flux-capabilities; see Progress.)

## Progress

- 2026-08-03: Story and design opened from the owner-approved implementation plan.
- 2026-08-03: Captured the embedded route failing first, shipped the ten-slide deck and console
  entry point, refreshed the ecosystem source/design, regenerated both mirrors and the binary
  bundle, and passed the product smoke plus every repository check except workspace clippy.
  Workspace clippy's only failure is an unrelated concurrent A-147 change in `flux-agent`; both
  crates touched here pass clippy. Keep the story open until that shared-worktree gate is green.
- 2026-08-05: Amended acceptance for the post-C-503 truth (the deck must state the shipped
  Exchange Service Account seam, not list it as missing) and handed deck evolution to C-540, which
  refreshed the snapshots to connectors v0.20.0 / exchange v0.17.0 and grew the deck to thirteen
  chapters. Closing this story rides on the same full-gate run.
- 2026-08-05: Closed. Ecosystem source/mirrors verified in sync (website_in_sync 6/6), both
  changelogs carry the deck's entries, the embedded bundle passed its determinism check, and the
  website contract suite is 34/34. The original A-147 clippy blocker is long gone; today's only
  red is a different, equally foreign in-flight C-518 change in flux-capabilities (E0308 in its
  test target; excluded run of the remaining workspace: 208 suites green).

## Notes

- This is presentation and documentation work over L-127/L-128's shipped surface. It must not add a
  second docs server, presentation dependency, or executable example manifest.
