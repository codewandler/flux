---
id: D-225
title: "flux drives a sipx host over `sipx.app.v1` — as a separate process, never a linked library"
pillar: Agent
status: blocked
design: docs/designs/sip-channel.md
epic: sip-channel
areas: [flux-channels]
note: "⚠ BLOCKED UPSTREAM: sipx-app's own docs say none of the three transports that would let customer code drive it exist yet (A-2/A-4/A-5), so the host runs no app callback. ⚠ And linking sipx in is refused on the D-205 precedent — it owns its own sockets and DNS, so its egress cannot route through guard_url_scoped"
---

# The seam, and why it is a process boundary

## Goal

flux drives a sipx host process over the `sipx.app.v1` contract, so sipx owns the sockets and flux owns
the decisions.

## ⚠ Why a process, decided by precedent rather than preference

D-205 rejected `tokio-xmpp` for a structural reason, in its own words: *"it opens its own TCP socket
and resolves its own DNS, so its egress cannot be routed through `flux_system::net::guard_url_scoped`
— and it drags a full XEP stack and a second TLS backend."*

sipx is the same class, larger: `sipx-transport`, `sipx-rtp` and `sipx-media` exist to own sockets; SIP
resolution is NAPTR→SRV→A; RTP binds its own UDP ports per call. Linking it installs a second egress
path beside the guard, which `AGENTS.md` prohibits outright.

**The good news:** sipx already designed this seam. `sipx-app-protocol` is the `sipx.app.v1` vocabulary
— `Envelope`/`CallSnapshot`/`EventKind` host→app, `Document`/`Instruction` app→host — with a **sans-IO**
interpreter (*"nothing in this crate opens a socket, reads a clock, or wants an async runtime"*), and
`sipx-app` is the host process meant to be driven by customer code. flux is that customer code.

## ⚠ Why this is `blocked`, and what is not

`sipx-app`'s own docs: *"What is not here is any of the three transports that would let customer code
drive it (`A-2`, `A-4`, `A-5`), so the host runs no app callback yet."* **flux cannot drive sipx
today.** Do not start wiring against an unbuilt binding.

The *semantics* are not blocked: [D-226](D-226-inbound-a-caller-is-untrusted.md),
[D-227](D-227-outbound-a-call-is-an-effect-that-costs-money.md) and
[D-229](D-229-what-redaction-cannot-reach.md) can all be settled now, and they are the parts most
likely to be got wrong under delivery pressure.

## Acceptance

- [ ] A `sipx.app.v1` client seam in `flux-channels`, driving a sipx host process. sipx is **not** a
      Cargo dependency of any flux crate that links into the binary.
- [ ] The transport is chosen and justified: a **full-duplex session** for live voice; webhook documents
      suit IVR shapes and are the wrong fit for a conversation. Say which and why.
- [ ] ⚠ **The sipx version is pinned exactly, and a bump is a reviewed change.** `sipx.app.v1` *"may
      change incompatibly until two dissimilar applications have run against it… this crate's public API
      and the bytes on the wire may both move in a patch release."* A dependency refresh that silently
      crosses a wire change would surface as calls failing in production.
- [ ] Sidecar death is survivable and surfaces as an operation failure, not a killed session — the same
      requirement D-208 places on the room media sidecar.
- [ ] No unbounded buffering of inbound audio; frames drop rather than grow without limit.
- [ ] Failing-first test: the channel's non-media surface works with no sidecar present.
- [ ] Full gate green.

## Notes

- ⚠ Mirror D-208 (the room media sidecar) deliberately. Two sidecar protocols with different
  death/backpressure semantics is a maintenance trap, and D-208 lands first.
- ⚠ **Cross-repo decision worth making explicitly**: sipx stabilizes `sipx.app.v1` once *"two dissimilar
  applications have run against it — an inbound IVR and an outbound notifier."* flux is being asked for
  exactly inbound and outbound. Volunteering flux as one or both makes the coupling deliberate and buys
  influence over the contract while it is still shapeable. Decide it; do not drift into it.
- sipx is `1.0.0-alpha.4` with public APIs explicitly not frozen.

## Progress

- Filed 2026-08-01 with the sip-channel epic. `blocked` on sipx `A-2`/`A-4`/`A-5`.
