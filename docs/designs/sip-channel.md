# Design: The SIP channel — flux answers the phone, and places calls

**Status:** proposed · **Pillar:** Agent · **Stories:** [D-225](../stories/D-225-the-sip-sidecar-seam.md) · [D-226](../stories/D-226-inbound-a-caller-is-untrusted.md) · [D-227](../stories/D-227-outbound-a-call-is-an-effect-that-costs-money.md) · [D-228](../stories/D-228-one-voice-turn-machinery.md) · [D-229](../stories/D-229-what-redaction-cannot-reach.md)

## Why

A phone call is the oldest and widest channel there is. Giving flux an inbound number — it answers,
listens, acts, speaks — and an outbound one — it calls a person when something needs saying — puts the
agent where no chat integration reaches.

[`codewandler/sipx`](../../../sipx) is the stack to do it with: a SIP/VoIP **user agent** in Rust —
place and answer, register, hold, transfer, session timers; G.711 and DTMF, Opus behind a feature; TLS
and secure WebSocket; SRTP with SDES where signalling protects the key.

**flux was built partway toward this already.** `flux-audio`'s own doc names the target: *"telephony's
8 kHz, WebRTC's 48 kHz, a device mic's 16 kHz, versus whatever a model speaks natively"* — PCM16
conversion both endiannesses, a phase-carrying streaming `Resampler`, a `Framer`. G.711 is 8 kHz. The
sample-math layer under a telephony voice pipeline is already written and dependency-free.

## ⚠ The architecture is decided by a precedent this repo already set

**sipx cannot be an in-process library dependency of flux.** Not for taste — for the same structural
reason `tokio-xmpp` was rejected in D-205, quoted from that design:

> *"`tokio-xmpp` was rejected for a reason that is structural rather than aesthetic: it opens its own
> TCP socket and resolves its own DNS, so its egress cannot be routed through
> `flux_system::net::guard_url_scoped` — and it drags a full XEP stack and a second TLS backend."*

sipx is that same class, larger: `sipx-transport`, `sipx-rtp` and `sipx-media` exist precisely to own
sockets. SIP resolution is NAPTR→SRV→A, and RTP binds its own UDP ports per call. Linking it in would
put a whole second egress path beside the guard — and `AGENTS.md` names guarded IO a safety invariant
with an explicit prohibition on a second path.

**So flux drives sipx as a separate process.** That is not a workaround; it is the same shape D-208
chose for room media (headless browser owns WebRTC, flux drives it over a thin local control protocol),
and it puts the trust boundary where it can be reasoned about.

### ⚠ And sipx has already designed exactly that seam — but has not built the transport

`sipx-app-protocol` is the `sipx.app.v1` contract: `Envelope`/`CallSnapshot`/`EventKind` host→app,
`Document`/`Instruction` app→host, with a **sans-IO** interpreter — *"nothing in this crate opens a
socket, reads a clock, or wants an async runtime."* `sipx-app` is the host process that terminates
calls and is driven by customer code over that contract. flux is *exactly* the customer code that
contract describes.

Two things that must be stated plainly, because they set this epic's schedule:

1. ⚠ **None of the transports exist yet.** `sipx-app`'s own docs: *"What is not here is any of the
   three transports that would let customer code drive it (`A-2`, `A-4`, `A-5`), so the host runs no
   app callback yet."* **flux cannot drive sipx today.** The blocker is upstream, not here.
2. ⚠ **The contract is experimental and may break in a patch release.** *"`sipx.app.v1` may change
   incompatibly until two dissimilar applications have run against it — an inbound IVR and an outbound
   notifier — after which a change requires a new line."*

That second point cuts both ways, and it is the opportunity in this epic: sipx stabilizes its contract
once **an inbound and an outbound application** have run against it. This epic is asking for exactly an
inbound *and* an outbound channel. **flux can be both of sipx's two dissimilar applications** — which
makes the coupling deliberate rather than unlucky, and gives flux a say in the contract while it is
still shapeable. That should be a decision, not an accident.

## Approach

Five stories. ⚠ The wiring is blocked upstream; the **semantics are not**, and they are the part most
likely to be got wrong. D-226, D-227 and D-229 can be settled now.

### D-225 — the sidecar seam

flux drives a sipx host process over `sipx.app.v1`. Own the transport choice (full-duplex session for
live voice; webhook documents suit IVR shapes), the pinning against an experimental wire, and
sidecar-death behaviour. Blocked on sipx's `A-2`/`A-4`/`A-5`.

### D-226 — inbound: the caller is untrusted, and caller ID proves nothing

⚠ **SIP `From` headers are trivially forged.** An inbound caller is `Untrusted`, without exception, and
the adapter is the component that knows it — which is exactly what
[C-416](../stories/C-416-a-channel-adapter-should-declare-its-principal.md) asks every adapter to
declare. C-408's `unauthenticated_participant` is the constructor; do not add a second.

### D-227 — outbound: a call is an effect that costs money and reaches a person

⚠ Placing a call is not a read. It bills, and it makes a phone ring in someone's hand. It must be a
destructive, approval-gated effect with a **destination allowlist** — the telephone analogue of the
egress guard. A model that can choose a dialled number is a premium-rate toll-fraud vector, and this is
the story that must not be softened for ergonomics.

### D-228 — one voice-turn machinery, not two

⚠ Rooms are building the same thing right now: D-209 (audio in, attributed speech), D-210 (audio out,
interruptible). `crates/flux-flow/src/voice/` already holds `driver`, `sink`, `speaker`, `transcript`
and `room_transcript`, and `VoiceTurnHandler` is its seam. A SIP call is a room with one remote
participant and a worse codec. Building a second voice path would guarantee they drift.

### D-229 — what redaction cannot reach

⚠ The `Redactor` operates on **text**. A spoken secret in recorded audio is not redactable by anything
flux has, and **DTMF is how people enter PINs and card numbers** — sipx supports DTMF, so flux will
receive them. This story decides what is recorded, what is refused, and what is said out loud about it.

## Alternatives considered

- **Link `sipx` as a Cargo dependency.** Simplest and fastest. Rejected on the D-205 precedent: it
  installs a second egress path beside `guard_url_scoped`, against a stated safety invariant. Revisit
  only if sipx grows a way to hand socket construction to the host.
- **Drive the `sipx` CLI.** Attractive — process isolation for free. Rejected: the CLI is *"a scriptable
  phone, not a desktop softphone"* that **reads and writes WAV files** and whose `dial`/`register`
  select UDP or TCP only. File-based media cannot carry a live conversation, and no TLS from the CLI
  rules out encrypted signalling.
- **A telephony SaaS (Twilio and similar) instead of sipx.** Genuinely less work and it solves NAT and
  media relay. Rejected as the basis: it puts the call — audio included — through a third party, which
  is the opposite of what flux argues about keeping things on the machine. Worth revisiting as an
  *additional* backend, exactly as rooms carry `xmpp` and `jaas` side by side.
- **Wait for `sipx.app.v1` to stabilize.** Safe. Rejected: stabilization is *defined* as two dissimilar
  applications having run against it, so waiting is waiting for someone else to be the guinea pig on a
  contract flux would rather influence.

## Risks & open questions

- ⚠ **Blocked upstream.** No sipx transport exists yet. Track `sipx`'s `A-2`/`A-4`/`A-5`; do not start
  D-225's wiring against a moving, unbuilt binding.
- ⚠ **Experimental wire that can break in a patch release.** Pin exactly, and treat a sipx bump as a
  reviewed change, not a dependency refresh.
- ⚠ **Toll fraud.** See D-227. The failure mode is financial and fast.
- ⚠ **Audio disclosure.** See D-229. Redaction does not reach it.
- **No ICE in sipx.** NAT traversal is limited; deployment needs a SIP-aware path. A demo behind a home
  router is not the same test as production.
- **Latency budget.** A telephone conversation is unforgiving: resample, model, resample back, all
  inside a turn people will not wait through. Rooms (D-209/D-210) hit this first and should measure it
  first.
- **Open:** does flux register (REGISTER, so it has a number people can dial) or answer on a static
  route? Registration means holding a credential for a SIP provider.
- **Open:** which of sipx's two stabilizing applications flux volunteers to be — inbound, outbound, or
  both. This is a cross-repo commitment and should be made explicitly.

## Acceptance / done

- flux answers an inbound call, holds a spoken conversation, acts through the normal envelope, and hangs
  up — with the caller treated as `Untrusted` throughout.
- flux places an outbound call only through the approval envelope and only to an allowlisted
  destination.
- Exactly one voice-turn machinery serves both rooms and SIP.
- What flux does and does not record from a call is decided, documented, and refuses rather than
  silently retaining what it cannot redact.
- sipx runs as a separate process; no second egress path is linked into flux.
