---
id: D-247
title: "Ship ARI recordings and the event WebSocket"
pillar: Agent
status: in-progress
priority: 7
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [plugins, flux-plugin]
note: "binary stored recording goes host→blob; event stream uses guarded ws ids with bounded reads"
---

# Ship ARI recordings and the event WebSocket

## Goal

Close the two ARI surfaces that ordinary JSON request/response generation cannot represent.

## Acceptance

- [x] Every recording REST operation is present, including stored-file download returning host blob
      metadata rather than inline base64.
- [x] The official `/events` operation opens a host-owned authenticated WebSocket; explicit bounded
      read and close operations make its lifecycle usable without direct plugin IO.
- [x] Tests cover text events, binary refusal/representation, ping/pong, close, deadline, size limits,
      authentication, cleanup and a representative typed ARI event model.
- [x] User-event publication preserves arbitrary declared variables without treating them as auth.

## Progress

- 2026-08-02 failing first:
  `cargo test --test ari_recordings_events
  the_official_event_websocket_is_registered_as_a_callable_operation -- --exact --nocapture`
  exited 101 because the generated registrar deliberately skipped the WebSocket source operation.
- The factual registrar remains 109 official source operations and 108 REST operations. The real
  Asterisk manifest layers the official `asterisk.ari.events.eventWebsocket` opener over it, plus
  `asterisk.ari.control.events.read` and `asterisk.ari.control.events.close`; their namespace and
  descriptions identify them as Flux lifecycle controls rather than invented Swagger operations.
- The opener passes only `asterisk.ari`, the percent-encoded relative `/events` path and
  `ari_basic` to the host. Reads require a 1–300,000 ms deadline, preserve typed JSON text events
  and unknown vendor fields, return explicit timeout/close receipts, and refuse represented binary
  frames because ARI events are JSON text. The host-owned D-241 layer retains the eight-connection,
  1 MiB frame/message and 32-message queue bounds, answers ping/pong, and closes on cancellation,
  overflow or session teardown.
- All twelve generated recording operations are registered once and delegate only endpoint-ref/auth.
  `recordings.getStoredFile` uses the bounded 256 MiB, 30-second response-to-blob path and returns
  only `blob_ref`, `size` and `sha256`. `events.userEvent` keeps arbitrary nested variables in the
  declared JSON body; even credential-shaped variable names never become authentication callbacks.
- 2026-08-02 verification atop `v0.51.0`: `cargo test --test ari_recordings_events -- --nocapture`
  passed 10 tests; `cargo clippy --test ari_recordings_events -- -D warnings` passed; focused
  production-manifest, two-way 109-source census and D-246 conditional-preflight tests each passed;
  the three D-241 host tests for authenticated/pinned frame typing, lifecycle cleanup and
  frame/queue overflow each passed; and `rustfmt --edition 2021 --check` plus `git diff --check`
  passed for the D-247 source and test files.
