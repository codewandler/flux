# Design: meeting rooms — a multi-party channel where humans and agents meet

**Status:** proposed (2026-07-30) · **feasibility proven live** against a real Brave Talk room ·
**Pillar:** Agent · **Layer:** L6 (new `flux-rooms` module/crate under `flux-channels`) + L3 seam
(`flux-flow::voice`) · **Epic:** [D-203](../stories/D-203-meeting-rooms-epic.md) · **Owner:** Timo

## Why

Every channel flux has is either **1:1** or **fire-and-forget**. `flux-channels` (D-04, D-09) carries
`schedule` / `webhook` / `slack` / `a2a` — each an event that wakes a journey or an agent and returns.
The voice path (D-06, D-132) is richer but assumed **exactly one caller**: before D-204,
`VoiceTurnHandler::turn(&self, user_text: &str)` had no notion of *who* spoke, because on a phone line
there is only one candidate. D-204 changed that signature to carry a `Speaker`; the paragraphs below
describe the problem as it stood, and the "As landed" block records what shipped.

There is no channel where flux is **one participant among several** — where two humans and one or more
agents are co-present, the agent hears everything but is addressed by only some of it, and the agent can
**show** something instead of only saying it.

A meeting room is exactly that shape: **presence + text + audio + video, many parties.** It is also, per
the framing that motivated this design, the *root* substrate in which **agents can meet** — not merely an
input channel for one agent, but a place a fleet convenes, with humans in the same room. That connects it
to the fleet work (A-111 coordinator, A-119/A-120 agent address) rather than leaving it a voice curiosity.

## Feasibility — what was measured, not assumed (2026-07-30)

A spike joined a live Brave Talk room (`talk.brave.com`, a room the author created and invited the agent
to) from a **plain HTTP/WebSocket client — no browser, no Brave account, no Brave Premium.** Every step
below was observed, and the sequence is what any room backend must implement.

Brave Talk is [8x8's Jitsi-as-a-Service (JaaS)](https://www.8x8.com/resources/customer-stories/brave)
with a Brave-operated token service in front. The client is open source
([`brave/brave-talk`](https://github.com/brave/brave-talk)), which is how the handshake was derived.

**1. Guest token — public, unauthenticated.** `src/rooms.ts` joins an existing room with `PUT`
(`POST` = create, which *does* require a subscriber cookie):

```
OPTIONS https://talk.brave.com/api/v1/rooms/<room>     → x-csrf-token: …  + _gorilla_csrf cookie
PUT     https://talk.brave.com/api/v1/rooms/<room>     → 200 {"jwt": "…"}
        x-csrf-token: <token>, body {"mauP": true}
```

The returned RS256 JWT carried: `aud: jitsi`, `iss: chat`, `sub: vpaas-magic-cookie-a4818bd…` (Brave's
JaaS tenant), `room: <the exact mixed-case room name>`, `context.user.moderator: "false"`,
`context.features.recording/transcription: "false"`, and **10800 s (3 h) validity**. Creating a room needs
Premium; **joining one needs nothing at all.**

**2. Focus allocation.** JaaS wants an HTTP conference-request before signalling:

```
POST https://8x8.vc/<tenant>/conference-request/v1?room=<room>
     Authorization: Bearer <jwt>
  → 200 {"ready": true, "focusJid": "focus@auth.8x8.vc",
         "room": "<room-lowercased>@conference.<tenant>.8x8.vc"}
```

Note the asymmetry that will bite an implementor: **the MUC JID lowercases the room name while the JWT
`room` claim keeps the original case.** Use the JID from this response, not a locally-built one.

**3. Signalling — and the token does *not* ride SASL.** `wss://8x8.vc/<tenant>/xmpp-websocket?room=<room>&token=<jwt>`,
subprotocol `xmpp`. The server offers **only `ANONYMOUS`** — with or without the token in the URL. `PLAIN`
with the JWT as password is rejected `<invalid-mechanism/>`. Authorization happens at *focus* and via the
URL-borne token, not in SASL. So: `<open/>` → SASL `ANONYMOUS` → `<open/>` → resource bind → MUC presence.

**4. Presence and text — no WebRTC involved.** Joining the MUC put the agent in the room as a visible
occupant; the occupant list read back the human, `focus`, and `flux-agent`. A `type='groupchat'` message
landed in the human's Brave Talk chat pane, and inbound human messages were read off the same socket.
**This is the load-bearing finding: text + presence are pure XMPP.** No media stack, no browser.

**5. Two RFC 7395 details that cost time.** Every stanza must be namespace-qualified
(`<iq xmlns='jabber:client' …>`) — without it prosody answers `<unsupported-stanza-type/>` and closes the
stream. And a whitespace keepalive is **illegal** on the WebSocket binding: sending `" "` closes the
connection with `1007 Invalid payload start character`. Keepalive must be an XMPP ping IQ.

### What this splits the design into

| Modality | Requires | Verdict |
|---|---|---|
| Presence (join, occupant list, leave) | XMPP MUC | **proven, native Rust viable** |
| Text (groupchat + private) | XMPP MUC | **proven, native Rust viable** |
| Audio in/out | full WebRTC (ICE/DTLS-SRTP, Jingle, simulcast) | needs a media peer |
| Screenshare out | full WebRTC + a video source | needs a media peer |

## The media problem, and the decision

Audio and screenshare need a real WebRTC endpoint. Three ways:

- **(a) Headless-Chrome sidecar — the [Jibri](https://github.com/jitsi/jibri) pattern.** A browser process
  runs `lib-jitsi-meet` in-page and exposes a thin control protocol (NDJSON over a local WebSocket) to
  flux: PCM frames in/out, `sendChatMessage`, and a canvas whose `captureStream()` is published as the
  bot's video track. Chrome does all the WebRTC.
- **(b) Native `webrtc-rs`.** Means reimplementing lib-jitsi-meet's signalling — Jingle over XMPP, ICE,
  DTLS-SRTP, simulcast, the endpoint data channel. Heavy, and brittle against JaaS changes we don't control.
- **(c) SIP dial-in via [jigasi](https://github.com/jitsi/jigasi).** A server-side component of a
  *self-hosted* deployment; not exposed on Brave's tenant. Unavailable to us.

**Decision: split the port.** Presence + text are **native** and ship without any browser dependency;
media is an **optional, feature-gated sidecar** (a). This keeps the text channel — the half the spike
proved and the half most of the value sits in — free of a ~200 MB runtime dependency, and keeps CI honest
(invariant 6).

### Measured media findings — audio works, and none of the documented recipes are why

The same 2026-07-30 spike drove a real call from headless Chrome 150 via the JaaS external API and got
**audio confirmed audible by the human in the call.** Everything about *how* is a scar worth recording,
because every widely-documented approach failed:

- **Chrome 150 ignores `--use-fake-device-for-media-capture`.** Device labels stay real and even Chrome's
  own built-in beep tone never appears — an in-page RMS probe read `peak 0.0004` (silence) both with and
  without `--use-file-for-fake-audio-capture`. The standard Jibri/CI recipe is dead on this version.
- **Jitsi's `setAudioInputDevice` did not stick.** The page called it with the correct label and deviceId;
  `getCurrentDevices()` kept reporting `Default` and the published track stayed silent.
- **What worked:** a private PipeWire/Pulse **null sink + remapped source**, then moving *only our own*
  browser capture stream onto it with `pactl move-source-output <id> fluxagent_mic`. Per-stream, so the
  human's own microphone stream in the same call was never touched — which is the property that makes this
  safe to do on a machine someone is using.
- **`dominantSpeakerChanged` is not evidence of audible audio.** The bridge elected the bot dominant
  speaker while the human heard nothing. Hence invariant 8: publication needs a **level probe**, not a
  mute-state check.
- **Voice quality is a product decision, not a plumbing one.** `espeak-ng` was intelligible but robotic;
  `piper` with `en_US-ryan-high` (local, 43 s of speech synthesized in 7.6 s) is the one worth shipping,
  and keeping TTS local fits the room's consent posture.

**Screenshare, by contrast, is unproven.** `getDisplayMedia` in headless returns
`NotReadableError: Could not start video source` — headless has no display — and the auto-select capture
flags change nothing; `toggleShareScreen` yielded no sharing participant. A non-headless fallback on
`Xvfb :99` started Chrome but the page never executed within 180 s. The conclusion is to **stop chasing
desktop capture** and instead drive `lib-jitsi-meet` directly with a **canvas-sourced video track**, which
needs no display and no capture permission. That is D-211, and it is the reason D-211 sequences after
D-208 rather than beside it.

## Shape — a `Room` port with swappable backends

The house pattern for this is already established: a trait port with swappable implementations, exactly as
D-71's state store, the workboard port (A-113), and the agent-runtime port (A-121) do.

```rust
#[async_trait]
pub trait Room: Send + Sync {
    fn id(&self) -> &RoomId;
    async fn join(&self, identity: &RoomIdentity) -> Result<RoomStream>;  // stream of RoomEvent
    async fn occupants(&self) -> Result<Vec<Occupant>>;
    async fn say(&self, text: &str) -> Result<()>;                        // groupchat
    async fn whisper(&self, to: &OccupantId, text: &str) -> Result<()>;   // private
    async fn leave(&self) -> Result<()>;
}

#[non_exhaustive]
pub enum RoomEvent {
    Joined { occupant: Occupant },
    Left { occupant: OccupantId },
    Message { from: OccupantId, text: String, scope: MessageScope },
    /// sidecar-only, feature-gated
    Audio { from: OccupantId, frame: AudioFrame },
    SpeechStarted { from: OccupantId },
    Ended,
}
```

**As landed (D-204)** — four deliberate departures from the sketch as originally drafted. Note that two
of them (`#[non_exhaustive]`, `scope: MessageScope`) were folded back **into** the sketch above in the
same commit, so the sketch no longer visibly departs; they are still listed here because they are
decisions a D-205/D-206 implementor needs, not accidents. Source: —
`crates/flux-channels/src/rooms/`:

- **`Message` carries a `MessageScope`** (`Groupchat` | `Private`). Without it a whisper is
  indistinguishable from public text, which breaks *both* consumers downstream: D-207 treats a whisper
  as a stronger addressing signal than public text, and a reply has to go back the way it came. Adding
  the field later would have been a breaking change to the port D-205/D-206 implement.
- **`RoomEvent` is `#[non_exhaustive]`**, so the feature-gated media variants (D-208…D-211) are
  additive for downstream consumers. Inside `flux-channels` the driver still matches exhaustively on
  purpose: a new variant must fail to compile rather than be dropped on the floor.
- **`RoomStream` is an owned bounded receiver, not a `Stream`.** A backend's protocol loop is a task
  feeding a channel — that is literally what the XMPP WebSocket read loop is — and the consumer is a
  `select!` against a cancellation token. Bounded so a busy room backpressures its own socket instead
  of queueing unheard chatter. It also keeps `futures` out of this crate's dependency list.
- **`Occupant` carries `kind: OccupantKind` and `is_self`.** `kind` is what lets D-207 refuse to
  ping-pong with another agent's plain text and lets a driver ignore a MUC's own service occupants
  (Jitsi's `focus`); `is_self` is not cosmetic — a MUC echoes our groupchat messages back to us, so a
  consumer that cannot recognize itself answers itself forever.

**As landed (D-205)** — the portable backend, `crates/flux-channels/src/rooms/xmpp/`. Registered as
`backend = "xmpp"`, it implements the frame sequence in Feasibility above against any
prosody/ejabberd/JaaS MUC. Decisions a D-206 implementor inherits:

- **A parser, not an XMPP client.** `quick-xml` (MIT, one transitive dep already in the graph) plus a
  ~200-line element tree; the protocol is ours. `tokio-xmpp` was rejected for a reason that is
  structural rather than aesthetic: it opens its own TCP socket and resolves its own DNS, so its
  egress cannot be routed through `flux_system::net::guard_url_scoped` — and it drags a full XEP stack
  and a second TLS backend. The WebSocket is `tokio-tungstenite`, already in the tree for the realtime
  and codex providers, so no second TLS stack enters the graph.
- **The endpoint is guarded in its `http`/`https` form.** `flux_system::net` speaks HTTP schemes, so
  `wss://` is rewritten for the guard and the dialled URL is rebuilt from the guard's normalized
  answer. Loopback and private addresses need `allow_private_net` — the guard's scoped grant, not a
  bypass. Known gap, inherited from the guard's URL-returning API: the connection is **not pinned** to
  the vetted addresses, so this closes SSRF-by-configuration and not DNS rebinding. The endpoint is
  operator configuration and never model output.
- **`is_self` is decided from two independent signals**, because `Occupant::new` defaults it to
  `false` and a backend that forgets makes the agent answer its own echo forever: XEP-0045's
  `<status code='110'/>` (authoritative, and survives the service reassigning our nick), and the nick
  we joined under. The driver additionally re-checks the nick, so self-suppression no longer rests
  entirely on the backend.
- **The room JID is `OnceLock`-set from our own self-presence**, which is what lets `Room::id()` keep
  returning a borrow while still answering the *server's* spelling rather than the configured one.
- **`RoomSessionEnd` splits the two failures** (`crates/flux-channels/src/rooms/driver.rs`). The host
  ends the process on a channel error, which is right for a room that could never be joined and
  disproportionate for a socket that died mid-meeting — so a join failure is `run`'s `Err` and
  anything after it is `RoomSessionEnd::Failed`, logged and non-fatal. The driver also now leaves the
  room on **every** path out of the session, including a failed send.
- **History and non-client namespaces are dropped.** A `<delay/>`-marked groupchat message is the
  MUC's replay of what was said before flux arrived; answering it is the same unbounded-cost mistake
  as answering our own echo.
- **`OccupantKind` is `Unknown` for everyone but us and `focus`.** XMPP presence carries no
  human-or-bot signal and inventing one would be worse than admitting we cannot tell.

**The L3 turn seam changed with it (breaking):** `VoiceTurnHandler::turn` is now
`turn(&self, speaker: &Speaker, user_text: &str)`. `flux_flow::voice::Speaker` is a surface-owned id
plus an optional display name; a 1:1 surface passes `Speaker::sole()`, which is how a phone line's
single caller becomes *named* rather than absent.

Backends: **`XmppMucRoom`** (generic prosody/ejabberd MUC — the portable one), **`JaasRoom`** (Brave Talk
and any own-tenant JaaS: the guest-JWT + conference-request handshake above), and **`MockRoom`** for
tests. A room is a new `ChannelDecl` `kind` — `room`, with
`settings { backend, room, nick, address_rule }` — so it enters through `build_channels` and needs no new
host, exactly as D-04 established.

**Every inbound event carries an `OccupantId`.** That is the one change the existing turn seams needed: a
room has N speakers, and before D-204 `VoiceTurnHandler::turn` took only text. Attribution is not a feature,
it is the precondition for the address rule below. The one exception is `Ended` — the room's own
lifecycle terminator, which no participant causes; `RoomEvent::occupant()` returns `None` only there,
and a consumer that requires `Some` for everything else is holding the port to its contract.

## Multi-party is the design problem; the plumbing is the easy half

- **Addressing.** The agent hears everything and is the addressee of almost none of it. It must respond
  only when **addressed** — nick mention, private whisper, or a configured wake phrase — and otherwise
  accumulate context silently. Without this the agent answers every sentence two humans say to each other.
- **Agent-to-agent chatter.** Two flux agents in one room, both replying to mentions, will ping-pong from a
  single human message. Needs a per-room reply budget per unit time, and a rule that an agent never
  auto-replies to another agent's plain text — only to a structured A2A envelope.
- **Concurrency.** `AppDeliverer` serializes deliveries behind a mutex, deliberately (D-04: concurrent
  deliveries would double-process each other's broadcast cascade). A busy room delivers *continuously*, so
  **A-112 (per-delivery bus isolation) is a real prerequisite** for more than one room, or a room plus any
  other channel.

## Safety envelope

This is where the repo's fail-closed doctrine bites, and where the spike's own success is the warning.

1. **A room is untrusted multi-party input.** Any occupant can inject text, and — as the spike proved —
   **anyone holding the link can put a client in the room without an account.** Room-sourced turns get the
   same `Executor` + approver as a CLI turn; joining a room grants no authority whatsoever.
2. **Consent is owed, and the token doesn't cover us.** Our guest token carried
   `recording: false, transcription: false`. flux transcribing the room *locally* is not governed by that
   flag. The agent must announce itself on join, and a room transcript is evidence-stamped like any other
   artifact.
3. **A screenshare is an outbound publish.** A rendered pane showing a raw transcript could publish a
   credential to everyone on the call. The render path must run the same redaction as every other surface —
   reuse the C-215/C-216 corpus against it rather than trusting the renderer.
4. **Media publication is an approved act, not an implicit capability.** Joining, publishing audio, and
   publishing video are three distinct outward-facing actions in a room containing humans.

## Reuse — what already exists

- **`flux-channels`** — `Channel::start(deliverer, cancel)`, `Deliverer`, `build_channels(kind → adapter)`.
- **`flux-flow::voice`** — `VoiceSessionDriver::run_flow_turns`, `VoiceTurnHandler`,
  `VoiceReply::{Continue, Complete}`, `VoiceSink`, barge-in on `SpeechStarted`, `TranscriptAccumulator`.
  The room audio path is a second driver against this seam, plus speaker identity.
- **`flux-audio`** — PCM16 ⇄ samples, phase-carrying streaming `Resampler`, `Framer`. WebRTC is 48 kHz and
  OpenAI Realtime is 24 kHz: this crate exists for precisely this seam.
- **`RealtimeProvider`** (`flux-providers::realtime`, feature `realtime`) — voice-to-voice whose tool calls
  already traverse `Executor::dispatch`.
- **The agent-authored surface** (C-219…C-225) — typed panes the agent can draw. The screenshare should
  publish *that*, not invent a second rendering surface.
- **Fleet/A2A** (A-111, A-119, A-120) — for agents-meet-agents, a room becomes an `AgentAddress` transport.

## Invariants (verify before ship)

1. **Joining grants no authority.** An op requiring approval, triggered from a room message, is *denied*
   absent approval — the test asserts denial, not an approval prompt rendered into the room.
2. **The agent answers only when addressed.** A replayed transcript of two humans talking, N messages, none
   addressed to the agent → **zero** outbound messages and **zero** planner calls.
3. **Reply is bounded.** Two mock agents mentioning each other converge under a per-room reply budget
   instead of running to the agent cap.
4. **No unredacted publish.** A pane whose source text contains a credential shape publishes redacted
   (C-216 corpus, run against the render path).
5. **Self-announcement is not optional.** Join emits an identifying room message *before* the first inbound
   message is read.
6. **Text needs no browser.** ✅ **Met (D-205).** `crates/flux-channels/tests/xmpp_room.rs` drives the
   whole join → occupants → say → read → leave path against an in-process WebSocket double with no
   browser, no vendor SDK and no network; the media sidecar is feature-gated and skipped. The two
   RFC 7395 traps are regressed on the raw bytes:
   `every_stanza_the_xmpp_backend_emits_is_jabber_client_qualified` and
   `the_xmpp_keepalive_is_a_ping_iq_and_never_whitespace`.
7. **Token refresh is transparent.** A session crossing the 3 h guest-token expiry re-mints and stays
   joined.
8. **Published media carries signal.** A published track whose source is silence is reported as a failure,
   not as success — asserted with a level probe, because the spike watched the bridge elect a silent bot
   dominant speaker.

## Open questions

- **Brave Talk acceptable use.** The endpoint is public and unauthenticated, and the spike used it exactly
  as the open-source client does, against a room it was invited to. A bot joining calls *at scale* is a
  different posture. **Read Brave's ToS before this is anything but a spike** — and prefer the generic XMPP
  backend, or our own JaaS tenant, for anything beyond own-room use.
- **Own JaaS tenant vs Brave Talk.** An own 8x8 JaaS tenant (our API key, our JWT signing) is the
  supportable production answer and lifts the participant cap; Brave Talk is the zero-setup path for a
  human who already uses it. The `JaasRoom` backend should cover both — the only difference is where the
  JWT comes from.
- **The 4-participant free cap.** Two humans + two agents = 4, and our token carried
  `x-brave-features.group-room: "false"`. Multi-agent meetings hit Brave's free ceiling immediately.
- **E2EE.** Brave's "Video Bridge Encryption" (insertable streams) would leave a sidecar unable to decode
  media unless it joins the key exchange. Per Brave's own description, **chat is not covered** by it — so
  the text channel is unaffected either way.
