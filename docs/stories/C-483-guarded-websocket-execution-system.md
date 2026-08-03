---
id: C-483
title: "Open bounded WebSockets through the selected execution system"
pillar: Core
status: done
epic: generated-connector-websocket-channels
design: docs/designs/generated-connector-websocket-channels.md
areas: [flux-system]
note: "extract D-241's reusable guard into flux-system; opaque native and remote sessions, one pinned resolution, TLS/SNI, cancellation and strict bounds"
---

# Open bounded WebSockets through the selected execution system

## Goal

Give every selected `ExecutionSystem` one fail-closed WebSocket primitive so higher layers can drive
declared channels without holding a transport or silently falling back to the local machine.

## Acceptance

- [x] Failing-first system tests prove DNS rebinding, private-address scope, undeclared endpoint,
      missing auth and caller-supplied whole authority are refused before credentials reach a dial.
- [x] `GuardedNetwork::open_websocket_scoped` returns an opaque `GuardedWebSocketSession` with bounded
      read and close, native DNS pinning, TLS/SNI, auth-safe handshake, ping/pong and cancellation.
- [x] Defaults are 1 MiB per frame/message, 32 queued messages and at most five seconds for graceful
      close; binary/text/close/protocol failures remain typed.
- [x] Remote-system protocol and server/client implementations provide equivalent open/read/close
      semantics and tests prove a selected remote system never executes locally.
- [x] Existing D-241 endpoint-reference consumers can delegate to this primitive without weakening
      their capability or session-scope checks; no removed Asterisk plugin code is restored.
- [x] Root gate and both sandbox postures are green.

## Progress

- 2026-08-02: rebased to v0.52.1 and added the native/remote port, opaque bounded session, pinned
  native dial, TLS/SNI, cancellation, ping/pong and remote delegate seam. The affected tests and
  clippy are green; the full root/sandbox release gate remains for C-488.

- 2026-08-03: rebased through v0.53.0. Tests now pin the public defaults, exact private-host grant,
  one DNS resolution, TLS hostname/SNI, guarded handshake-owned authority, queue/message bounds and
  the absence of a local fallback when a selected remote does not serve sockets. D-249 left no
  endpoint-reference plugin consumer in the tree to migrate; the generic port is the surviving seam
  and no Asterisk source returned.

- 2026-08-03: the complete workspace build/test/clippy/format/codegate gate passed, followed by the
  missing-bubblewrap workspace posture and the real confined sandbox backend suite.

## Notes

- Consume C-478's typed execution placement rather than adding another placement enum.
