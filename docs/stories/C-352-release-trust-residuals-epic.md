---
id: C-352
title: "Release trust residuals — the authority half of REL-01 that no code change addressed (epic)"
pillar: Core
status: backlog
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "EPIC — main/release refs, a dedicated promotion App, separate control/publication environments and exact promotion closure remain open; C-353/C-354/C-355/C-516 block v0.56.0"
---

# Release trust residuals

## Goal

Give the release pipeline an authority and promotion model that matches the integrity model it
already has: constrain who can change and publish releases, authenticate the build-to-host handoff,
and make the exact public/latest state part of promotion success.

## Acceptance

- [ ] C-353 establishes protected `main`, cumulative creation/immutability tag rulesets, the
      repository-only `flux-release-promoter` App, separate `release-control` and `release`
      environments, and value-free evidence for App configuration and all four environment-secret
      names. Administrators bypass none of those protections.
- [ ] C-354 makes promotion, signing, GitHub Release publication and Cargo publication separate
      least-privilege jobs; plugin publication is tag-triggered; `RELEASE_TOKEN` is never used for
      promotion; model/build jobs have no write token.
- [ ] C-355 binds the exact seven artifact archives into candidate receipt v3 and verifies their raw
      ZIP bytes before safe extraction, hosting, attestation or publication.
- [ ] C-516 makes the cut merge through a green `ci` pull request to `main`, then binds the candidate
      and immutable tag to that merged SHA before exact-run waits, 28-asset live verification,
      latest-state audit and last-step candidate cleanup.
- [ ] C-356 makes attestation verification part of the documented primary install path and declares
      the first attested tag machine-readably.
- [ ] C-357 records bus factor and independent review as owned risks with a review date.
- [ ] The platform queries that produced this epic are re-run value-free and return the intended
      protection, environment, token-policy and governance state.

## Progress

- 2026-08-01 — opened from the validation pass. `gh api` answered every query, so ASSURE-04's
  "external-unknown" half became verified absent rather than unknown.
- 2026-08-04 — after PR #29, revalidated canonical `origin/main` at
  `9e3108b1b6856e30fa2e0baa2475d75d21fbc19f`: 26 PRs are merged with zero recorded reviews; one
  administrator remains; main protection, rulesets, `release-control` and `release` remain absent;
  Actions defaults remain write + workflow PR approval. C-516 was added for the promotion
  residuals. C-353/C-354/C-355/C-516 block v0.56.0 publication; C-356/C-357 do not.
- 2026-08-04 — the epic remains `backlog` with every acceptance box open because C-356 and C-357
  remain visible residuals after the v0.56.0 publication blockers close.

## Notes

- v0.56.0 is a checkpoint core release. C-509/C-510 own the separate clean-machine journey proof;
  this epic does not authorize a Milestone-1 product-release claim.
- GitHub exposes secret names and metadata, never values. C-353's environment migration cannot be
  automated from existing values; a secure maintainer must possess or replace the credentials.
- Dependency order is C-353 → C-354 → C-355 → C-516. All four stories remain `ready` with every
  Acceptance item open; this ordering is a fail-closed implementation sequence, not a readiness or
  completion claim.
