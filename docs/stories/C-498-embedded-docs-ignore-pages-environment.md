---
id: C-498
title: "Embedded docs drift under the GitHub Pages environment"
area: Docs
status: in-progress
priority: 1
areas: [docs, release, ci]
note: "found by the v0.54.0 main workflow: GITHUB_PAGES=true rebuilds a different server archive, while the release candidate gate did not refresh the docs changed in the release"
---

# Embedded docs drift under the GitHub Pages environment

## Goal

Make the documentation archive embedded in `flux-server` a deterministic function of the website
source, independent of GitHub Pages runner variables, and ship the refreshed archive.

## Acceptance

- [ ] **Failing first:** `GITHUB_PAGES=true scripts/build-embedded-docs.sh --check` reproduces the
      stale-archive failure from website workflow run 30780486792.
- [ ] The embedded-docs builder explicitly excludes the Pages-only environment from its Docusaurus
      subprocess; builds with `GITHUB_PAGES` set and unset produce the same archive.
- [ ] `public-docs.zip` is regenerated from the v0.54 documentation and the website workflow is
      green on main.
- [ ] A patch release rebuilds all platform artifacts from the corrected committed archive and its
      release/publish workflows complete successfully.

## Progress

- 2026-08-03: the exact CI failure was reproduced locally. A normal rebuild also differs from the
  committed archive, proving the release docs themselves were stale rather than the runner variable
  being the only cause.
- 2026-08-03: v0.54.0 is recorded as intentionally unshippable rather than bypassing the verifier
  to publish known-stale binaries; v0.54.1 is the release-current rebuild.
