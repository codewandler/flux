---
id: C-188
title: "Dependency advisory scanning in CI — cargo-audit + cargo-deny over the 38-crate tree"
pillar: Core
status: ready
priority: 5
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW — the one confirmed finding whose truth value is UNKNOWN today: a RUSTSEC advisory in the transitive tree either exists right now or does not, and nothing in CI can tell you which"
---

# Dependency advisory scanning in CI — cargo-audit + cargo-deny over the 38-crate tree

## Goal
CI runs locked fetches, fmt, warning-free clippy, full build/test, layering checks and
backwards-compatibility tests — and no vulnerability signal whatsoever. Add advisory scanning so a
known-vulnerable transitive dependency fails the build instead of shipping silently.

## Acceptance
- [ ] `cargo-audit` (or `rustsec/audit-check`) runs in CI over the workspace lockfile and fails the
      job on any advisory not explicitly ignored.
- [ ] `cargo-deny` runs with a committed `deny.toml` covering at minimum `advisories`, `licenses`
      and `sources` — the last of these pins that every dependency comes from crates.io or a
      declared source, which is the supply-chain half of the value.
- [ ] The nested `plugins/` workspace is covered too, or its exclusion is justified in `deny.toml`.
- [ ] Any advisory ignored to get to green carries an inline comment naming the advisory ID, why it
      is not exploitable in flux's usage, and what would change that. An unexplained ignore is a
      silent regression.
- [ ] The job is demonstrated to actually fail: a temporary pin to a known-vulnerable crate version
      turns CI red, then is reverted.

## Progress
- (not started)

## Notes
- Verified absent: grepping `.github/workflows/*.yml` for `cargo-audit`, `cargo audit`,
  `cargo-deny`, `cargo deny`, `codeql`, `osv`, `fuzz`, `miri` returns **zero** hits. The single
  `provenance` hit (`release.yml:174`) is build-candidate selection for the build-once/promote-on-tag
  flow — not SLSA attestation, and not a vulnerability signal.
- **Expect the first run to fail.** That is the point of adding it, not a reason to defer. Budget
  triage time in the same change rather than landing a job that is immediately allowlisted into
  uselessness.
- Deliberately scoped to advisory + license + source. SAST (CodeQL), fuzzing and Miri were also
  found absent by the review; they are larger investments and should be argued separately rather
  than smuggled in here.
- Source: [2026-07-29 review](../../reviews/2026-07-29-security-posture-desk-review.md), finding
  "Security assurance lags behind the architecture" — verified.
