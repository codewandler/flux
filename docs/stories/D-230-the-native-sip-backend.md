---
id: D-230
title: "The native backend — sipx takes its sockets and resolver from flux, so SIP traffic goes through the one guard"
pillar: Agent
status: blocked
design: docs/designs/sip-channel.md
epic: sip-channel
areas: [flux-channels, flux-system]
note: "⚠ REPLACES the sidecar framing, which was wrong twice: the D-205 precedent's real reason was that tokio-xmpp CANNOT BE CHANGED, and we own sipx; and a sidecar does not satisfy the guard invariant — it relocates unguarded egress into another process. Blocked on C-435 (no network port, no guarded inbound) and on sipx growing the injection seam"
---

# Hand sipx its IO, and the guard actually covers the call

## Goal

Embed sipx in flux with its **socket construction and name resolution supplied by flux**, so SIP
signalling and RTP media are guarded by the same `flux-system` path as every other flux egress.

## ⚠ Why this replaces the sidecar design

The first draft of this story made sipx a separate process, citing D-205's rejection of `tokio-xmpp`.
Both halves of that were wrong:

**1. The precedent does not transfer.** D-205's stated reason was structural: *"it opens its own TCP
socket and resolves its own DNS, so its egress cannot be routed through `guard_url_scoped`."* The
operative word is **cannot** — `tokio-xmpp` is a third-party crate that could not be changed. **We own
sipx.** "This library owns its sockets" is a fact about a library's current API, not a law; for a
library we control it is a design decision we can revisit. Applying the precedent without checking
whether its premise held was the error.

**2. ⚠ A sidecar does not satisfy the invariant — it hides the violation.** sipx in another process
still resolves its own DNS and opens its own sockets; flux simply cannot see it. That is *isolation*,
not *guarding*, and the safety property `AGENTS.md` states — that egress goes through one guard — would
have been quietly false while looking satisfied. Injection makes it actually true.

**Injection is also the pattern flux already prescribes.** `port.rs`: *"This module states the same
guarded operations as capability ports so a non-native substrate can serve them… **This is not a second
IO path.** … The port makes the caller substitutable, not the guard."* And sipx already has the shape —
`resolve::{Naptr, Resolver, Srv, resolve}` and `endpoint::{Config, Handle, bind}` are exported today as
concrete types. They need to become injectable, not to be invented.

## Acceptance

- [ ] sipx accepts socket construction and name resolution from the host, and flux supplies them from
      `flux-system`. **No socket in the call path is opened by sipx's own defaults.**
- [ ] ⚠ **Every leg is guarded**: SIP signalling (UDP/TCP/TLS/WS) *and* RTP media. A design that guards
      signalling and lets media open its own ports has guarded the cheap half — media is where the
      audio is and where the ports are numerous.
- [ ] Resolution goes through flux's resolver, and the dialled address is the **vetted** one. C-396
      established the discipline: resolve once, vet, `connect` to the pinned address so the kernel
      enforces both send destination and reply source. ⚠ Do not let sipx re-resolve after the guard has
      decided — that is the rebinding window C-396's tests exist to close.
- [ ] Passes [D-225](D-225-one-sip-channel-two-localities.md)'s conformance suite unchanged, identically
      to the remote backend.
- [ ] ⚠ **A test proves the negative**: with flux's provider refusing, sipx reaches nothing. A test that
      only shows the happy path cannot distinguish "guarded" from "guarded except when it matters".
- [ ] Full gate green.

## Notes

- **Blocked on two things, both real:**
  1. [C-435](C-435-a-guarded-network-port.md) — flux has **no network port trait and no guarded inbound
     primitive**. RTP binds local ports; inbound SIP needs a listener. C-396 landed guarded UDP *dial*;
     inbound is unbuilt.
  2. sipx growing the injection seam. Cross-repo, and ours to schedule.
- ⚠ This makes sipx the **second consumer** the [execution-substrate](../designs/execution-substrate.md)
  epic was filed for — C-395's argument verbatim: a port with no second consumer *"would be indirection
  without a seam"*, and a second consumer is the condition that expires that reasoning. Worth saying out
  loud in that epic, because it changes C-435's priority from speculative to load-bearing.
- ⚠ If injection turns out to be impractical in sipx, the sidecar is the fallback — but it must then be
  documented as **isolation, not guarding**, so nobody later reads it as satisfying the invariant.
- `sipx.app.v1` and `sipx-app` remain the right seam for a *hosted* sipx; this story is about the
  embedded case. They are not rivals.

## Progress

- Filed 2026-08-01. Rewritten the same day after the owner pointed out that we own sipx, so the
  can't-change-it premise behind the sidecar design does not hold.
