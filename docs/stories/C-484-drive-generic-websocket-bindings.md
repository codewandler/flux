---
id: C-484
title: "Drive webhook and generic WebSocket connector bindings"
pillar: Agent
status: in-progress
epic: generated-connector-websocket-channels
design: docs/designs/generated-connector-websocket-channels.md
areas: [flux-channels, flux-app]
note: "transport-neutral binding driver; shared discrimination/projection/delivery; selected-system outbound sockets; fail-closed reconnect classification"
---

# Drive webhook and generic WebSocket connector bindings

## Goal

Turn a catalogue binding into one transport-neutral channel whose webhook and generic WebSocket
inputs share every event-routing rule and whose outbound socket runs on the selected system.

## Acceptance

- [x] Failing-first mock ARI test routes wire `ChannelCreated` to
      `<channel>.channel-created` with the complete typed payload.
- [x] `ChannelContext` carries the deliverer, cancellation token and selected `ExecutionSystem`;
      existing inbound listeners ignore the selected system and outbound socket channels use it.
- [x] The binding driver shares closed event-set enforcement, wire-value matching, discrimination,
      payload-root/path projection, delivery labels and malformed-event counters across transports.
- [x] Network failures and 5xx handshakes reconnect from one to thirty seconds with deterministic
      jitter tests and reset after stability; invalid declaration/config/auth and 400/401/403/404 are
      terminal until configuration changes.
- [x] Binary ARI frames close as protocol violations; malformed and undeclared event types are
      dropped and counted without producing vendor-controlled trigger labels.
- [ ] Placement matrix proves local/single-tenant scoped private dialing, shared endpoint-reference or
      trusted-selected-remote dialing, whole-authority refusal and pre-credential placement failure.
- [x] Full gate is green in both sandbox postures.

## Progress

- 2026-08-02: the transport-neutral driver, selected-system context, ARI mock socket, closed wire
  mapping, raw payload routing and reconnect/terminal classification are implemented and green in
  the affected suites. Cross-repository placement profiles remain release-line work.

- 2026-08-03: the driver gained a public owned zero-I/O plan seam, so an independent host can feed
  connector-pack's prepared URL/headers/routing facts into it without Flux parsing provider TOML.
  Malformed and undeclared socket events are counted/dropped before a valid event is delivered;
  binary frames are terminal; pure clock/jitter inputs pin backoff cap and stable reset.

- 2026-08-03: the complete workspace gate and both sandbox postures are green. The remaining
  placement matrix is cross-repository by design: it needs connector-pack's prepared plan and
  Exchange's deployment-owned placement resolver.

- (blocked on C-482 and C-483)

## Notes

- D-220 remains Slack Socket Mode. It may feed this binding driver, but its vendor handshake does not
  become a generic RFC 6455 declaration.
