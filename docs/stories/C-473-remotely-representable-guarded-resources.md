---
id: C-473
title: "Remotely representable guarded resources — managed processes and byte streams without native handles"
pillar: Core
status: done
priority: 5
epic: remote-agents
design: docs/designs/remote-agents.md
areas: [flux-system]
note: "C-399 cannot delegate background children because ManagedChild owns tokio::process::Child; C-435 will have the same problem for sockets unless the port returns opaque guarded handles"
---

# Remotely representable guarded resources

## Goal

Make every long-lived result of guarded IO representable by either the native system or a remote
delegate, without putting transport or protocol types in `flux-system`.

## Acceptance

- [x] A failing-first test proves a non-native `GuardedProcess` can start, observe, wait for and stop
      a long-lived process without constructing a `tokio::process::Child`.
- [x] Object-safe managed-process and duplex-stream handles expose only guarded lifecycle and byte
      operations; native implementations wrap the current process/socket resources.
- [x] Dropping or cancelling a handle has an explicit disposition; it cannot silently orphan a
      process or leave an inbound listener accepting indefinitely.
- [x] Optional lifecycle and stream operations deny by default.
- [x] Existing native process, plugin-host and sandbox behavior remains unchanged; direct-IO and
      `flux-codegate` checks remain green.

## Progress

- Filed from the remote-effects implementation plan after C-399 explicitly documented that its
  concrete `ManagedChild` result cannot cross a wire.
- 2026-08-02: `ManagedChild`, network streams, listeners and datagram endpoints now wrap object-safe
  substrate handles. Native and HTTPS/WSS implementations share lifecycle semantics; drop closes or
  kills, optional methods refuse, frames and pending accepted-stream handles are bounded.

## Notes

- Coordinate with C-435 so guarded network traits return these opaque stream/listener handles rather
  than concrete Tokio sockets.
- This is substrate machinery, not the HTTPS protocol; C-475 owns serialization and transport.
