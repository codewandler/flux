---
id: C-311
title: "Vendor-host disclosure at approval — show what an op reaches when flux is not the one dialing"
pillar: Core
status: ready
priority: 8
epic: connector-platform
areas: [flux-plugin, flux-runtime]
note: "the compensating control for the connectors seam's one real trade-off: when a platform dials the vendor, guard_url_scoped only ever sees localhost:8000, so flux's per-vendor egress allowlist stops constraining which vendor is reached"
---

# Vendor-host disclosure at approval — show what an op reaches when flux is not the one dialing

## Goal

When an operation's real network destination is reached by something *other* than flux — a connector
platform that injects the credential and calls the vendor itself — the operator must still be told
which vendor the call reaches, at the moment they are asked to approve it.

## Why this is not optional

The connectors seam's accepted design has the deployment execute the vendor call: flux sends
`{op, args}` to `localhost:8000` and never sees a vendor credential or a vendor URL. That is the right
credential boundary, and it costs something concrete:

**`guard_url_scoped` only ever sees `localhost:8000`.** flux's per-vendor egress allowlist — the
control that says "this agent may reach `api.zendesk.com` and nothing else" — stops constraining which
vendor is reached, because from flux's side every operation has the same destination. The platform's
own manifest becomes that control instead.

An approval prompt that says "call `connectors.zendesk-ticket-create`" while the operator cannot see
that this reaches `api.zendesk.com` is an approval given without the material fact. This story is the
compensating control that makes the trade-off defensible rather than merely accepted.

## Acceptance

- [ ] **Failing-first test**: an approval request for an op whose manifest declares a vendor host
      carries that host in what the approver sees. It fails today because the declaration never
      reaches the approval path.
- [ ] The declared host is **re-verified host-side** against the manifest's `http_hosts` allowlist
      rather than trusted as free text on the individual op — a manifest that names a host outside its
      own declared allowlist is refused, and the test names that case.
- [ ] The disclosure appears on **every** approval surface that renders an op, not only the TUI —
      enumerate them and cover each, or state explicitly which are out of scope and why.
- [ ] An op that declares **no** vendor host is disclosed as such rather than silently rendering as
      if it reaches nothing. "Unknown destination" and "no destination" must not look identical.
- [ ] The disclosed value is redacted-safe: it must not become a channel for a token embedded in a
      URL by a hostile manifest.
- [ ] Full gate green in both workspaces.

## Progress
- Filed 2026-07-31 from the approved connectors-seam plan.

## Notes
- Depends on nothing in [C-310](C-310-plugin-catalog-refresh.md) but shares its file
  (`crates/flux-plugin/src/host/loading.rs`) — the two should not run in the same wave.
- Precedent for pinning what a guard admitted: **C-256/C-257** bound fleet A2A, plugin HTTP/OAuth and
  plugin TCP to the exact DNS answers `guard_url_scoped` returned, disabled ambient proxies and
  automatic redirects, and re-authorized every supported redirect hop. The platform base URL still
  goes through that path; loopback is the easy case and must not become a special case.
- ⚠ C-309 changed `plugin_tool_spec` in the same file (`AccessKind::Process` is now unconditional).
