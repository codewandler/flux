---
id: D-226
title: "Inbound — flux answers, and the caller is `Untrusted` because caller ID proves nothing"
pillar: Agent
status: ready
priority: 8
design: docs/designs/sip-channel.md
epic: sip-channel
areas: [flux-channels, flux-runtime]
note: "⚠ SIP `From` headers are trivially forged — caller ID is a claim, not an authentication. This is exactly what C-416 asks every adapter to declare, and C-408's `unauthenticated_participant` is the constructor to reuse. The SEMANTICS are settleable now even though the wiring (D-225) is blocked upstream"
---

# Anyone can be anyone on the phone

## Goal

flux answers an inbound call and treats the caller as `Untrusted` throughout — with the identity
decision made in one place and the spoofability stated where it will be read.

## ⚠ The property that must not be softened

**A SIP `From` header is trivially forged.** Caller ID is a claim carried by the network, not an
authentication of the person speaking. Every downstream decision must assume it can be anyone.

This is precisely the gap [C-416](C-416-a-channel-adapter-should-declare-its-principal.md) names: *the
payload's principal is authenticated by nothing, and the adapter is the only component that knows it.*
A SIP adapter knows it with unusual certainty, so it should be the clearest instance of that pattern.

## Acceptance

- [ ] **Failing-first**: a test asserting an inbound caller's turn authorizes and audits as an
      `Untrusted` principal, and that a forged `From` grants nothing — failing at the merge base.
- [ ] ⚠ **Reuses C-408's `TurnIdentity::unauthenticated_participant`** — the single constructor and the
      single trust decision, exactly as C-415 did for room-triggered journeys. **Do not add a second
      constructor**; a second trust decision is how the invariant erodes.
- [ ] Caller ID is carried as an *attribute*, never as an authentication, and nothing downstream can
      mistake it for one. It should be visibly a claim at every place it is rendered.
- [ ] An answer policy exists — which calls flux picks up — and defaults closed.
- [ ] ⚠ The spoofability is documented **where an operator configuring an inbound number will read it**,
      not only in a design doc. An operator who believes caller ID is identity will build an
      authorization on it.
- [ ] Full gate green.

## Notes

- Settleable ahead of [D-225](D-225-the-sip-sidecar-seam.md): the identity and policy semantics do not
  need the transport, and they are the part most likely to be rushed once the wiring works.
- ⚠ Registration is an open question with a credential attached: does flux `REGISTER` with a provider
  (so it has a dialable number) or answer on a static route? The former means holding a SIP credential
  — see [C-432](C-432-browser-credentials-never-come-from-the-prompt.md) on where credentials may and
  may not come from.
- C-417 (a shared conversation with one reply channel and several audiences) is adjacent: a conference
  call is exactly that shape.

## Progress

- Filed 2026-08-01 with the sip-channel epic.
