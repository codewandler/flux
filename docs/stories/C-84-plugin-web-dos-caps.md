---
id: C-84
title: Cap plugin and web DoS vectors (QuickJS, PG-auth, CDP, HTML slice)
pillar: Core
status: done
priority: 11
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "DoS (Medium) — QuickJS no mem/stack limit, unbounded PG-auth body, unbounded CDP frame, UTF-8 slice panic"
---

# Cap plugin and web DoS vectors (QuickJS, PG-auth, CDP, HTML slice)

## Goal
Add the missing memory/size bounds where a hostile page or endpoint can amplify through a trusted
component: the QuickJS pre-tool hook runtime sets a 1s CPU interrupt but no memory/stack limit (a single
doubling alloc OOMs before the interrupt fires); the plugin PG auth path `read_exact`s a server-declared
~2GB length; the CDP read loop accumulates an unbounded frame and uses an unbounded event channel; and
`looks_like_html` slices `head[..512]` off a UTF-8 char boundary → reachable panic on attacker content.

## Acceptance
- [x] `rquickjs::Runtime` gets `set_memory_limit` + `set_max_stack_size` (hooks.rs:80-81); test
      `memory_bomb_hook_is_killed_not_ooming_the_host`.
- [x] Plugin PG auth `body_len` capped to `MAX_MESSAGE_BYTES` (pg.rs:320); test
      `read_message_refuses_an_oversized_declared_length`.
- [x] CDP per-frame size capped and the event queue bounded/back-pressured.
- [x] `looks_like_html` slices on a char boundary (`is_char_boundary`, fetch.rs:283); test
      `looks_like_html_does_not_panic_on_utf8_boundary`.

## Progress
- (not started) — filed from the 2026-07-15 full code review.
- 2026-07-15 — CDP DoS item landed (`flux-web/src/cdp.rs` + `browser.rs`). The `\0`-framed read loop
  now caps a single message at `MAX_FRAME_BYTES` (16 MiB): an over-cap frame is dropped and the stream
  resynchronises to the next terminator instead of buffering it whole, and framing scans only the
  freshly-read tail (O(total) not O(n²)) so a large frame is no longer a CPU-DoS either. The
  CDP→pump event channel is now bounded (`EVENT_CHANNEL_CAP` = 4096) via `mpsc::channel`; the reader
  `try_send`s and drops on full rather than blocking — blocking would wedge response correlation
  (the pump awaits CDP responses through the same reader). Tests: `over_cap_frame_is_dropped_and_
  stream_resyncs` and `saturated_event_channel_does_not_wedge_response_correlation` (the latter fails
  under a naive blocking-bound). All 58 `flux-web` lib tests + clippy `-D warnings` + fmt green. The
  QuickJS, PG-auth, and `looks_like_html` items were already done in-tree by an earlier pass.
  PUBLIC-API CHANGE (flag for release): `flux-web` exposes `pub mod cdp` and `pub mod browser`, so
  swapping the CDP event stream from `UnboundedReceiver<CdpEvent>` to `mpsc::Receiver<CdpEvent>` is a
  breaking signature change on two public items — `CdpClient::connect` (return type) and
  `BrowserSession::from_client` (the `events` param). Per the flux SemVer rule (breaking → MINOR on
  0.y) this belongs in the next MINOR, not a patch. `pump_loop` is private (internal only).

## Notes
- `crates/flux-plugin/src/hooks.rs:73`; `crates/flux-plugin/src/pg.rs:313`; `crates/flux-web/src/cdp.rs:117`
  + `crates/flux-web/src/browser.rs:51`; `crates/flux-web/src/fetch.rs:278`.
- Design: [harness-hardening](../designs/harness-hardening.md).
