---
id: C-80
title: Give the default provider HTTP client connect/read timeouts
pillar: Core
status: done
priority: 7
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "Availability (High) — a stalled connection hangs the turn forever and wedges every future turn"
---

# Give the default provider HTTP client connect/read timeouts

## Goal
Prevent one stalled provider connection from wedging the whole client. `NativeProvider::new` uses
`reqwest::Client::new()` with no `connect_timeout` and no read timeout, and every default constructor
goes through it. A proxy that completes the TCP handshake then stalls before headers hangs
`rb.send().await` indefinitely; the retry loop only fires on `send()` errors / retryable statuses, so a
silent stall is never recovered. Because `Session::send`/`stream` hold the client-wide `turn_guard`
mutex for the whole call, that one hang then blocks every subsequent turn on the client. (The WS path
was hardened for this in C-28; the HTTP path — including the WS fallback — was not.)

## Acceptance
- [ ] Failing-first test (blackhole/stall server, mirroring the codex `first_frame_timeout` test): a
      stalled connect/response fails the turn within a bounded time instead of hanging.
- [ ] Default client built with `connect_timeout` + an idle/read timeout. Do **not** set a total
      `.timeout()` — it would truncate long legitimate streams.

## Progress
- (not started) — filed from the 2026-07-15 full code review.

## Notes
- `crates/flux-provider/src/lib.rs:343` (`NativeProvider::new`), `:729` (`send`); guard at
  `crates/flux-sdk/src/session.rs:57`.
- Design: [harness-hardening](../designs/harness-hardening.md).
