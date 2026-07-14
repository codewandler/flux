---
id: C-73
title: Promote release-candidate artifacts without rebuilding
pillar: Core
status: done
design: docs/designs/build-once-release-promotion.md
note: "prepare an exact release SHA once; a matching tag promotes its immutable workflow artifacts"
---

# Promote release-candidate artifacts without rebuilding

## Goal

Separate expensive release-candidate construction from the version-tag event. An explicitly
prepared release commit should build and verify the five-platform cargo-dist artifact set once; a
matching version tag should then publish those exact workflow artifacts without recompiling them.

## Acceptance

- [x] A manual candidate run validates the requested version against the workspace manifest, records
      the exact 40-character commit SHA, and builds the existing cargo-dist local and global artifact
      set without creating a GitHub Release.
- [x] A tag run locates only a successful candidate at the tag's exact commit, verifies its
      version/SHA/run receipt, and feeds that candidate's immutable artifacts into the existing
      cargo-dist host and GitHub Release verification steps.
- [x] Candidate/tag version or SHA mismatches fail closed before publication; a missing candidate is
      reported clearly and follows the documented compatibility fallback instead of silently
      promoting unrelated artifacts.
- [x] Promotion remains idempotent when a GitHub Release already exists, preserves the existing
      installer/checksum/platform-archive verification, and exposes the source candidate run in the
      workflow summary.
- [x] `scripts/test-release-candidate.sh` fails first against the absent receipt contract, then covers
      valid receipts plus malformed version, SHA, run ID, tampering, and mismatch cases.
- [x] The release runbook explains prepare → inspect → tag/push → promote, including retention and
      fallback behavior.

## Progress

- Mapped the cargo-dist plan, five-target local build, global checksum/installer build, host, upload,
  and post-upload verifier boundaries in `.github/workflows/release.yml`.
- Chosen design: the same workflow prepares and promotes, so candidate artifacts use the exact same
  cargo-dist version and build commands as the compatibility path.
- Added deterministic receipt creation/verification and an exact-SHA candidate finder whose hermetic
  fake-GitHub tests cover successful selection, expired/incomplete candidates, no match, invalid
  inputs, and API failure.
- Added manual preparation, 14-day artifact retention, tag-time receipt verification, cross-run
  artifact download, audit summaries, and an explicit legacy-build fallback to `release.yml`.
- Tightened candidate discovery to require the receipt, global artifacts, and all five local target
  artifacts to remain present and unexpired before promotion.
- Proved cargo-dist can plan the prospective (not-yet-pushed) tag with all five target builds, and
  validated the edited workflow with `actionlint` plus YAML and shell syntax checks.

## Notes

- The tag-triggered crates.io workflow remains independent and unchanged.
- `release.yml` is cargo-dist generated; the promotion customization must be re-applied after a
  future `dist generate`.
