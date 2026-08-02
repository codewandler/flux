---
id: D-239
title: "Asterisk ARI — complete official Swagger surface behind guarded host IO (epic)"
pillar: Agent
status: done
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [plugins, flux-plugin]
note: "EPIC — preserve 8 AMI ops; account for all 109 official ARI operations, including events WebSocket and binary recordings"
---

# Asterisk ARI — complete official Swagger surface behind guarded host IO

> **Superseded by [D-249](D-249-remove-asterisk-plugin.md).** This story remains done as historical
> evidence of what v0.51.1 shipped; its ownership decision is no longer current.

## Goal

Make the existing Asterisk plugin a complete ARI client generated from Asterisk's pinned official
Swagger documents, without moving private-network, authentication, WebSocket or binary IO out of
Flux's guarded host.

## Acceptance

- [x] D-240 vendors and proves the exact official LTS Swagger set.
- [x] D-241 adds the guarded WebSocket capability required by `/events`.
- [x] D-242 adds host-owned HTTP-to-blob delivery for stored recordings.
- [x] D-243 compiles the spec into exact manifest/input/output contracts and a generic REST executor.
- [x] D-244 through D-247 close every resource family, including the WebSocket event operation.
- [x] D-248 proves 109/109 coverage, preserves all eight AMI contracts, updates docs/smoke and cuts the plugin-pack release.

## Progress

- 2026-08-02: official tag `22.10.1` (tag object `4f85d058…`, peeled commit `f0e408a7…`)
  measured at 11 resource documents, 76 paths, 109 operations, 85 models and 275 parameters;
  108 operations are REST and one upgrades to WebSocket.
- 2026-08-02: confirmed this belongs in `../flux/plugins`, not `flux-connectors`.
- 2026-08-02: D-240 through D-248 are done. Core `v0.51.1` and the workflow-created
  `plugins-v0.1.6` tag both target exact commit
  `7270b2f75fda9bd3f1e9b21bbad7531886e6c5f3`; the signed pack carries 95 indexed plugin
  archives plus its index/signature, including all five Asterisk target archives.
- 2026-08-02: an isolated signed-pack install reported Asterisk `v0.1.6` as `[ok] [verified]` with
  120 total AMI/ARI/control operations and generated its skill/reference. Live PBX verification
  remained correctly env-gated because `ASTERISK_ARI_USERNAME`, `ASTERISK_ARI_PASSWORD` and
  `ASTERISK_ARI_URL` were absent.

## Notes

- Design: [Asterisk ARI](../designs/asterisk-ari.md).
- Existing AMI implementation: `plugins/asterisk/src/main.rs`.
