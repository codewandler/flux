---
id: C-259
title: "Content-authenticate core release tooling and publish verifiable artifact provenance"
pillar: Core
status: done
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/adversarial-review-remediation-2026-07-30.md
areas: [release, supply-chain]
note: "HIGH supply chain — privileged jobs pipe unsigned installers into shells; core hashes share the artifact trust root"
---

# Content-authenticate core release tooling and publish verifiable artifact provenance

## Goal

Prevent upstream installer replacement from becoming release-code execution and give consumers an
authenticity signal independent of the binaries and checksum file produced by the same job.

## Acceptance

- [x] Every downloaded bootstrap executable/script in `release.yml` is verified against a committed
      digest or cryptographic signature before execution; no `curl | sh` remains in privileged jobs.
- [x] Planning/build jobs have read-only token permissions and publication credentials exist only in
      the smallest final job that needs them.
- [x] Core release artifacts publish a consumer-verifiable signature or provenance attestation, and
      `verify-github-release.sh` verifies it rather than checking only same-origin SHA-256 files.
- [x] Verification binds provenance to the requested tag's currently resolved commit digest and
      rejects any release asset outside the exact manifest-supported, attestation-checked set.
- [x] A failing-first workflow/source-policy test rejects an unverified remote execution step and a
      release manifest without the authenticity artifact.
- [x] README installation guidance leads with a version-pinned, verified path; any one-line installer
      is labeled as a convenience with the trust tradeoff.
- [x] Release checks, action-pin checks, docs mirrors, and the standard applicable gates are green.

## Progress

- 2026-07-30 — `release.yml` no longer executes cargo-dist/rustup
  installer scripts: `scripts/install-release-tooling.sh` downloads the exact platform archive,
  compares it to the committed `release-tooling.sha256` trust root, then extracts it. Global
  workflow permissions are read-only; only `host` carries publication/OIDC authority. The host uses
  commit-pinned `actions/attest@v4`, and `verify-github-release.sh` downloads every executable asset
  and binds `gh attestation verify` to the release workflow and requested tag. A semantic
  source-policy guard and self-test reject pipe-to-shell/generated installer execution and missing
  attestations. Local Linux install, shell syntax, YAML parsing, action pins, policy self-test, tree
  policy check, changelog mirrors, and the integrated workspace gate pass.
- 2026-07-30 closure review — strengthened the consumer verifier from ref-name matching to exact
  tag-commit digest binding and made release assets a closed set. Its offline self-test now rejects
  an extra executable that the old download globs silently ignored; install docs perform the same
  digest-bound verification.

## Notes

- Evidence: all three reviews' release findings. Plugin Minisign publishing is the in-repo precedent,
  though the independently versioned plugin protocol/release remains untouched.
