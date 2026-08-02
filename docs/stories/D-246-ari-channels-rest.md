---
id: D-246
title: "Ship the complete ARI channels resource"
pillar: Agent
status: done
priority: 6
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [plugins]
note: "largest live-call resource: originate, snoop, external media, variables, DTMF, hold, mute and lifecycle"
---

# Ship the complete ARI channels resource

## Goal

Expose every ARI channel operation while making live-call and external-media effects explicit.

## Acceptance

- [x] Every operation in `channels.json` is present exactly once with exact path/query/body schemas.
- [x] Conditional originate/external-media/snoop input rules run identically in dry-run and dispatch.
- [x] Failing-first tests pin live-call mutations as high or destructive and verify delete/hangup
      semantic effects.
- [x] Representative tests cover the widest query operation, a JSON body, path encoding, void and
      model responses, and each relevant error class.

## Progress

- 2026-08-02 failing first: `cargo test -p asterisk --test ari_channel_resources
  conditional_channel_rules_are_identical_in_preflight_and_dispatch -- --exact --nocapture`
  exited 101 because originate accepted `app` together with `extension` and no custom preflight
  rule existed.
- Added one shared channel preflight used by both `plugin.validate` and live dispatch. It enforces
  originate dialplan/application exclusivity, originate capability-source exclusivity, the three
  documented external-media transport/encapsulation pairs and connection/host requirements, and a
  meaningful snoop direction before any host IO.
- Before the shared-tree reset, `cargo test -p asterisk --test ari_channel_resources` passed all 10
  tests (four included generic executor tests and six channel-resource tests); focused clippy with
  `-D warnings`, the Asterisk build and direct rustfmt over the D-246 files passed. Reconstruction
  must re-run these gates.
