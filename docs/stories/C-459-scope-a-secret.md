---
id: C-459
title: "A secret has no scope — once resolved it can go anywhere the egress guard already allows"
pillar: Core
status: ready
priority: 5
design: docs/designs/secrets-the-agent-never-sees.md
epic: secrets-the-agent-never-sees
areas: [flux-secret, flux-app, flux-policy]
note: "⚠ applies to EVERY secret whatever the transport, so it is worth taking independently of C-458. flux guards egress per CALLER, never per SECRET. And per-principal scoping is newly expressible — C-408/C-415 established per-speaker TurnIdentity"
---

# Which destinations, and on whose behalf

## Goal

A secret carries its own scope: **which destinations** it may be sent to, and **which principal** may
cause it to be used.

## The two gaps

**1. Destination.** flux's egress guard (`guard_url_scoped`) decides whether *this caller* may reach
*this host*. It knows nothing about *which secret* is in the request. So once
`crates/flux-app/src/secrets.rs`'s `resolve_in` has substituted a plaintext value, that value can
travel to **any** host the caller is already permitted to reach.

Vaults scopes per credential — `networking.allowed_hosts`, described as preventing *"your key from ever
being shared with unauthorized hosts"* — and pairs it with `injection_location` (header, body, or both)
on the reasoning that *"request payloads are often assembled from content the agent is working with, so
the request body is the broader exposure surface."*

**2. Principal.** A vault is *"the collection of credentials associated with an end user"*, referenced
per session. flux has no equivalent — and ⚠ **it newly could**: [C-408](C-408-room-participants-share-one-identity.md)
and [C-415](C-415-a-room-triggered-journey-still-runs-as-the-operator.md) established per-speaker
`TurnIdentity`, so "which principal may use this secret" is expressible where it was not before. On a
shared surface — a room with several humans in it — that is the difference between a credential the
operator holds and a credential anyone in the room can spend.

## Acceptance

- [ ] **Failing-first**: a test asserting a destination-scoped secret is refused for an out-of-scope host
      that the caller is otherwise permitted to reach — failing at the merge base.
- [ ] Destination scope is **default-deny** where declared, and the check happens on the **resolved,
      vetted** address, matching the discipline `guard_target_host_pinned` already enforces. ⚠ A scope
      matched against the pre-resolution hostname is a bypass.
- [ ] ⚠ **A secret with no declared scope keeps working.** Breaking every existing `secret "NAME"` to add
      scoping would guarantee nobody adopts it. Unscoped must remain valid and must be *visible* as
      unscoped.
- [ ] Principal scope, built on the existing `TurnIdentity` — ⚠ **not a second identity concept.**
      C-415's lesson holds: one constructor, one trust decision.
- [ ] Injection location (header / body) decided — implement it, or record why flux's shape does not
      need it.
- [ ] Full gate green.

## Notes

- ⚠ Worth doing **independently of [C-458](C-458-substitute-at-egress.md)**: scoping applies to every
  secret whatever the transport, while substitution only fits HTTP-shaped egress. If only one of the two
  ships, this is the one with broader reach.
- Related from the other side: [D-227](D-227-outbound-a-call-is-an-effect-that-costs-money.md)'s
  destination allowlist for outbound calls is the same idea for a different resource — check whether one
  mechanism serves both before building two.

## Progress
- Filed 2026-08-02 from the Vaults comparison.
