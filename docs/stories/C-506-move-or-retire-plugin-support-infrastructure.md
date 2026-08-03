---
id: C-506
title: "Move or retire the official plugin support infrastructure"
pillar: Core
status: backlog
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "decide host-kit and pack-index after adapter migration: protocol SDK may move with connector runtime artifacts; the official signed plugin index must not remain a rival catalogue"
---

# Move or retire the official plugin support infrastructure

## Goal

Give `plugins/host-kit` and `plugins/pack-index` an explicit post-migration home so the generic stdio
runtime can survive where useful without preserving a second official integration distribution path.

## Acceptance

- [ ] `host-kit` is either moved/published as the connector runtime SDK or replaced by a versioned
      protocol client with identical capability and credential-boundary guarantees.
- [ ] `pack-index` ceases to list official vendor integrations once connector artifact distribution
      is live; compatibility entries have a bounded removal policy.
- [ ] `flux plugin install` either becomes a connector-runtime compatibility command with explicit
      deprecation or remains only for third-party extensions; docs cannot present it as the official
      integration path.
- [ ] Supply-chain verification is at least as strong as the current minisign/archive-digest path and
      shares artifact truth with Flux Exchange.
- [ ] A test distinguishes generic support crates from vendor adapters so C-505's zero count remains
      meaningful.

## Progress

- (not started)
