---
id: C-498
title: "Embedded docs drift under the GitHub Pages environment"
area: Docs
status: done
priority: 1
areas: [docs, release, ci]
note: "found by the v0.54.0 main workflow: GITHUB_PAGES=true rebuilds a different server archive, while the release candidate gate did not refresh the docs changed in the release"
---

# Embedded docs drift under the GitHub Pages environment

## Goal

Make the documentation archive embedded in `flux-server` a deterministic function of the website
source, independent of GitHub Pages runner variables, and ship the refreshed archive.

## Acceptance

- [x] **Failing first:** `GITHUB_PAGES=true scripts/build-embedded-docs.sh --check` reproduces the
      stale-archive failure from website workflow run 30780486792.
- [x] The embedded-docs builder explicitly excludes the Pages-only environment from its Docusaurus
      subprocess; builds with `GITHUB_PAGES` set and unset produce the same archive.
- [x] `public-docs.zip` is regenerated from the v0.54 documentation and the website workflow is
      green on main.
- [x] A patch release rebuilds all platform artifacts from the corrected committed archive and its
      release/publish workflows complete successfully.

## Progress

- 2026-08-03: the exact CI failure was reproduced locally. A normal rebuild also differs from the
  committed archive, proving the release docs themselves were stale rather than the runner variable
  being the only cause.
- 2026-08-03: v0.54.0 is recorded as intentionally unshippable rather than bypassing the verifier
  to publish known-stale binaries; v0.54.1 is the release-current rebuild.
- 2026-08-03: v0.54.1's tag workflow exposed the remaining ordering bug: the cutter rolled the
  website changelog after the one-time archive refresh. The cutter now regenerates, verifies,
  snapshots and commits the archive after all release-owned website edits; v0.54.2 will carry the
  corrected invariant.
- 2026-08-03: exact-SHA candidate run 30784525192 rebuilt and attested every platform archive;
  release run 30787166399 promoted v0.54.2 and crates run 30787166427 published its complete crate
  closure. The public release is https://github.com/codewandler/flux/releases/tag/v0.54.2.
