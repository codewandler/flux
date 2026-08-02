---
id: D-239
title: "Asterisk ARI — complete official Swagger surface behind guarded host IO (epic)"
pillar: Agent
status: in-progress
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [plugins, flux-plugin]
note: "EPIC — preserve 8 AMI ops; account for all 109 official ARI operations, including events WebSocket and binary recordings"
---

# Asterisk ARI — complete official Swagger surface behind guarded host IO

## Goal

Make the existing Asterisk plugin a complete ARI client generated from Asterisk's pinned official
Swagger documents, without moving private-network, authentication, WebSocket or binary IO out of
Flux's guarded host.

## Acceptance

- [ ] D-240 vendors and proves the exact official LTS Swagger set.
- [ ] D-241 adds the guarded WebSocket capability required by `/events`.
- [ ] D-242 adds host-owned HTTP-to-blob delivery for stored recordings.
- [ ] D-243 compiles the spec into exact manifest/input/output contracts and a generic REST executor.
- [ ] D-244 through D-247 close every resource family, including the WebSocket event operation.
- [ ] D-248 proves 109/109 coverage, preserves all eight AMI contracts, updates docs/smoke and cuts the plugin-pack release.

## Progress

- 2026-08-02: official tag `22.10.1` (tag object `4f85d058…`, peeled commit `f0e408a7…`)
  measured at 11 resource documents, 76 paths, 109 operations, 85 models and 275 parameters;
  108 operations are REST and one upgrades to WebSocket.
- 2026-08-02: confirmed this belongs in `../flux/plugins`, not `flux-connectors`.

## Notes

- Design: [Asterisk ARI](../designs/asterisk-ari.md).
- Existing AMI implementation: `plugins/asterisk/src/main.rs`.
