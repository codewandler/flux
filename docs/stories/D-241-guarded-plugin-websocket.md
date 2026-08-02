---
id: D-241
title: "Add a guarded endpoint-reference WebSocket capability for plugins"
pillar: Core
status: done
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [flux-plugin, flux-system]
note: "deny by default; host resolves endpoint/auth and owns bounded connect/read/close lifecycle"
---

# Add a guarded endpoint-reference WebSocket capability for plugins

## Goal

Let a plugin consume a declared WebSocket without learning the endpoint URL or credential and
without opening its own socket.

## Acceptance

- [x] Failing-first host tests prove an undeclared WebSocket, undeclared endpoint, disallowed host,
      ungranted private target and unknown auth purpose are each refused before a connection.
- [x] `ws.connect/read/close` resolves endpoint references and Basic/Bearer/header/query auth through
      the same host-owned path as HTTP; connection ids are scoped to one plugin session.
- [x] Frame size, queued messages, read deadline and connection count are bounded; binary and text
      frames are distinguished; ping/pong and close are handled without exposing raw transport state.
- [x] Every live connection closes on explicit close, plugin exit and host cancellation.
- [x] The protocol wire golden, guest SDK, manifest capability validation and no-bypass tests cover
      the additive capability; protocol-line and plugin-pack release obligations are recorded.

## Progress

- 2026-08-02 failing first:
  `cargo test -p codewandler-flux-plugin-protocol --test wire_contract websocket_capability_round_trips_as_an_explicit_deny_by_default_wire_grant -- --exact`
  exited 101 because the unknown input field was discarded and absent on serialization.
- Added the explicit default-false `PluginCapabilities::websocket` wire grant, regenerated the wire
  golden with the required deliberately-red `FLUX_UPDATE_GOLDEN=1` run, reviewed its one-field diff,
  and re-ran both the focused round-trip and golden checks green.
- 2026-08-02 failing first: `cargo test -p codewandler-flux-plugin
  websocket_connect_is_an_explicit_deny_by_default_capability -- --nocapture` exited 101 because
  `ws.connect` was still an unknown callback rather than an explicit manifest-gated capability.
- 2026-08-02: implemented endpoint-reference-only `ws.connect/read/close`. The host resolves named
  or discovered endpoints and Basic/Bearer/header/query auth, checks the declared host and scoped
  private-network grant, then gives the exact vetted DNS answer to `flux-system`'s pinned TCP dial
  before handshaking over that connected stream. Limits are eight connections, 1 MiB per frame and
  message, 32 queued application messages, and 1–300,000 ms connect/read/close deadlines.
- 2026-08-02: live tests cover the refusal matrix, one-resolution pinning, Basic injection,
  text/binary typing, ping/pong, peer close, timeout, queue/frame overflow, connection-id session
  scope, explicit close, callback cancellation and session drop. The guest SDK/mock, network
  authority projection, refresh widening and subprocess no-bypass path are covered too.
- 2026-08-02 release obligation: this additive wire field requires the next compatible minor
  releases of `codewandler-flux-plugin-protocol` and host-kit before publication, followed by a
  separately dispatched plugin-pack release after the Asterisk work lands. The pack workflow owns
  the `plugins-v*` tag; do not hand-push it.
- 2026-08-02 verification: `cargo test -p codewandler-flux-plugin` passed 146 tests with one ignored;
  `cargo test -p codewandler-flux-plugin-protocol --test wire_contract` passed seven;
  `cargo test -p codewandler-flux-host-kit` passed 37 unit tests plus four boundary tests; and
  `cargo test -p flux-codegate` passed 44. Package-scoped clippy with `-D warnings` and root/nested
  formatting checks passed.
- 2026-08-02 adversarial follow-up, failing first: the peer-termination regression timed out because
  completed managers left their receivers in the session map, and the opaque-ref regression received
  `ws.connect: tcp dial opaque-internal.test:9: ...`. Terminal handles now retain the connection
  permit, are marked atomically, and are reaped before reconnect, which bounds the whole session to
  eight handles and releases their at-most-32 queued frames without requiring `read` or `close`.
  Cancellation and drop synchronously clear the registry; each handle retains an abort control, and
  close, overflow and pong flushes have a one-second manager-side ceiling in addition to caller
  deadlines.
- The follow-up live-wire proof covers Bearer, custom-header and query auth; a local CA/server proves
  `wss` uses the endpoint hostname as SNI while consuming exactly one pinned DNS answer. Discovered
  endpoint failures are now location-free while named endpoint failures retain their actionable
  diagnostics. `cargo test -p codewandler-flux-plugin` passed 150 tests with one ignored, and focused
  clippy with `-D warnings`, formatting and `git diff --check` passed.
- 2026-08-02 workspace-union follow-up: the WSS fixture no longer relies on rustls auto-selecting a
  crypto provider, which panics when workspace feature unification enables both `aws-lc-rs` and
  `ring`. A fixture-local `OnceLock` installs the repository's existing `aws-lc-rs` choice exactly
  once when no provider is already installed. The exact SNI test passed under `cargo test
  --workspace host::tests::wss_uses_the_endpoint_hostname_for_sni_over_the_single_pinned_dial --
  --exact --nocapture`; the plugin package then passed 150 tests with one ignored, plus focused
  clippy with `-D warnings`, formatting and `git diff --check`.
