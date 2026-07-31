---
id: C-355
title: Bind artifact digests into the release-candidate receipt
pillar: Core
status: backlog
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "the receipt binds version + commit SHA + run id only; host downloads artifacts-* by run id and uploads them verbatim, then attests whatever arrived — provenance of the workflow, not integrity of the build output"
---

# Bind artifact digests into the release-candidate receipt

## Goal

Authenticate the build-to-publish handoff, which is currently the one unverified transition in an
otherwise content-authenticated pipeline.

## Acceptance

- [ ] `scripts/release-candidate.sh` records a digest manifest of every `artifacts-*` upload at
      `record-release-candidate` time.
- [ ] The `host` job re-verifies each downloaded artifact against that manifest before `dist host`
      and before attestation; a mismatch fails the release.
- [ ] Failing-first proof: a fixture where one artifact's bytes differ from its recorded digest
      makes the promotion step fail.
- [ ] The BUILD-ONCE promotion documentation states that the receipt now binds content, not only
      identity.

## Progress

- 2026-08-01 — filed from validation of REL-01 subclaim (d).

## Notes

- Build jobs hold no write token, which limits push-side compromise; this closes the remaining
  handoff, not a demonstrated exploitation path.
