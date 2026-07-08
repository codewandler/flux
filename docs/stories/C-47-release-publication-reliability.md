---
id: C-47
title: Release-publication reliability — a tag must yield a downloadable GitHub Release
pillar: Core
status: backlog
priority:
epic:
design:
note: "N-001: `/releases/latest` reported an older version than the newest `vX.Y.Z` tag with no release object for the newer tag, so users asking for 'latest' get a stale binary — the release workflow can push a tag without producing the Release/assets (cf. the earlier v0.4.2 macOS-upload flake)"
---

# Release-publication reliability — a tag must yield a downloadable GitHub Release

## Goal
Guarantee that pushing a `vX.Y.Z` tag results in a GitHub Release object with downloadable binary
assets — or fails loudly — so "install the latest release" never silently serves a stale version.

## Why (evidence)
A beta retest found GitHub `/releases/latest` reporting an older version while `/tags` and
`origin/main` carried a newer `vX.Y.Z` tag with **no** Release object, so a prebuilt binary for the
newer version was undownloadable and testers asking for "latest" got the older one. This matches a
prior release flake (the v0.4.2 macOS-upload ENOTFOUND that skipped the announce/Release step). The
crates.io publish workflow now also fires on the same tag, so tag → published-artifacts reliability
matters on both the binary and crate paths.

## Acceptance
- [ ] Root-cause why recent tags did not produce a GitHub Release object (cargo-dist `release.yml`
      upload/announce step failing or being skipped) and fix or add a retry so a tagged version
      reliably yields a Release with assets.
- [ ] A post-tag check (CI step or a `scripts/` verification) confirms a Release object exists for
      the tag and fails the pipeline if it does not — no silent "tag but no Release".
- [ ] Backfill the missing Release object(s) for the affected recent tag(s) so `/releases/latest`
      matches the newest shipped version.
- [ ] Documented in the release runbook alongside the crates.io publish flow.

## Progress
- Not started.

## Notes
- Release-ops, not part of the gitlab-plugin-hardening epic — filed standalone.
- Cross-references the crates.io publish workflow (`.github/workflows/crates-io.yml`) and the binary
  `release.yml`, both tag-triggered.
