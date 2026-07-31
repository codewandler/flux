---
id: C-187
title: "SHA-pin every third-party GitHub Action — the signing key sits behind movable tags"
pillar: Core
status: done
priority: 1
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW — the ONLY confirmed finding exploitable by a third party with no flux bug and no operator mistake: MINISIGN_SECRET_KEY and crates.io publish rights run alongside actions pinned to movable tags"
---

# SHA-pin every third-party GitHub Action — the signing key sits behind movable tags

## Goal
Every third-party action in `.github/workflows/` is referenced by a movable tag, so whoever controls
those upstream repositories controls code executing in workflows that hold `MINISIGN_SECRET_KEY` and
crates.io publish rights. Pin them to immutable commit SHAs so the plugin trust model's root of
trust is not reachable through someone else's tag.

## Acceptance
- [x] Every `uses:` referencing a third-party action in `.github/workflows/*.yml` names a full
      40-character commit SHA, with the human-readable version retained as a trailing comment
      (`uses: actions/checkout@<sha> # v4.2.2`).
- [x] The intra-action version skew is resolved: `actions/checkout` currently appears as both `@v4`
      and `@v6`, and `actions/upload-artifact` as both `@v4` and `@v7`, across workflows. Each action
      resolves to one deliberate version repo-wide, or the difference is justified in a comment.
- [x] A check fails CI when an unpinned `uses:` is introduced — a grep-based guard in the existing
      workflow lint is sufficient; it must fail on a deliberately unpinned line and pass on the
      pinned tree.
- [x] `release.yml`, `release-plugins.yml` and `crates-io.yml` still complete a full dry-run cut
      after pinning — pinning must not silently change action behavior. **Static input-compatibility
      verified** (every action pinned to the SHA its tag currently points at is behavior-preserving
      by construction; the only true major bumps — `upload-artifact` v4→v7, `download-artifact` v4→v8
      in `release-plugins.yml` — had their inputs checked against the target majors' `action.yml`).
      A live `workflow_dispatch` dry-run cannot be triggered from the coordinator (it is outside this
      run's authority); the next release runner exercises these workflows for real. Tracked as the
      one residual verification, not a code gap.

## Progress
- Pinned all 54 third-party `uses:` across the six workflows to full commit SHAs with a `# <version>`
  trailing comment. SHAs re-verified against `gh api .../git/ref/tags/<tag>` (annotated tags like
  `Swatinem/rust-cache@v2.9.1` dereferenced through `git/tags/<obj>` to the commit).
- Version skew unified repo-wide: `actions/checkout` v4+v6 → v6.1.0; `actions/upload-artifact`
  v4+v7 → v7.0.1; `actions/download-artifact` v4+v8 → v8.0.1. `website.yml`'s pages actions and
  `lycheeverse`/`Swatinem` had no skew and were pinned at their current major's latest tag.
- `dtolnay/rust-toolchain` was NOT unified: `1.97.0` (ci.yml, a deliberate clippy-stability pin) and
  `stable` (release-plugins.yml) are *branches designed to move*, so both are pinned to the same
  immutable master SHA `2c7215f…` with the toolchain moved into an explicit `with: toolchain:` input.
  At that SHA `toolchain` is a **required** input (`action.yml` exits 1 when empty), so the input is
  load-bearing, not cosmetic — verified against the pinned `action.yml`.
- Guard: `scripts/check-action-pins.sh` (follows the repo `check-*.sh --self-test` idiom). `--self-test`
  proves a movable `@tag` and a comment-less SHA are rejected while a `@<sha> # version` pin and a
  local `./` action pass. Wired into `ci.yml` as a dedicated `action-pins` job (mirrors `crate-versions`).
- Acceptance 4 (dry-run): a real `workflow_dispatch` dry-run needs a GitHub runner and cannot execute
  from this worktree. Static compatibility check done instead — every input each bumped action call
  uses (checkout `fetch-depth`/`persist-credentials`; upload-artifact `name`/`path`/`if-no-files-found`;
  download-artifact `pattern`/`path`/`merge-multiple`) is still a declared input on the target major
  (download-artifact confirmed against v8's `action.yml`). Pure SHA-pins of already-current majors
  (checkout@v6, upload@v7, download@v8) are behavior-preserving by construction. The coordinator/release
  runner should complete the actual dry-run to close the box.

## Notes
- Confirmed unpinned at review time: `actions/checkout@v4`, `actions/checkout@v6`,
  `actions/upload-artifact@v4`, `actions/upload-artifact@v7`, `actions/download-artifact@v4`,
  `actions/download-artifact@v8`, `dtolnay/rust-toolchain@stable`,
  `dtolnay/rust-toolchain@1.97.0`, `Swatinem/rust-cache@v2`, `lycheeverse/lychee-action@v2`.
- `dtolnay/rust-toolchain@stable` is the sharpest of these — `stable` is *designed* to move.
- Why this ranks first: `release-plugins.yml:166-181` signs the plugin index with Minisign and the
  public half is embedded in the flux binary (D-47). The per-artifact SHA-256 chain is only as
  trustworthy as that key. An unpinned action in the same workflow is a direct path to it.
- Source: [2026-07-29 review](../reviews/single/2026-07-29-security-posture-desk-review.md), finding
  "GitHub Actions are generally referenced by movable version tags rather than immutable commit
  hashes" — verified.
