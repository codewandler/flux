---
id: C-413
title: "Two residuals in the 0.47.x diff: the XMPP socket is guarded but not DNS-pinned, and `JaasRoom::join` has a TOCTOU"
pillar: Core
status: in-progress
priority: 9
epic: meeting-rooms
areas: [flux-channels]
note: "F10 of the 2026-08-01 security-posture review at 0.47.1. Both are low severity and both sit in a file that is otherwise meticulous about exactly these two classes — which is why they are worth closing rather than tolerating"
---

# What the 0.47.x room work left behind

## Goal

Close the two residuals the release-diff pass found, so the JaaS path is pinned and raced-free
throughout rather than nearly so.

**1. The XMPP WebSocket is guarded but not DNS-pinned, and it now carries the guest token.**
`guarded_endpoint` (`crates/flux-channels/src/rooms/xmpp/session.rs:167`) vets via
`guard_url_scoped`, then hands the *hostname* URL to `connect_async`, which **re-resolves at connect
time**. The three JaaS HTTP hops in the same handshake close exactly this gap with
`guard_url_scoped_pinned` + `resolve_to_addrs`. Pre-existing from D-205, and `wss://` certificate
validation bounds it — but it is now the only unpinned hop in a deliberately pinned chain, and the
token rides its query string.

**2. `JaasRoom::join` has a TOCTOU on its own "already joined" guard.**
`crates/flux-channels/src/rooms/jaas/mod.rs:368` checks `inner.is_some()`, releases the lock, awaits
`mint_and_join`, then stores. Two concurrent joins both pass; the loser's session and its
`SessionPump` leak. Low severity — the driver joins once — but this file is otherwise meticulous
about precisely this race: the `leave`/`rejoin` cancel-then-take pairing is correct and tested, which
is what makes the asymmetry worth removing.

## Acceptance

- [x] **Failing-first for the TOCTOU**: a test driving two concurrent `join` calls and asserting one
      session, no leak — failing at the merge base. The `leave`-race test added for the same file is
      the shape to follow, including its "verify the guard fires by deleting it" step.
- [x] `join` closes the window the same way `leave`/`rejoin` already do — check and install under one
      guard, no bare `await` between them.
- [x] The XMPP socket is DNS-pinned like its HTTP siblings, or the asymmetry is documented at
      `session.rs:167` with the reason it cannot be.
- [ ] Full gate green.

## Notes

- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F10.
- `guard_url_scoped_pinned` + `resolve_to_addrs` is the pattern already used by the JaaS token hops
  in the same handshake — reuse it rather than deriving a second one.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
- 2026-08-04: failing-first concurrent-join proof observed two constructed sessions (`left: 2`,
  `right: 1`) before the initial-join gate was installed; the same focused test now passes and a
  fixed-resolver WebSocket handshake proves the RFC 6455/TLS request dials the exact address vetted
  by `flux-system` without handing the hostname to a second resolver. The wave full gate is pending.
