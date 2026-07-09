---
id: C-47
title: Release-publication reliability — a tag must yield a downloadable GitHub Release
pillar: Core
status: blocked
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
- [x] Root-cause why recent tags did not produce a GitHub Release object (cargo-dist `release.yml`
      upload/announce step failing or being skipped) and fix or add a retry so a tagged version
      reliably yields a Release with assets.
- [x] A post-tag check (CI step or a `scripts/` verification) confirms a Release object exists for
      the tag and fails the pipeline if it does not — no silent "tag but no Release".
- [ ] Backfill the missing Release object(s) for the affected recent tag(s) so `/releases/latest`
      matches the newest shipped version.
- [x] Documented in the release runbook alongside the crates.io publish flow.

## Progress
- 2026-07-09: Root cause confirmed from failed Release run logs for `v0.9.3` and `v0.11.0`: the
  hand-written `gh release create` step returned `HTTP 403: Resource not accessible by integration`
  when it fell back to `GITHUB_TOKEN`. `v0.9.3` still has a tag and no GitHub Release; `v0.11.0` was
  already manually backfilled and verifies with 16 assets.
- 2026-07-09: Workflow fix landed locally: tag publishes now require `RELEASE_TOKEN`, release
  creation is retry/idempotent, and `scripts/verify-github-release.sh` verifies installer,
  checksum, and platform archive assets. Runbook updated in `crates/flux-sdk/PUBLISHING.md`.
- 2026-07-09 BLOCKED: attempted to backfill `v0.9.3` from failed-run artifacts (`28933549554`), but
  local `gh release create` failed with `workflow` scope required. Active account token has
  `repo` but not `workflow`; a maintainer token with `workflow` scope must run the documented
  backfill command.

## Notes
- Release-ops, not part of the gitlab-plugin-hardening epic — filed standalone.
- Cross-references the crates.io publish workflow (`.github/workflows/crates-io.yml`) and the binary
  `release.yml`, both tag-triggered.
