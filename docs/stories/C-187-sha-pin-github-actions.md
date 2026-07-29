---
id: C-187
title: "SHA-pin every third-party GitHub Action — the signing key sits behind movable tags"
pillar: Core
status: ready
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
- [ ] Every `uses:` referencing a third-party action in `.github/workflows/*.yml` names a full
      40-character commit SHA, with the human-readable version retained as a trailing comment
      (`uses: actions/checkout@<sha> # v4.2.2`).
- [ ] The intra-action version skew is resolved: `actions/checkout` currently appears as both `@v4`
      and `@v6`, and `actions/upload-artifact` as both `@v4` and `@v7`, across workflows. Each action
      resolves to one deliberate version repo-wide, or the difference is justified in a comment.
- [ ] A check fails CI when an unpinned `uses:` is introduced — a grep-based guard in the existing
      workflow lint is sufficient; it must fail on a deliberately unpinned line and pass on the
      pinned tree.
- [ ] `release.yml`, `release-plugins.yml` and `crates-io.yml` still complete a full dry-run cut
      after pinning — pinning must not silently change action behavior.

## Progress
- (not started)

## Notes
- Confirmed unpinned at review time: `actions/checkout@v4`, `actions/checkout@v6`,
  `actions/upload-artifact@v4`, `actions/upload-artifact@v7`, `actions/download-artifact@v4`,
  `actions/download-artifact@v8`, `dtolnay/rust-toolchain@stable`,
  `dtolnay/rust-toolchain@1.97.0`, `Swatinem/rust-cache@v2`, `lycheeverse/lychee-action@v2`.
- `dtolnay/rust-toolchain@stable` is the sharpest of these — `stable` is *designed* to move.
- Why this ranks first: `release-plugins.yml:166-181` signs the plugin index with Minisign and the
  public half is embedded in the flux binary (D-47). The per-artifact SHA-256 chain is only as
  trustworthy as that key. An unpinned action in the same workflow is a direct path to it.
- Source: [2026-07-29 review](../../reviews/2026-07-29-security-posture-desk-review.md), finding
  "GitHub Actions are generally referenced by movable version tags rather than immutable commit
  hashes" — verified.
