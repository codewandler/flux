# Design: The SIP channel — flux answers the phone, and places calls

**Status:** proposed · **Pillar:** Agent · **Stories:** [D-225](../stories/D-225-one-sip-channel-two-localities.md) · [D-226](../stories/D-226-inbound-a-caller-is-untrusted.md) · [D-227](../stories/D-227-outbound-a-call-is-an-effect-that-costs-money.md) · [D-228](../stories/D-228-one-voice-turn-machinery.md) · [D-229](../stories/D-229-what-redaction-cannot-reach.md) · [D-230](../stories/D-230-the-native-sip-backend.md) · [D-231](../stories/D-231-the-remote-sip-backend.md)

## Why

A phone call is the oldest and widest channel there is. Giving flux an inbound number — it answers,
listens, acts, speaks — and an outbound one — it calls a person when something needs saying — puts the
agent where no chat integration reaches.

[`codewandler/sipx`](../../../sipx) is the stack: a Rust SIP/VoIP **user agent** — place and answer,
register, hold, transfer, session timers; G.711 and DTMF, Opus behind a feature; TLS and secure
WebSocket; SRTP with SDES where signalling protects the key.

**flux was built partway toward this already.** `flux-audio`'s own doc names the target: *"telephony's
8 kHz, WebRTC's 48 kHz, a device mic's 16 kHz, versus whatever a model speaks natively"* — PCM16
conversion both endiannesses, a phase-carrying streaming `Resampler`, a `Framer`. G.711 is 8 kHz. The
sample-math layer under a telephony voice pipeline is written and dependency-free.

## The shape: one channel, two localities — and neither is required

A SIP call can be terminated in two places, and **both are first-class**:

| | **native** | **remote** |
|---|---|---|
| who terminates the call | sipx, embedded in flux | [flux-exchange](ecosystem.md) |
| who opens the sockets | **flux** — sipx is handed its IO | the exchange |
| who holds the SIP credential | the operator's machine | the exchange, per tenant |
| what flux sees | the channel vocabulary, in-process | channel events over a WebSocket |
| what guards the traffic | flux's own `net.rs` guard | the exchange's own posture |

This is the `kubectl` shape the owner named: the same vocabulary whether the thing serving it is
across a socket on your laptop or across the network behind a cert. flux writes and reads the same
channel events either way; only the binding differs.

⚠ **Neither locality may become mandatory**, and this is doctrine, not preference:

> **flux must never *require* flux-exchange.** A `.flux` program loading a connector module on a
> laptop is a complete path. Trading plugin-binary distribution pain for service lock-in would be a
> bad trade made twice. — [ecosystem.md](ecosystem.md)

and, from [C-399](../stories/C-399-remote-guarded-io-backend.md)'s ownership decision:

> flux owns it, flux-exchange reuses it. **flux must be able to do this locally as dev without
> depending on a service — that is the local-first principle, not a convenience.**

⚠ **An earlier draft of this design got that backwards**, rejecting remote termination as *"the
opposite of what flux argues about keeping things on the machine."* That criterion is wrong. The
ecosystem design already amended it: *"flux-exchange **is** a path flux traffic takes in a hosted
deployment."* The line that survives is narrower and is stated above — never *required*, in either
direction.

### The ecosystem's own test says who owns what

`ecosystem.md` gives each domain one mechanical interrogative — *"a boundary that requires taste is a
boundary that erodes"*:

- **flux (engine)** — *does it change what happens when an effect executes?* Owns the envelope, the
  substrate, Flux-Lang, the agent, the SDK. **Knows kinds, never vendors.**
- **flux-exchange** — *does it require holding a credential or knowing a tenant?* Owns principals,
  connections, credentials, **channels**, leases, stored programs, execution records.

A SIP trunk needs a registrar credential and belongs to a tenant, so **in a hosted deployment the
exchange owns the channel** — its README already says it *"terminates channels."* flux's side stays a
**kind**: "a voice call channel", never "this SIP provider". That split is what lets the same program
run against a local sipx process or a hosted exchange without knowing which.

Rooms already prove the pattern in-repo: one `room` channel with `mock`, `xmpp` and `jaas` backends
side by side. The SIP channel is the same idea with locality as the axis.

## ⚠ Natively, sipx takes its IO from flux — it is not a sidecar, and not an unguarded link

An earlier draft made sipx a separate process, citing D-205's rejection of `tokio-xmpp`. That was wrong
twice, and the corrections are the most important part of this design.

**1. The precedent does not transfer.** D-205's reason was: *"it opens its own TCP socket and resolves
its own DNS, so its egress **cannot** be routed through `guard_url_scoped`."* The operative word is
*cannot* — a third-party crate that could not be changed. **We own sipx.** "This library owns its
sockets" is a fact about a current API, not a law, and for a library we control it is a design decision
we can revisit. Applying a precedent without checking whether its premise held was the mistake.

**2. ⚠ A sidecar would not have satisfied the invariant — it would have hidden the violation.** sipx in
another process still resolves its own DNS and opens its own sockets; flux merely cannot see it. That is
*isolation*, not *guarding*, and the property `AGENTS.md` states — egress through one guard — would have
been quietly false while looking satisfied. **Injection makes it actually true**, which is the whole
argument for it.

**Injection is the pattern flux already prescribes.** `crates/flux-system/src/port.rs`: *"This module
states the same guarded operations as capability ports so a non-native substrate can serve them
instead… **This is not a second IO path.** … The port makes the caller substitutable, not the guard."*
It even prescribes the shape — no god trait, split by guarded resource, and a consumer spanning families
*"declares its own bundle (see `flux_plugin::PluginSystem`)"*.

And sipx already has the seams as concrete types: `resolve::{Naptr, Resolver, Srv, resolve}` and
`endpoint::{Config, Handle, bind}` are exported today. They need to become injectable, not to be
invented.

### ⚠ The prerequisite flux does not have yet

Measured 2026-08-01: `port.rs` declares **four** traits — `GuardedEnv`, `GuardedProcess`,
`GuardedHostFiles`, `GuardedWorkspaceFiles` — and **none for the network**. Egress guarding lives in
`net.rs` as free functions, usable by flux's own callers but not by a consumer that must be *handed* its
IO. And **inbound is scattered rather than absent**: `flux-server` has `guard_open_bind`, while
[C-409](../stories/C-409-channel-served-http-has-no-resource-limits.md) found the channel adapters that
bind their own listeners *"got none of it."*

SIP needs all three: resolve, dial (UDP **and** TCP), and **bind a local port to receive** — RTP is
bidirectional, and inbound SIP needs a listener. [C-396](../stories/C-396-datagram-dial-targets.md)
landed guarded UDP dial today; **the inbound half does not exist.**
[C-435](../stories/C-435-a-guarded-network-port.md) is that work, filed under
[execution-substrate](execution-substrate.md) where it belongs — and ⚠ **sipx is exactly the "second
consumer" that epic exists for**, which is C-395's own argument verbatim: a port with no second consumer
*"would be indirection without a seam"*, and a second consumer is the condition that expires it.

`sipx.app.v1` and `sipx-app` remain the right seam for a **hosted** sipx (the remote locality). They are
not rivals to embedding; they answer a different question.

## Approach

Seven stories. The **semantics** (D-226/D-227/D-229) need no transport and are the part most likely to
be rushed once wiring works, so they are settleable now.

- **D-225 — one channel, two localities.** The locality-independent channel vocabulary and the parity
  requirement: the same `.flux` program runs against either backend without knowing which.
- **D-230 — the native backend.** sipx embedded, taking its sockets and resolver from flux, so SIP and
  RTP go through the one guard. Blocked on [C-435](../stories/C-435-a-guarded-network-port.md) and on
  sipx growing the injection seam.
- **D-231 — the remote backend.** flux-exchange terminates; flux exchanges channel events over a
  WebSocket. Consumes [C-399](../stories/C-399-remote-guarded-io-backend.md), whose ownership is
  already decided in exactly this direction.
- **D-226 — inbound.** ⚠ SIP `From` headers are trivially forged: the caller is `Untrusted`, always.
- **D-227 — outbound.** ⚠ Dialling bills money and rings a human: approval-gated, default-deny
  destination allowlist, normalization inside the check.
- **D-228 — one voice-turn machinery**, shared with rooms (D-209/D-210), not a second path.
- **D-229 — what redaction cannot reach.** ⚠ The `Redactor` works on text; DTMF is how people type PINs.

## Alternatives considered

- **Link `sipx` and let it open its own sockets.** Rejected — that genuinely is a second egress path
  beside `guard_url_scoped`. This is the only reading of the D-205 precedent that survives.
- **Run `sipx` as a sidecar process.** ⚠ Rejected, and worth stating why since it was this design's
  first answer: a sidecar does not guard anything, it relocates unguarded egress one process away.
  Acceptable only as a fallback if injection proves impractical in sipx — and then it must be
  documented as *isolation, not guarding*, so nobody later reads it as satisfying the invariant.
- **Drive the `sipx` CLI.** Process isolation for free. Rejected: the CLI is *"a scriptable phone, not
  a desktop softphone"* that **reads and writes WAV files**, and its `dial`/`register` select UDP or TCP
  only. File-based media cannot carry a live conversation.
- **Remote-only, via the exchange.** Tempting — it is where credentials and tenancy belong anyway, and
  it sidesteps sipx's unbuilt transports entirely. ⚠ Rejected as the *whole* answer because it would
  make flux require a service, which the ecosystem charter forbids in as many words.
- **Native-only.** Rejected symmetrically: a hosted deployment has nowhere to put a per-tenant SIP
  credential, and the exchange exists precisely to hold it.
- **A telephony SaaS backend (Twilio and similar).** Not rejected — **deferred**. It solves NAT and
  media relay, and under the two-locality shape it is simply a third backend beside native and remote,
  exactly as rooms carry `xmpp` and `jaas`. It does not belong in the first cut, and nothing here should
  preclude it.

## Risks & open questions

- ⚠ **Parity is the thing that rots.** Two backends drift, and the one that drifts silently is the one
  nobody demos. D-225 must make parity testable, not aspirational.
- ⚠ **Native is blocked on C-435** (no network port, no guarded inbound) and on sipx accepting injected
  IO — both ours, neither free. ⚠ **RTP binds a local port per call and receives from a remote that may
  differ from the one dialled**; an inbound design shaped around "accept a connection" will not fit
  datagram media.
- ⚠ **Toll fraud** (D-227) — financial and fast. ⚠ **Audio disclosure** (D-229) — redaction cannot reach
  it.
- **No ICE in sipx.** NAT traversal is limited natively; the remote locality largely dissolves this,
  which is a real argument for it rather than a nice-to-have.
- **Latency budget.** A telephone conversation is unforgiving, and the remote locality adds a network
  hop to every turn. Measure per locality; do not assume one number covers both.
- **Open:** does flux `REGISTER` (so it has a dialable number) or answer on a static route? Natively
  that means holding a SIP credential on the machine — the exact thing the exchange exists to avoid.
- **Open:** whether the remote wire is a flux-exchange channel API or a `C-399` guarded-IO port
  delegation. They are different abstractions and only one should carry this.

## Acceptance / done

- flux answers an inbound call, holds a spoken conversation, acts through the normal envelope, and hangs
  up — with the caller `Untrusted` throughout.
- flux places an outbound call only through the approval envelope and only to an allowlisted destination.
- ⚠ The **same `.flux` program** runs against the native and remote backends without knowing which, and
  a test proves it.
- Neither locality is required: no service dependency for local use, no local sipx needed for hosted use.
- Exactly one voice-turn machinery serves rooms and SIP.
- What flux records from a call is decided, and refuses rather than retains what it cannot redact.
