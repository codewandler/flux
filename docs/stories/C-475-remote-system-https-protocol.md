---
id: C-475
title: "A versioned remote-system protocol and authenticated HTTPS daemon"
pillar: Core
status: done
epic: remote-agents
design: docs/designs/remote-agents.md
areas: [flux-system, flux-server, flux-cli]
note: "C-399 deliberately chose no wire format; C-436 assumes an address can be connected to, so this is the missing product bridge"
---

# A versioned remote-system protocol and HTTPS daemon

## Goal

Serve the complete guarded execution-system contract over authenticated HTTPS/WSS so a local runtime
can place effects on one explicitly configured remote workspace.

## Acceptance

- [x] A versioned handshake reports protocol version, substrate identity, canonical workspace root,
      sandbox posture and the exact supported operation set.
- [x] `flux system serve --bind <addr> --workspace <path>` serves one single-tenant workspace; a
      non-loopback bind refuses to start without TLS certificate/key and bearer authentication.
- [x] The client endpoint passes through the existing scoped URL guard and normal certificate
      validation; bearer values are credential references and never URL/query literals.
- [x] HTTPS carries bounded request/response operations and authenticated WSS carries process,
      listener and socket byte streams. No unbounded frame or output path exists.
- [x] The wire covers every execution-system family. A missing capability is an `Unserved` answer,
      never local fallback.
- [x] Protocol/frame errors become `Unreachable`; far-side guard refusals remain `Refused`.
- [x] An offline loopback server test drives the shared execution-system contract over real bytes.

## Progress

- Filed from the remote-effects plan. C-399 proves delegation can cross bytes in a test-owned
  protocol but deliberately provides no production codec, daemon or client.
- 2026-08-02: protocol v1 ships behind `flux system serve` and agent `--remote`. Every route is
  bearer-authenticated TLS; the guarded URL resolver pins client addresses; bounded HTTPS carries
  finite calls and bounded WSS carries managed process/TCP/UDP lifecycles. A real self-signed TLS
  test exercises every family and preserves a far-side refusal as `Refused`.

## Notes

- Depends on C-474. C-476 owns retry/reconnect semantics; C-439 owns user-facing trust and evidence
  provenance.
