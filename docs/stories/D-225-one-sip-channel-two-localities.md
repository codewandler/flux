---
id: D-225
title: "One SIP channel, two localities — the same program runs against a local process or a hosted exchange"
pillar: Agent
status: ready
priority: 8
design: docs/designs/sip-channel.md
epic: sip-channel
areas: [flux-channels]
note: "the seam, and it is NOT blocked. ⚠ Neither locality may become mandatory — ecosystem.md: `flux must never require flux-exchange`; C-399: `flux must be able to do this locally as dev without depending on a service — that is the local-first principle, not a convenience`. Rooms already prove the pattern: one `room` channel, three backends"
---

# The same call, wherever it is terminated

## Goal

A SIP channel whose vocabulary is locality-independent: the same `.flux` program runs whether the call
is terminated by a local sipx process or by a hosted [flux-exchange](../designs/ecosystem.md), and does
not know which.

## The shape

| | **native** ([D-230](D-230-the-native-sip-backend.md)) | **remote** ([D-231](D-231-the-remote-sip-backend.md)) |
|---|---|---|
| terminates the call | a local sipx process flux drives | flux-exchange |
| holds the SIP credential | the operator's machine | the exchange, per tenant |
| flux sees | channel events over a local control wire | channel events over a WebSocket |
| flux links | nothing | nothing |

This is the `kubectl` shape: one vocabulary, whether the thing serving it is across a socket on your
laptop or across the network behind a cert.

## ⚠ Neither locality may become mandatory

This is doctrine, and both directions have already been decided:

- `ecosystem.md`: *"**flux must never require flux-exchange.** A `.flux` program loading a connector
  module on a laptop is a complete path. Trading plugin-binary distribution pain for service lock-in
  would be a bad trade made twice."*
- [C-399](C-399-remote-guarded-io-backend.md): *"flux owns it, flux-exchange reuses it. **flux must be
  able to do this locally as dev without depending on a service — that is the local-first principle,
  not a convenience.**"*

And by the ecosystem's own mechanical test — *flux knows kinds, never vendors*; *the exchange owns
whatever requires holding a credential or knowing a tenant* — flux's side of this is the **kind** ("a
voice call channel"), never the SIP provider.

## Acceptance

- [ ] **Failing-first**: a test running one program against two backends and asserting identical
      observable behaviour — failing at the merge base.
- [ ] A locality-independent channel vocabulary: call arrives, call answered, speech in, speech out,
      DTMF, call ended. ⚠ Nothing in it names SIP, sipx, or the exchange — a vocabulary that leaks its
      backend is a vocabulary with one backend.
- [ ] ⚠ **Parity is testable, not aspirational.** Two backends drift, and the one that drifts silently
      is the one nobody demos. A shared conformance suite both must pass is the mechanism; a doc
      promising equivalence is not.
- [ ] Backend selection is config, mirroring how the `room` channel already carries `mock`, `xmpp` and
      `jaas` — extend that pattern rather than inventing a second way to pick a backend.
- [ ] Neither locality is required: no service dependency for local use, no local sipx for hosted use.
      Pin both directions.
- [ ] Full gate green.

## Notes

- **Not blocked.** [D-230](D-230-the-native-sip-backend.md) is blocked on sipx's unbuilt transports;
  the seam, the semantics (D-226/D-227/D-229) and the remote backend are not.
- ⚠ A mock backend is worth having first — it makes the conformance suite runnable with neither sipx nor
  an exchange, exactly as `room`'s `mock` does today.
- ⚠ Open, and it should be decided here: whether the remote wire is a flux-exchange **channel API** or a
  [C-399](C-399-remote-guarded-io-backend.md) **guarded-IO port delegation**. They are different
  abstractions and only one should carry this.

## Progress

- Filed 2026-08-01. Replaces an earlier framing of this story that treated remote termination as
  contrary to flux's grain — corrected against `ecosystem.md` and C-399, which say the opposite.
