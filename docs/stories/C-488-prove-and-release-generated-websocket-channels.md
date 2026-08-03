---
id: C-488
title: "Prove and release generated WebSocket channels"
pillar: Core
status: in-progress
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

- [ ] Hermetic cross-repository test proves generated ARI declaration → prepared plan → guarded
      native and remote socket → generic binding → Exchange fan-out and cancellation.
- [ ] Environment-gated live Asterisk smoke connects an app, observes representative lifecycle
      events and proves cancellation closes the socket without logging secrets or payloads.
- [ ] Exchange marks `subscribe` live in descriptor, console, README and public capability docs only
      after route and end-to-end tests ship.
- [ ] Releases occur in order: Flux guarded runtime, `flux-connectors` catalogue/pack, then Exchange
      with both dependency families bumped together.
- [ ] Release notes state that v0.52.0 already removed ARI and AMI from the plugin pack; no plugin
      compatibility/deprecation release or operation mapping is fabricated.
- [ ] Durable replay, acknowledgements, subscriber cursors, Slack Socket Mode generalization and
      reintroduction of removed plugin surfaces remain explicitly out of scope.

## Progress

- 2026-08-03: release work began on the v0.53 baseline. Flux's guarded/runtime slice and live-smoke
  harness are implemented; the five-crate channel host closure is being added to the CI publisher
  before the ordered connector and Exchange releases.
