---
id: C-47
title: Release-publication reliability — a tag must yield a downloadable GitHub Release
pillar: Core
status: done
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
- [x] Backfill the missing Release object(s) for the affected recent tag(s) so `/releases/latest`
      matches the newest shipped version.
- [x] Documented in the release runbook alongside the crates.io publish flow.

## Progress
- 2026-07-29 **DONE.** Audited the whole fleet: 5 tags had no Release. Classified each from its
  workflow run rather than assuming they shared N-001's cause — and they did not:
  | tag | why no Release | artifacts | disposition |
  |---|---|---|---|
  | `v0.9.3` | `host` job HTTP 403 — **the true N-001** | all 5 platforms + global ✓ | **backfilled** |
  | `v0.11.1` | `plan` rejected the hand-edited `release.yml` as an out-of-date generated workflow | none | unshippable; `v0.11.2` released 16 assets |
  | `v0.12.0` | Windows build failed to compile `codewandler-flux-web`, no `flux.exe` | 6 partial | unshippable; `v0.12.1` released 16 assets |
  | `v0.17.0` | cargo-dist could not find bin `flux_sdk_plugin_fixture`; all 5 platforms failed identically (a config defect at that commit, not a flake — re-running fails the same way) | none | unshippable; `v0.17.1` released 16 assets |
  | `v0.2.7-9a02b56cc73a` | pre-0.3 dev tag, never a shipped version | — | out of scope (not `vX.Y.Z`) |
  Only `v0.9.3` was backfillable: the other three never produced a complete asset set, and a Release
  with partial assets advertises downloads that do not exist — strictly worse than no Release, and
  the same failure this story forbids. They are now recorded as a reasoned allowlist in
  `scripts/check-release-tags.sh` rather than as folklore.
- 2026-07-29 `v0.9.3` backfilled from run `28933549554`: all 6 archive checksums re-verified against
  their `.sha256` sidecars, the extracted linux binary confirmed `flux 0.9.3`, and the exact
  cargo-dist title/body recovered from `dist-manifest.json`'s `announcement_*` fields rather than
  hand-written. Published with the canonical 16 assets; `scripts/verify-github-release.sh v0.9.3`
  passes.
- 2026-07-29 **NEW DEFECT FOUND, BY CAUSING IT.** The backfill immediately flipped
  `/releases/latest` from `v0.33.0` to `v0.9.3` — i.e. performing this story's own documented
  runbook *reintroduced N-001*. Root cause: GitHub ranks `/releases/latest` by **`published_at`**,
  not by tag date or semver. It is invisible in `created_at`, which harmlessly inherits the tag date
  (this is why the earlier `v0.11.0`/`v0.12.1` backfills *looked* safe and misled the prediction
  that a backfill could not disturb `latest`). Repaired within the same session with
  `gh release edit v0.33.0 --latest`; `/releases/latest` is `v0.33.0` again.
  Neither `verify-github-release.sh` nor `release.yml` checked the latest pointer at all — the one
  invariant N-001 is actually about. Fixed structurally:
  - `scripts/check-release-tags.sh` (new) audits the **whole** tag/release fleet, not just the tag
    being cut, and asserts `/releases/latest` == the newest released version. Carries `--self-test`
    per the repo idiom, and exit 2 = "GitHub state unobtainable" is a logged skip so an outage does
    not turn `main` red.
  - Wired into `ci.yml` as the `release-tags` job on every push to `main`. This is the half
    `verify-github-release.sh` structurally cannot cover: a tag whose workflow dies *before* the
    verify step never runs the verify step, which is exactly how N-001 survived unnoticed until an
    external tester reported it.
  - `crates/flux-sdk/PUBLISHING.md`: the backfill recipe now passes `--latest=false`, warns about
    the `published_at` ranking, and tells the maintainer to confirm the run actually built before
    backfilling at all.
  Self-test proven non-vacuous by mutation: lexical sort instead of `sort -V` (the case that
  matters, `v0.9.3` vs `v0.33.0`) and dropping the allowlist skip both fail it.
- 2026-07-09 ROOT CAUSE CONFIRMED on v0.12.0/v0.12.1: the Release workflow's host step fails with
  `HTTP 403: Resource not accessible by personal access token` on POST /releases — the scoped
  RELEASE_TOKEN (cf1612f) has lost/lacks Contents:write (expired or mis-scoped fine-grained PAT).
  Every platform build succeeds; only release creation dies, reproducing N-001 exactly (tag with
  no Release object). MANUAL BACKFILL RUNBOOK PROVEN (v0.12.1): `gh run download <release-run-id>`
  -> `gh release create vX.Y.Z <16 assets>` with the local repo-scoped token ->
  `scripts/verify-github-release.sh`. UNBLOCK: maintainer must mint a fresh RELEASE_TOKEN with
  Contents: read+write on codewandler/flux (or revert the step to GITHUB_TOKEN with
  `permissions: contents: write`).
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
