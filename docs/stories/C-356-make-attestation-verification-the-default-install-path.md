---
id: C-356
title: Make attestation verification part of the primary install path
pillar: Core
status: backlog
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "the installer IS attested, but the documented primary path is download-then-sh with no verify step; the checksums it embeds come from the same workflow and are not an independent root"
---

# Make attestation verification part of the primary install path

## Goal

Put the one independent trust root the project has — the Sigstore attestation — on the path users
actually follow, and let a downstream verifier fail closed on unattested releases.

## Acceptance

- [ ] `README.md` and `website/docs/getting-started.md` place `gh attestation verify flux-installer.sh`
      (signer-workflow, source-ref, source-digest, `--deny-self-hosted-runners`) in the primary
      install sequence, not only in a separate verification section.
- [ ] A machine-readable statement of the first attested tag (`v0.38.0`) ships with the release
      metadata so a verifier can distinguish "unattested by policy" from "attestation missing".
- [ ] The docs state plainly that `.sha256`, `sha256.sum` and the installer's embedded checksums are
      produced by the same workflow as the artifacts and defend against transport corruption only.
- [ ] Build-time toolchain and package acquisition (`rustup toolchain install`,
      `dtolnay/rust-toolchain`, `apt-get install minisign`) is documented as relying on ecosystem
      trust roots rather than repo-pinned digests — the honest remainder of REL-01(b).

## Progress

- 2026-08-01 — filed from validation of REL-02. Attestations verified live against `v0.44.0`.
- 2026-08-04 — remains `backlog`, with every acceptance box open. This is the attestation-first
  consumer-experience residual: C-516 will require the v0.56.0 assets and live attestations to verify,
  but it does not make verification part of a user's primary install command. Useful and still owed,
  not a v0.56.0 publication blocker.

## Notes

- The website contract test will need the mirrored page regenerated in the same commit.
- Do not use publication of v0.56.0 as evidence that this consumer-path contract is complete.
