---
id: C-352
title: "Release trust residuals — the authority half of REL-01 that no code change addressed (epic)"
pillar: Core
status: backlog
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "EPIC — bootstrap integrity and consumer attestation are genuinely closed (verified live against v0.44.0), but main has NO branch protection, NO rulesets, zero merged PRs and no release environment; ci.yml has been red on main for six pushes and blocked nothing"
---

# Release trust residuals

## Goal

Give the release pipeline an authority model that matches the integrity model it already has: bytes
are authenticated, but nothing constrains who or what can push, publish, or promote them.

## Acceptance

- [ ] C-353 protects `main` and moves every release secret behind a protected environment.
- [ ] C-354 scopes publication tokens to the steps that publish.
- [ ] C-355 binds artifact digests into the release-candidate receipt so `host` promotes verified
      bytes rather than attesting whatever arrived.
- [ ] C-356 makes attestation verification part of the documented primary install path and declares
      the first attested tag machine-readably.
- [ ] C-357 records bus factor and independent review as owned risks with a review date.
- [ ] The platform queries that produced this epic are re-run and return the intended state.

## Progress

- 2026-08-01 — opened from the validation pass. `gh api` answered every query, so ASSURE-04's
  "external-unknown" half is now verified absent rather than unknown.

## Notes

- Verified live during validation: `v0.44.0` carries 28 assets and no `.sig`/`.minisig` asset, but
  Sigstore attestations exist out-of-band for the archive, the installer and `dist-manifest.json`.
  README and getting-started document `gh attestation verify` bound to signer-workflow, tag ref and
  source digest — the strong form. Releases at or below `v0.37.x` are permanently unattested.
