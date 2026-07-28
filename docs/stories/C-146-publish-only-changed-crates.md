---
id: C-146
title: Publish only the crates that changed — and assert that changed crates changed version
pillar: Core
status: done
priority: 15
epic: plugin-protocol-decoupling
design: docs/designs/plugin-protocol-decoupling.md
note: the closure publishes all 28 crates every release (~13 min); already-published is treated as success, but each skip still pays a full cargo package to learn it
---

# Publish only the crates that changed — and assert that changed crates changed version

## Goal

A release publishes what moved. Once the protocol line stops tracking flux, most of this falls out
of the existing idempotency — this story makes it cheap and adds the guard that independent
version lines require.

## Acceptance

- [x] `scripts/publish-crates-io.sh` pre-checks the crates.io API for `<crate>@<version>` and skips
      without invoking `cargo publish`, so an unchanged crate no longer pays a full package to
      discover it is already live. The existing already-published branch stays as the backstop.
- [x] A CI check asserts the inverse: a crate whose content changed since the previous release tag
      must also have a changed version. Failing-first test proves it catches a stale version.
- [x] `codewandler-flux-host-kit` leaves the flux closure in `scripts/publish-crates-io.sh` and
      publishes with the plugin pack release instead.
- [x] Measured: publish wall-clock for a release in which the protocol line did not move, compared
      against 0.28.0's baseline.

## Progress
- Done. See the CHANGELOG `[Unreleased]` entries and `docs/designs/plugin-protocol-decoupling.md` ("As built").
- **Measured at v0.30.0**, the first release after the protocol line settled: **6 crates skipped
  without packaging at all** — exactly `flux-plugin-protocol`, `flux-spec`, `flux-policy`,
  `flux-secret`, `flux-evidence`, `flux-datasource`, all still at 1.0.0. Publish wall-clock
  **13m13s (v0.29.0) -> 11m33s (v0.30.0)**, ~13% off. Not a controlled comparison — v0.29.0 also
  published one brand-new crate name — but the 6 skips are the direct effect, and they grow into
  the dominant saving on any release where the wire does not move (i.e. most of them).

## Notes
- Depends on C-143 — with every crate on one version line there is nothing to skip.
- The changed-crate assertion is what replaces the safety the single-version rule used to provide;
  AGENTS.md should point at it.
