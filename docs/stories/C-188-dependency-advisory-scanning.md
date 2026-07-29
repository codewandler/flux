---
id: C-188
title: "Dependency advisory scanning in CI — cargo-audit + cargo-deny over the 38-crate tree"
pillar: Core
status: in-progress
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
- [x] `cargo-audit` (or `rustsec/audit-check`) runs in CI over the workspace lockfile and fails the
      job on any advisory not explicitly ignored.
      → `.github/workflows/security-audit.yml` `cargo-audit` job: `cargo audit --deny warnings`
        (root Cargo.lock + `--file plugins/Cargo.lock`), verified locally exit 1 on a real
        vulnerability, exit 0 only with the four justified `--ignore`s.
- [x] `cargo-deny` runs with a committed `deny.toml` covering at minimum `advisories`, `licenses`
      and `sources` — the last of these pins that every dependency comes from crates.io or a
      declared source, which is the supply-chain half of the value.
      → `deny.toml` + the `cargo-deny` job; `[sources]` sets `unknown-registry`/`unknown-git = deny`
        and allow-lists only crates.io plus the one first-party git source.
- [x] The nested `plugins/` workspace is covered too, or its exclusion is justified in `deny.toml`.
      → second `cargo deny --manifest-path plugins/Cargo.toml --config deny.toml check` step, plus a
        second `cargo audit --file plugins/Cargo.lock`. `deny.toml` header documents the split.
- [x] Any advisory ignored to get to green carries an inline comment naming the advisory ID, why it
      is not exploitable in flux's usage, and what would change that. An unexplained ignore is a
      silent regression.
      → `deny.toml [advisories].ignore`: four triaged entries (RUSTSEC-2024-0436 paste,
        -2026-0192 ttf-parser, -2026-0206 rustybuzz, -2026-0002 lru), each with ID + reachability +
        clearing condition. All four are informational (unmaintained/unsound), zero vulnerabilities.
- [x] The job is demonstrated to actually fail: a temporary pin to a known-vulnerable crate version
      turns CI red, then is reverted.
      → shown against a synthetic scratch lockfile (Cargo.lock is out of this story's fence): pinning
        `smallvec 1.6.0` + `time 0.1.45` made `cargo audit --deny warnings` report 2 vulnerabilities
        (RUSTSEC-2021-0003 critical, RUSTSEC-2020-0071) and exit 1. Nothing in the tree was pinned,
        so nothing needed reverting.

## Progress
- Landed `deny.toml` (advisories + licenses + sources) and `.github/workflows/security-audit.yml`
  (standalone workflow — kept file-disjoint from `ci.yml`, which a sibling story edits this wave).
  Two tools by design: cargo-deny (config-driven gate) + cargo-audit (independent RustSec scanner
  with `--deny warnings`, which also catches unsound/notice classes).
- Triaged the tree's real advisory signal: **zero vulnerabilities**, four informational advisories
  (3 unmaintained: paste, ttf-parser, rustybuzz; 1 unsound: lru), each ignored with justification.
- `unsound = "all"` in `deny.toml` overrides cargo-deny's default `"workspace"`, which would have
  silently passed the transitive `lru` unsound advisory. `yanked = "deny"` hardens past the "warn"
  default.
- Follow-up (out of fence here — needs a Cargo.lock bump): update `lru` to `>= 0.16.3` to clear
  RUSTSEC-2026-0002 at the source and drop its ignore entry.
- Gate run locally: `cargo deny check` green on both workspaces; `cargo audit --deny warnings` green
  on both lockfiles with the four ignores; workflow YAML parses; `scripts/check-action-pins.sh`
  passes over the new file. Rust build/test/clippy gate intentionally not run — no Rust changed.

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
