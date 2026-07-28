---
id: C-145
title: Run the previously released plugin binary against the current host in CI
pillar: Core
status: done
priority: 14
epic: plugin-protocol-decoupling
design: docs/designs/plugin-protocol-decoupling.md
note: the claim "a plugin built against protocol 1.0 still works against a much later flux" is the whole point of the decoupling and nothing tests it — every current test builds host and guest from the same tree
---

# Run the previously released plugin binary against the current host in CI

## Goal

Prove the decoupling holds across time, not just across crates: an actual released binary, built
against an older protocol crate, still speaks to today's host.

## Acceptance

- [x] A CI job downloads a plugin binary from the most recent `plugins-v*` GitHub release, runs it
      against the host built from the current tree, and asserts its manifest loads and one
      read-shaped operation round-trips.
- [x] The job fails loudly on an incompatibility rather than skipping — a skip on download failure
      is allowed only when the release is genuinely absent, and it says so in the log.
- [x] The asserted operation needs no third-party credential (pick one whose failure mode is local,
      or drive it through the existing plugin test fixtures).
- [x] Documented in the design doc as the test that backs the compatibility claim.

## Progress
- Done. See the CHANGELOG `[Unreleased]` entries and `docs/designs/plugin-protocol-decoupling.md` ("As built").

## Notes
- Complements `scripts/smoke-plugins.sh`, which exercises the *installed* pack against live
  services; this one is hermetic and time-directional.
- Natural follow-on (out of scope): record a `min_protocol` in the pack index so `flux plugin
  install` can refuse an incompatible pack before running it.
