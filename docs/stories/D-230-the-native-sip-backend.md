---
id: D-230
title: "The native backend — a local sipx process driven over `sipx.app.v1`"
pillar: Agent
status: blocked
design: docs/designs/sip-channel.md
epic: sip-channel
areas: [flux-channels]
note: "⚠ BLOCKED UPSTREAM: sipx-app's own docs say none of the three transports that would let customer code drive it exist yet (A-2/A-4/A-5) — `the host runs no app callback yet`. ⚠ And sipx is a PROCESS, not a linked crate: the D-205 precedent refuses a dependency that owns its own sockets and DNS"
---

# The local half — sipx as a process flux drives

## Goal

Terminate a call on the operator's own machine: flux drives a local sipx host over `sipx.app.v1`.

## ⚠ Why a process rather than a crate

D-205 rejected `tokio-xmpp` structurally: *"it opens its own TCP socket and resolves its own DNS, so
its egress cannot be routed through `flux_system::net::guard_url_scoped` — and it drags a full XEP
stack and a second TLS backend."*

sipx is that class at larger scale — `sipx-transport`, `sipx-rtp` and `sipx-media` exist to own
sockets, SIP resolves NAPTR→SRV→A, RTP binds UDP per call. Linking it into flux installs a second
egress path beside the guard, which `AGENTS.md` prohibits.

⚠ **This says nothing about flux-exchange linking sipx** — that is the exchange's decision in its own
trust domain ([D-231](D-231-the-remote-sip-backend.md)).

**sipx already designed this seam**: `sipx-app-protocol` is the `sipx.app.v1` contract with a **sans-IO**
interpreter — *"nothing in this crate opens a socket, reads a clock, or wants an async runtime"* — and
`sipx-app` is the host meant to be driven by customer code.

## Acceptance

- [ ] Drives a local sipx host over `sipx.app.v1`; sipx is **not** a Cargo dependency of any flux crate
      that links into the binary.
- [ ] Passes [D-225](D-225-one-sip-channel-two-localities.md)'s conformance suite unchanged.
- [ ] The transport is chosen and justified: a **full-duplex session** for live voice; webhook documents
      suit IVR shapes and are the wrong fit for a conversation.
- [ ] ⚠ **The sipx version is pinned exactly and a bump is a reviewed change.** `sipx.app.v1` *"may
      change incompatibly … this crate's public API and the bytes on the wire may both move in a patch
      release."* A dependency refresh silently crossing a wire change surfaces as calls failing live.
- [ ] Sidecar death is survivable and surfaces as an operation failure — the same requirement D-208
      places on the room media sidecar. ⚠ Mirror D-208's death/backpressure semantics rather than
      inventing a second set.
- [ ] No unbounded buffering of inbound audio.
- [ ] Full gate green.

## Notes

- **Blocked on sipx `A-2`/`A-4`/`A-5`.** Do not wire against an unbuilt binding.
- ⚠ Cross-repo decision worth making deliberately: sipx stabilizes `sipx.app.v1` once *"two dissimilar
  applications have run against it — an inbound IVR and an outbound notifier."* This epic is exactly
  inbound and outbound, so flux can be both — buying influence over the contract while it is shapeable.
- No ICE in sipx: NAT traversal is limited natively. The remote locality largely dissolves this.

## Progress

- Filed 2026-08-01 with the sip-channel epic. Split out of the former D-225 when the two-locality shape
  was corrected.
