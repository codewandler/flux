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
| who terminates the call | a local sipx process flux drives | [flux-exchange](ecosystem.md) |
| who holds the SIP credential | the operator's machine | the exchange, per tenant |
| what flux sees | channel events over a local control wire | channel events over a WebSocket |
| what flux links | nothing | nothing |

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

## ⚠ In the native locality, sipx is a process — not a linked crate

This constraint applies to **flux linking sipx**, and it is decided by precedent. D-205 rejected
`tokio-xmpp` for a structural reason, in its own words:

> *"it opens its own TCP socket and resolves its own DNS, so its egress cannot be routed through
> `flux_system::net::guard_url_scoped` — and it drags a full XEP stack and a second TLS backend."*

sipx is that class at larger scale: `sipx-transport`, `sipx-rtp` and `sipx-media` exist to own sockets;
SIP resolves NAPTR→SRV→A; RTP binds UDP ports per call. Linking it into flux would install a second
egress path beside the guard, which `AGENTS.md` prohibits.

⚠ **This says nothing about flux-exchange linking sipx.** The exchange is a different domain with its
own trust boundary; whether it embeds sipx or runs it beside itself is the exchange's decision, and
this design should not make it.

**sipx already designed the seam for the native case.** `sipx-app-protocol` is the `sipx.app.v1`
contract — `Envelope`/`CallSnapshot`/`EventKind` host→app, `Document`/`Instruction` app→host — with a
**sans-IO** interpreter: *"nothing in this crate opens a socket, reads a clock, or wants an async
runtime."* `sipx-app` is the host process meant to be driven by customer code. flux is that customer
code.

⚠ Two scheduling facts:

1. **None of sipx's transports exist yet** — *"what is not here is any of the three transports that
   would let customer code drive it (`A-2`, `A-4`, `A-5`), so the host runs no app callback yet."* The
   **native** binding is blocked upstream. The seam, the semantics and the remote binding are not.
2. **The contract is experimental** — *"`sipx.app.v1` may change incompatibly until two dissimilar
   applications have run against it — an inbound IVR and an outbound notifier."* This epic asks for
   exactly inbound and outbound, so **flux can be both of sipx's two stabilizing applications** — which
   turns an awkward coupling into influence over the contract while it is shapeable. A deliberate
   cross-repo decision, not a drift.

## Approach

Seven stories. The **semantics** (D-226/D-227/D-229) need no transport and are the part most likely to
be rushed once wiring works, so they are settleable now.

- **D-225 — one channel, two localities.** The locality-independent channel vocabulary and the parity
  requirement: the same `.flux` program runs against either backend without knowing which.
- **D-230 — the native backend.** A local sipx process over `sipx.app.v1`. Blocked upstream.
- **D-231 — the remote backend.** flux-exchange terminates; flux exchanges channel events over a
  WebSocket. Consumes [C-399](../stories/C-399-remote-guarded-io-backend.md), whose ownership is
  already decided in exactly this direction.
- **D-226 — inbound.** ⚠ SIP `From` headers are trivially forged: the caller is `Untrusted`, always.
- **D-227 — outbound.** ⚠ Dialling bills money and rings a human: approval-gated, default-deny
  destination allowlist, normalization inside the check.
- **D-228 — one voice-turn machinery**, shared with rooms (D-209/D-210), not a second path.
- **D-229 — what redaction cannot reach.** ⚠ The `Redactor` works on text; DTMF is how people type PINs.

## Alternatives considered

- **Link `sipx` into flux.** Simplest. Rejected on the D-205 precedent — a second egress path beside
  `guard_url_scoped`. Revisit only if sipx grows a way to hand socket construction to the host.
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
- ⚠ **Native is blocked upstream** on sipx `A-2`/`A-4`/`A-5`; the wire may break in a patch release.
  Pin exactly and treat a bump as a reviewed change.
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
