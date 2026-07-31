---
id: C-346
title: Pin A2A push-notification delivery to the addresses its guard vetted
pillar: Core
status: backlog
epic: egress-pinning-and-confinement-residuals
design: docs/designs/egress-pinning-and-confinement-residuals.md
note: "the NET-01 shape on a path that ships X-A2A-Notification-Token — guarded but unpinned, on a shared pooled client with no no_proxy(); C-59 claims this is closed and it is not"
---

# Pin A2A push-notification delivery to the addresses its guard vetted

## Goal

Close the one remaining guard/connect DNS TOCTOU in the server, on the path that carries a
credential, and correct the story that claims it is already closed.

## Acceptance

- [ ] `send_push_guarded` (`crates/flux-server/src/a2a.rs:317-337`) establishes its connection over
      the `SocketAddr` set the guard vetted, not by handing a hostname to a shared pooled client.
- [ ] The delivery client sets `no_proxy()` — an ambient proxy is a separate unvetted peer and
      cannot inherit the destination's authorization.
- [ ] An empty vetted set fails closed before the notification token is attached.
- [ ] Failing-first regression: a `SequenceResolver` whose second answer is a link-local address,
      against a hostname with no system DNS entry, asserting the link-local listener is never
      contacted. The existing `push_delivery_reresolves_hostname_and_blocks_rebinding`
      (`a2a.rs:2173`) only counts resolver calls and passes with or without pinning — it is replaced,
      not extended.
- [ ] `docs/stories/C-59-guard-a2a-push-scoped-egress.md:38`'s closure claim is corrected to say what
      its test actually observed.

## Progress

- 2026-08-01 — filed from validation of NET-01/NET-02. Highest-ranked residual in the epic.

## Notes

- Registry client construction: `crates/flux-server/src/a2a.rs:297-300` — `redirect::none()` is set,
  `resolve_to_addrs` and `no_proxy()` are not.
- The working pattern to copy is `pinned_http_client` (`crates/flux-plugin/src/host.rs:1792-1812`).
