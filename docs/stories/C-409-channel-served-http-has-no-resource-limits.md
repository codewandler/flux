---
id: C-409
title: "Channel-served HTTP has none of flux-server's resource limits"
pillar: Core
status: ready
priority: 7
epic: connector-channels
areas: [flux-channels]
note: "F3 of the 2026-08-01 security-posture review at 0.47.1. C-189 gave flux-server body caps, timeouts, rate limits and concurrency admission; the two channel adapters that bind their own listeners got none of it. Not an auth bypass — both refuse a non-loopback bind without authentication — but a webhook behind a proxy inherits none of its sibling's hardening"
---

# The channel listeners never got C-189's hardening

## Goal

Give the channel adapters that bind their own HTTP listeners the resource limits `flux-server`
already has.

`flux-server` received body caps, timeouts, rate limits and concurrency admission (C-189). These two
did not:

- `crates/flux-channels/src/adapters/webhook.rs:455` — `Router::new().route(&self.path, post(handle))`,
  served at `:622`;
- `crates/flux-channels/src/adapters/connector.rs:686`, served at `:1117`.

The review grepped `DefaultBodyLimit|TimeoutLayer|RequestBodyLimitLayer|rate` across
`crates/flux-channels/src/adapters/` and found **no limit layer of any kind**. The webhook handler
takes `body: Bytes`, so axum's implicit 2 MiB default is the only cap; there is no request timeout
and no rate limit. These endpoints dispatch into the live app.

⚠ **Not an auth bypass.** Both refuse a non-loopback bind without authentication
(`webhook.rs:178`), both support bearer plus HMAC signature verification, and both use the same
constant-time compare. The gap is that a deployment putting a webhook channel behind a proxy
inherits none of the hardening its `flux-server` sibling got against the same threat.

## Acceptance

- [ ] **Failing-first**: a test that an oversized or slow request against a channel listener is
      refused — failing at the merge base.
- [ ] Both adapters carry body caps, request timeouts and the admission/rate controls C-189
      established, or state per-control why the server's answer does not transfer.
- [ ] Prefer **sharing** C-189's implementation over re-deriving it; two hardening stacks for one
      threat is the drift this repo already has stories about.
- [ ] Full gate green.

## Notes

- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F3.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
