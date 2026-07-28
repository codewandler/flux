---
id: D-194
title: "Flow package registry — flux flow install (epic)"
pillar: Language
status: backlog
priority:
epic: flow-package-registry
design:
note: "EPIC — flows/journeys are shareable artifacts with no distribution story; reuse the signed plugin-pack channel (D-46/D-47: signed index, sha256, versioned store) for .flux flow packages: flux flow install <name> fetches into ~/.flux/flows/, flow_list surfaces them, the analyzer runs at install time so a broken pack fails at install"
---

# Flow package registry — flux flow install (epic)

## Goal
Give the language pillar a distribution story: `.flux` flow packages fetched, verified, and
installed exactly like the plugin pack — `flux flow install <name>[@version]` resolves a signed
release index, sha256-checks the archive, unpacks into a versioned `~/.flux/flows/` store, and the
analyzer validates every flow at install time so a broken package fails at install, not at 2am.

## Acceptance
- [ ] `flux flow install <name>[@version]` fetches through a minisign-signed index with sha256
  verification (the D-47 trust ladder, no skip flag) into a versioned store — hermetic test against
  a local fixture release.
- [ ] The analyzer runs over every flow in the package at install time; any diagnostic aborts the
  install with the diagnostics printed — failing-first test with a deliberately broken package.
- [ ] Installed flows surface through `flow_list`/`flow_run` alongside project-local flows, with
  provenance visible (name@version, source).
- [ ] `flux flow list --installed` / uninstall round-trip.
- [ ] The release side: a pipeline (or documented manual path) that packages and signs a flow
  package, mirroring the plugin-pack pipeline (D-46).

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

## Notes
- Reuses the proven plugin distribution machinery; a flow package is strictly simpler (no
  per-target binaries).
- Later: a community index; out of scope for v1.
