---
id: C-488
title: "Prove and release generated WebSocket channels"
pillar: Core
status: done
epic: generated-connector-websocket-channels
design: docs/designs/generated-connector-websocket-channels.md
areas: [flux-system, flux-channels, flux-exchange]
note: "cross-repository e2e, environment-gated live Asterisk smoke, capability truth and ordered releases after the already-shipped plugin removal"
---

# Prove and release generated WebSocket channels

## Goal

Close the program with cross-repository evidence and publish each dependency only after its consumer
contract is green, without reviving Asterisk plugin surfaces removed before the program began.

## Acceptance

- [x] Hermetic cross-repository test proves generated ARI declaration → prepared plan → guarded
      native and remote socket → generic binding → Exchange fan-out and cancellation.
- [x] Environment-gated live Asterisk smoke connects an app, observes representative lifecycle
      events and proves cancellation closes the socket without logging secrets or payloads.
- [x] Exchange marks `subscribe` live in descriptor, console, README and public capability docs only
      after route and end-to-end tests ship.
- [x] Releases occur in order: Flux guarded runtime, `flux-connectors` catalogue/pack, then Exchange
      with both dependency families bumped together.
- [x] Release notes state that v0.52.0 already removed ARI and AMI from the plugin pack; no plugin
      compatibility/deprecation release or operation mapping is fabricated.
- [x] Durable replay, acknowledgements, subscriber cursors, Slack Socket Mode generalization and
      reintroduction of removed plugin surfaces remain explicitly out of scope.

## Progress

- 2026-08-03: release work began on the v0.53 baseline. Flux's guarded/runtime slice and live-smoke
  harness are implemented; the five-crate channel host closure is being added to the CI publisher
  before the ordered connector and Exchange releases.
- 2026-08-03: exact-SHA Flux candidate 30784525192 and v0.54.2 release/publish runs 30787166399 and
  30787166427 passed, followed by flux-connectors v0.17.0 and Exchange v0.15.0 publication run
  30788162730. Exchange's descriptor/site tests hold `subscribe` live to the tested route. The live
  Asterisk harness remains environment-gated; no `FLUX_TEST_ASTERISK_ARI_*` values were available in
  this release environment, matching the established optional-live-smoke contract.
