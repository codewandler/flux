---
id: C-481
title: "Generated connector WebSocket channels (epic)"
pillar: Core
status: done
epic: generated-connector-websocket-channels
design: docs/designs/generated-connector-websocket-channels.md
areas: [flux-system, flux-channels]
note: "EPIC — flux-connectors declares generic RFC 6455 bindings; flux-system guards native and remote sessions; Exchange owns durable tenant channels and live at-most-once fan-out; Slack Socket Mode stays vendor-specific"
---

# Generated connector WebSocket channels

## Goal

Make a spec-generated connector channel an executable, guarded event source from declaration through
Exchange subscription, with Asterisk ARI as the first complete proof and no connector-owned runtime.

## Acceptance

- [x] C-482 makes `flux-connectors` authoritative for declarative RFC 6455 channel handshakes and
      publishes a complete generated Asterisk ARI event binding.
- [x] C-483 moves the reusable guarded WebSocket session into `flux-system` and gives native and
      selected remote execution systems equivalent bounded operations.
- [x] C-484 drives webhook and generic WebSocket bindings through one connector-channel runtime,
      preserving closed event sets, wire discriminators, payload projection and placement rules.
- [x] C-485 records the already-shipped v0.52.0 Asterisk plugin removal and proves this program does
      not resurrect either ARI or AMI while extracting the reusable generic guard.
- [x] C-486 gives Exchange durable tenant-owned channel records, placement resolution and independent
      supervisors that restore and reconnect.
- [x] C-487 ships operator management plus one authenticated multiplexed agent WebSocket with
      default-deny inbound grants, bounded fan-out and live at-most-once semantics.
- [x] C-488 supplies cross-repository conformance, live smoke, public capability truth and ordered
      release evidence without inventing a plugin compatibility release after removal.

## Progress

- 2026-08-02: after rebasing to tag `v0.52.1` (`24c2ff21`),
  `cargo search codewandler-flux-core --limit 1` reported `0.52.1`. D-249 had already removed both
  Asterisk plugin surfaces in v0.52.0; the generic connector-channel runtime remains absent.
- 2026-08-02: the program was filed from the master design after the original dirty Flux worktree was
  preserved unchanged in a separate worktree.
- 2026-08-03: the complete program shipped in order as Flux v0.54.2, flux-connectors v0.17.0 and
  flux-exchange v0.15.0. All three publication workflows and their consumer gates passed.

## Notes

- Master design: [generated-connector-websocket-channels.md](../designs/generated-connector-websocket-channels.md).
- `flux-connectors` has its own implementation epic beginning at C-489; `flux-exchange` has its own
  implementation epic beginning at X-99. These local story contracts are required by those
  repositories' operating agreements and are not duplicate architecture decisions.
