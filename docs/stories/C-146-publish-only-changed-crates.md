---
id: C-146
title: Publish only the crates that changed — and assert that changed crates changed version
pillar: Core
status: ready
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

- [ ] `scripts/publish-crates-io.sh` pre-checks the crates.io API for `<crate>@<version>` and skips
      without invoking `cargo publish`, so an unchanged crate no longer pays a full package to
      discover it is already live. The existing already-published branch stays as the backstop.
- [ ] A CI check asserts the inverse: a crate whose content changed since the previous release tag
      must also have a changed version. Failing-first test proves it catches a stale version.
- [ ] `codewandler-flux-host-kit` leaves the flux closure in `scripts/publish-crates-io.sh` and
      publishes with the plugin pack release instead.
- [ ] Measured: publish wall-clock for a release in which the protocol line did not move, compared
      against 0.28.0's baseline.

## Progress
- (not started)

## Notes
- Depends on C-143 — with every crate on one version line there is nothing to skip.
- The changed-crate assertion is what replaces the safety the single-version rule used to provide;
  AGENTS.md should point at it.
