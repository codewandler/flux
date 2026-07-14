---
id: C-59
title: Route A2A push delivery through scoped guarded egress
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: release blocker — push webhook DNS resolution and redirects bypass the mandatory URL guard
---

# Route A2A push delivery through scoped guarded egress

## Goal

Make every A2A push-notification POST traverse the same DNS-aware, scoped private-network guard as
all other web egress, without leaking the notification token across origins.

## Acceptance

- [x] Failing-first tests prove a hostname resolving to loopback, RFC1918, link-local, CGNAT,
      IPv4-mapped private space, or an internal hostname is rejected at delivery time.
- [x] Failing-first tests prove a public URL cannot redirect to a private destination and that the
      `X-A2A-Notification-Token` header never crosses origins.
- [x] Registration and delivery use `flux_system::net::guard_url_scoped` (or the shared guarded HTTP
      helper built on it); every redirect hop is re-resolved and re-authorized, or redirects are
      disabled with a documented refusal.
- [x] Local/private push targets work only through an explicit scoped `PrivateNetAllow` grant; the
      process-wide `FLUX_A2A_PUSH_ALLOW_LOCAL` bypass is removed or reduced to configuration that
      produces that scoped grant.
- [x] Push remains best-effort and bounded, and existing realm isolation, timeout, task projection,
      and no-retry behavior remain covered.
- [x] A-57/A2A documentation and the engineering/customer changelogs describe any user-visible
      configuration or refusal change.

## Progress

- 2026-07-14 — Routed registration and delivery through scoped DNS-aware egress checks and a
  redirect-disabled client. Delivery-time internal-address-family, DNS-rebinding, redirect/token,
  realm, timeout, and no-retry regressions cover the closed blocker.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Follow-up to [A-57](A-57-a2a-push-notifications.md): its literal-host check explicitly left DNS
  rebinding/resolution to the network layer, which violates the repository's current all-egress guard.
- Primary evidence: `crates/flux-server/src/a2a.rs` (`push_url_allowed`, `deliver_push`).
