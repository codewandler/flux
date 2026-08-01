---
id: D-231
title: "The remote backend — flux-exchange terminates the call, flux exchanges channel events"
pillar: Agent
status: ready
priority: 8
design: docs/designs/sip-channel.md
epic: sip-channel
areas: [flux-channels]
note: "the hosted half. By ecosystem.md's own mechanical test the exchange owns this — it `terminates channels` and owns whatever `requires holding a credential or knowing a tenant`, and a SIP trunk is both. ⚠ Consumes C-399, whose ownership was already decided in exactly this direction"
---

# The hosted half — flux only speaks events

## Goal

A call terminated by [flux-exchange](../designs/ecosystem.md): flux holds no SIP credential, links
nothing, and exchanges channel events over a WebSocket.

## Why the exchange owns this

`ecosystem.md` separates the domains with one mechanical interrogative each — *"a boundary that
requires taste is a boundary that erodes"*:

- **flux (engine)** — *does it change what happens when an effect executes?* **Knows kinds, never
  vendors.**
- **flux-exchange** — *does it require holding a credential or knowing a tenant?* Owns principals,
  connections, credentials, **channels**, leases, stored programs, execution records.

A SIP trunk needs a registrar credential and belongs to a tenant, so it is the exchange's by that
test — and the exchange's README already says it *"terminates channels."* flux's side stays the **kind**:
a voice call channel, never a named SIP provider.

⚠ This also removes the native locality's sharpest operational problem: **no SIP credential on the
operator's machine**, and NAT traversal becomes the exchange's concern rather than a limitation
inherited from sipx having no ICE.

## Acceptance

- [ ] flux exchanges channel events with the exchange over a WebSocket and holds **no** SIP credential.
- [ ] Passes [D-225](D-225-one-sip-channel-two-localities.md)'s conformance suite unchanged — the same
      program, the same observable behaviour as native.
- [ ] ⚠ **Decide which abstraction carries this**: a flux-exchange **channel API**, or a
      [C-399](C-399-remote-guarded-io-backend.md) **guarded-IO port delegation**. They are different
      abstractions and only one should carry it; picking both is how two half-maintained paths appear.
- [ ] ⚠ **A refused operation and an unreachable exchange must not collapse into one error** — C-399's
      own acceptance, and it matters more here: an operator responds to those in opposite ways, and a
      dropped call that reads as "denied" sends someone to the wrong log.
- [ ] Fail-closed on anything the exchange does not serve — also C-399's.
- [ ] ⚠ **flux still works with no exchange configured.** Nothing here may make the service a
      requirement; that is the charter line this epic must not cross.
- [ ] The transport is guarded: the exchange endpoint routes through `guard_url_scoped` in its
      `http`/`https` form, exactly as D-205 does for `wss://`.
- [ ] Full gate green.

## Notes

- ⚠ **Not blocked on sipx.** Whether the exchange embeds sipx or runs it beside itself is the
  exchange's decision in its own trust domain — flux's guard invariant constrains what *flux* links,
  not what a separate service does. This is the half that can move while sipx's transports are unbuilt.
- The exchange is `v0.9.0` and its README carries an honest "what exists today" inventory — read it
  before planning around any of it. Channel termination is a charter claim, not necessarily shipped.
- Latency: the remote locality adds a network hop to every turn. Measure per locality; one number will
  not cover both.

## Progress

- Filed 2026-08-01 with the sip-channel epic, after the two-locality correction.
