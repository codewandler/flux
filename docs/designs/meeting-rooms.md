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

**As landed (D-206)** — the vendor backend, `crates/flux-channels/src/rooms/jaas/`. `JaasRoom` owns
exactly the two things that are vendor-specific — where the guest token comes from, and what happens
when it expires — and delegates everything else to D-205's `XmppMucRoom`. Decisions a follow-up
implementor inherits:

- **`JaasTokens` is the network boundary**, the same shape `flux_plugin::pack::Fetcher` uses and for
  the same reason: two operations scoped to `(room, token)` rather than to a caller-supplied URL, so
  the own-room posture holds *structurally* — there is no shape of the trait that enumerates rooms —
  and so `crates/flux-channels/tests/jaas_room.rs` never reaches Brave or 8x8.
- **The refresh re-joins rather than re-authenticating in place, and it closes before it opens.** The
  token rides the WebSocket URL, so a fresh token means a fresh socket. The tempting order — open the
  replacement first, swap under it, then close the old — **a real MUC refuses**: SASL is `ANONYMOUS`,
  so every connection is a *different* anonymous JID, and two overlapping sessions asking for one
  nick is XEP-0045 §7.2.9's nickname conflict, answered `<conflict/>`. So the order is mint (pure
  HTTP, touching neither the MUC nor the nick) → release the old session → take the nick back. Three
  consequences, none hidden: `say` fails with "not joined" during the gap rather than writing to a
  socket on its way out; a replacement join that keeps failing ends the room with `Ended`, because
  once the nick is released there is nothing to fall back on; and the handover is not atomic, so the
  replacement can meet its own predecessor and is retried. Transparency for the consumer comes from
  suppressing the replacement's replayed `Joined` events and from the deliberate `leave` emitting no
  `Ended`. Known gap: an occupant who leaves *between* sessions produces no `Left` (they are simply
  absent from `Room::occupants`, which reads the live session).
- **Leaving and refreshing race, and the pairing that settles it is explicit.** `JaasRoom::leave`
  cancels the pump's token and *then* takes the session out; the pump re-checks that token while
  holding the same lock before installing a replacement. Without that pairing a `leave` landing
  mid-refresh returns `Ok(())` while the replacement is installed behind it — joined, never left, and
  every later `join` answering "already joined", i.e. the room permanently un-rejoinable. That is the
  ordinary shutdown path (`RoomTurnDriver` breaks on cancellation, then leaves), and it is pinned by
  `tests/jaas_room.rs::leaving_while_a_replacement_join_is_in_flight_does_not_strand_it`.
- **The outgoing session's buffered events are drained before its stream is dropped**, so a refresh
  does not silently discard up to `DEFAULT_ROOM_EVENT_BUFFER` events — human messages among them —
  with no replay path (the replacement's MUC history arrives `<delay/>`-marked and is dropped).
- **A guest JWT is a secret that rides a query string.** `GuestToken`'s `Debug` redacts it, and the
  D-205 backend now renders an endpoint *without its query* in every error and `Debug` that names one
  (`xmpp::endpoint_for_display`) — a failed connect would otherwise publish the token to a log.
- **`BraveTalkTokens` is the vendor implementation** (`jaas/tokens.rs`), and every request it makes
  is **pinned** to the addresses `guard_url_scoped_pinned` vetted — `resolve_to_addrs` + `no_proxy`,
  redirects refused outright, an empty pin set failing closed. That is a stronger posture than the
  WebSocket path above, which the guard's URL-returning API cannot pin; it mirrors `flux-web`'s
  crawler rather than inventing anything. Redirects are refused specifically because one would carry
  the `Authorization: Bearer <jwt>` header off the vetted origin.
- **No vendor response body ever reaches an error message.** A failing response can echo our own
  token or CSRF value back at us, so failures name the step, the status and the query-trimmed URL.
- **Every unpublished vendor assumption is marked `VENDOR ASSUMPTION` at the line that depends on
  it.** Brave publishes no API for this; the shapes were derived from the open-source client and
  confirmed live once, on 2026-07-30. The markers exist so a future breakage is diagnosable as *the
  vendor moved* rather than *our code is wrong*. One of them is explicitly **inferred rather than
  measured**: the spike only ever saw `ready: true` from focus allocation, so `ready: false` is
  retried on a fixed backoff rather than keyed on a response field this repo has never observed.
- **Own-tenant signing is still deferred** and is the one Acceptance item left: it needs an RS256
  signer this workspace does not carry (`rsa`/`jsonwebtoken`/`ring` are all absent). It is a
  *second* implementation of `JaasTokens` and changes nothing else — which is what the seam is for.
- **The guest path carries no credential at all**, which is why `RoomSettings` has no JWT, API-key or
  private-key field for it: the CSRF handshake exists precisely *because* the endpoint is
  unauthenticated. When own-tenant signing lands it inherits the credential seam every other channel
  setting already uses — `flux_app::resolve_secrets` resolves `secret "KEY"` at load and registers
  the value with the host's `Redactor`.
- **Known gap: the runtime-minted JWT is not registered with the `Redactor`.** `flux-channels` does
  not depend on `flux-secret` (the same constraint `adapters/webhook.rs` documents), and unlike a
  declared secret this token is minted at *runtime*, so `resolve_secrets` never sees it. It is held
  out of logs structurally instead — redacting `Debug`, query-trimmed endpoints, no response bodies
  in errors, `HeaderValue::set_sensitive` on the Bearer — but a tool that echoed it would not be
  scrubbed. Closing this needs `flux-secret` in the manifest and a redactor handed down to the
  channel.

**As landed (D-207)** — the address rule and the reply budget, `crates/flux-channels/src/rooms/`
(`address.rs`, `budget.rs`) applied in `driver.rs`. Three filters stand between an inbound message
and a handler turn, and they are three because each covers a case the others cannot:

- **The rule governs public text; a whisper is always addressed.** Nobody whispers to the agent and
  does not mean it, which is what `MessageScope` was carried through the port for. `address_rule` is
  a comma-separated list of `mention` (default), `wake: <phrase>`, `always` or `never`, and a token
  outside that vocabulary is a **load error** — D-204 carried the field unvalidated because the
  vocabulary was unchosen, and a typo degrading to "answer everything" is the failure the rule exists
  to prevent.
- **Mention matching asks whether we were *addressed*, not whether our name occurred.** Two separate
  points, and both were bugs before they were rules. First, the nick matched is the one **presence
  says the service seated us under**, not `RoomSettings.nick`: a MUC may reassign it on a collision
  and `<status code='110'/>` is what names us afterwards (D-205, above), so matching the configured
  value would make the agent permanently silent — occupants type the name they can see. Second, the
  occurrence must be shaped like an address (`@nick`, a whitespace opening closed by end-of-line or
  `:,?!.;`, or a line-initial vocative), because our name also appears in URLs, log paths, JIDs and
  prose about the product: `see https://flux.dev/docs` is not a question for us. Wake phrases are
  deliberately looser — match-anywhere at word boundaries — since the *operator* chooses those and
  can make them as distinctive as the room requires. None of this identifies a *speaker* by nick,
  which stays `OccupantId`'s job (C-408).
- **A silent refusal explains itself once per session** on stderr, naming the rule and the nick we
  are answering to. Every refusal is silent in the room by design, so this is the operator's only
  window onto "the bot stopped answering"; one line per distinct reason keeps it from becoming the
  spam D-207 removed from the room itself.
- **An unaddressed line is overheard, not dropped and not thought about.** `VoiceTurnHandler` gained
  a defaulted `overheard`, and the room adapter accumulates those lines in an attributed, bounded
  `flux_flow::voice::RoomTranscript` that rides the *next* addressed turn as the payload's `context`.
  Zero deliveries and therefore zero planner calls is the assertion that matters: a
  silent-but-thinking agent still burns spend.
- **The agent-to-agent runaway is bounded twice, on purpose, because only one of the two is
  structural.** A declared `OccupantKind::Agent`'s **plain text** is refused outright at any scope —
  only a structured A2A envelope gets through, recognized in its JSON-RPC 2.0 shape as the D-212
  seam. **That arm is unreachable today on every backend, not just XMPP**: `OccupantKind::Agent` is
  only ever assigned to *ourselves*, so no peer is currently classifiable as an agent. XMPP presence
  carries no human-or-bot signal either, so a real MUC reports `Unknown` for everyone; the rule is
  the shape a backend must grow into (D-212), and the per-room `ReplyBudget` (a sliding window, 12 turns per
  60 s by default) is what holds the case flux cannot see. It gates the **turn**, and an exhausted
  budget is silent — announcing exhaustion is itself a reply, and two agents announcing it at each
  other is the same runaway one layer up.
- **Known gap: this is the *channel* path only.** `run_journey`'s room path still runs as
  `local`/Privileged (C-415), and nothing here widens or narrows that.
- **Known gap: the overheard context does not reach an `agent`-bound turn.** `flux-app`'s `run_agent`
  uses the payload's `text` when non-empty and synthesizes an event context only otherwise, so for an
  addressed room line the rest of the payload — `context` included — is dropped before the model. It
  survives on the **journey** path, which takes the payload whole. The fix is a judgement about how
  every channel's payload should reach an agent turn, not a room-specific patch, so it wants its own
  story.

**The L3 turn seam changed with it (breaking):** `VoiceTurnHandler::turn` is now
`turn(&self, speaker: &Speaker, user_text: &str)`. `flux_flow::voice::Speaker` is a surface-owned id
plus an optional display name; a 1:1 surface passes `Speaker::sole()`, which is how a phone line's
single caller becomes *named* rather than absent.

Backends: **`XmppMucRoom`** (generic prosody/ejabberd MUC — the portable one), **`JaasRoom`** (Brave Talk
and any own-tenant JaaS: the guest-JWT + conference-request handshake above), and **`MockRoom`** for
tests. A room is a new `ChannelDecl` `kind` — `room`, with
`settings { backend, room, nick, address_rule, reply_budget, reply_window_secs }` — so it enters
through `build_channels` and needs no new
host, exactly as D-04 established.

**Every inbound event carries an `OccupantId`.** That is the one change the existing turn seams needed: a
room has N speakers, and before D-204 `VoiceTurnHandler::turn` took only text. Attribution is not a feature,
it is the precondition for the address rule below. The one exception is `Ended` — the room's own
lifecycle terminator, which no participant causes; `RoomEvent::occupant()` returns `None` only there,
and a consumer that requires `Some` for everything else is holding the port to its contract.

## The media seam — `MediaPeer` and `flux.room-media.v1` (D-208, landed)

Media is a **second port beside `Room`**, not more variants on `RoomEvent`. flux is in the room twice when
media is on — natively for text and presence, through a browser sidecar for media — and the two halves
share an address and nothing else. That is what keeps `RoomEvent` browser-free and keeps invariant 6 true
by construction rather than by discipline: a text consumer's `match` never grows a media-shaped arm it
cannot see.

Behind the `room-media` cargo feature (off by default, **no new crate dependency** — the ~200 MB weight it
keeps out of a text-only room is a *runtime* one, a browser on the host) plus an explicit `media { … }`
block in the channel declaration. Declaring `media` on a flux built without the feature is a **load
error** naming the feature, never a silent no-op: the way this breaks in the field is an operator watching
text work and concluding media works too.

```rust
#[async_trait]
pub trait MediaPeer: Send + Sync {
    async fn join(&self, room: &RoomId, identity: &RoomIdentity) -> Result<MediaStream>;
    async fn publish_audio(&self, audio: &AudioChunk) -> Result<()>;
    async fn publish_video(&self, video: &VideoFrame) -> Result<()>;
    async fn mute(&self, muted: bool) -> Result<()>;
    async fn level(&self) -> Result<Level>;                 // the probe invariant 8 rests on
    async fn leave(&self) -> Result<()>;
    async fn verify_audible(&self, floor: f32) -> Result<Level>;  // default: silence is a failure
}
```

The wire is one JSON object per line, over the sidecar's stdin/stdout, in both directions:

```text
flux → sidecar   {"id":1,"cmd":"join","room":"standup@conference.example.org","nick":"flux","kind":"agent"}
                 {"id":2,"cmd":"publish_audio","audio":{"pcm16_le":"<b64>","sample_rate_hz":48000,"channels":1}}
                 {"id":3,"cmd":"publish_video","video":{"rgba":"<b64>","width":1280,"height":720}}
                 {"id":4,"cmd":"mute","muted":true}    {"id":5,"cmd":"level"}    {"id":6,"cmd":"leave"}

sidecar → flux   {"ready":"flux.room-media.v1","owns_device_routing":true}
                 {"id":1,"ok":true}   {"id":5,"ok":true,"level":{"rms":0.12,"peak":0.31}}
                 {"id":2,"ok":false,"error":"no published audio track"}
                 {"event":"audio_frame","from":"…/timo","audio":{…}}
                 {"event":"speech_started","from":"…/timo"}
                 {"event":"participant","occupant":"…/timo","nick":"timo","kind":"human","present":true}
```

Four decisions in it are load-bearing, and each one is a measured finding rather than a preference:

- **The protocol names no capture device, sink, source or audio server.** The recipe that works is
  Linux-specific, so it lives *inside* the sidecar and the port stays portable. Pinned on the rendered
  wire, not in prose: `rooms/media/protocol.rs::the_protocol_never_names_a_capture_device`.
- **What crosses the seam instead is a claim.** `owns_device_routing` in the handshake, **defaulting to
  `false`**. flux refuses to publish audio through a sidecar that has not taken ownership — because both
  ways of *not* owning it (Chrome 150 ignoring `--use-fake-device-for-media-capture`, `setAudioInputDevice`
  not sticking) fail by reporting success.
- **Publication is checked with a level probe, never a mute-state read** (invariant 8, below).
- **Stdout noise is skipped and counted, never fatal.** A browser harness prints. Ending a live call over
  a stray log line is the worse failure; only a line that *claims* to be protocol and is malformed is
  surfaced, and even that only counts.

**Backpressure.** Inbound audio arrives ~50×/s and never stops. `MediaStream` is bounded and sheds *audio*
past capacity rather than growing, keeping `MEDIA_CONTROL_RESERVE` (32) slots back so `speech_started` and
`participant` are never shed — a barge-in that arrives late is a bot talking over a person. Blocking
instead would push backpressure onto the sidecar's pipe and stall the *outbound* half of the same
protocol, so a flux that is slow at hearing would stop being able to speak.

**Failure posture.** Every media failure is an **operation** failure. A sidecar that would not start, died,
or wedged fails the `MediaPeer` call and nothing else: the room stays joined, text and presence keep
flowing, and `RoomChannel::start` still returns `Ok` — which matters because `flux_channels::serve` ends
the whole process on a channel `Err`. There is deliberately **no reconnect**: rejoining a live call is a
decision with a room full of people in it, and it belongs to the session owner, not to a transport.

### Sidecar preflight — the runbook (Linux/PipeWire)

The sidecar is an out-of-tree program; flux only speaks to it. This is what the 2026-07-30 spike had to do
to get audio a human could hear, and it is the checklist to run **before** a call rather than during one.

1. **Give the agent its own capture device, and only its own.**
   ```bash
   pactl load-module module-null-sink sink_name=fluxagent \
       sink_properties=device.description=fluxagent
   pactl load-module module-remap-source source_name=fluxagent_mic \
       master=fluxagent.monitor source_properties=device.description=fluxagent_mic
   ```
2. **Move only our own capture stream onto it, per-stream.** Find the browser's source-output id and
   `pactl move-source-output <id> fluxagent_mic`. **Never** change the default source — the human in the
   same call is using it, and that property is what makes this safe on a machine someone is working on.
3. **Do not rely on Chrome's fake-device flags.** `--use-fake-device-for-media-capture` and
   `--use-file-for-fake-audio-capture` are ignored on Chrome 150; the probe reads peak `0.0004`.
4. **Do not rely on `setAudioInputDevice`.** It reported the right label and `getCurrentDevices()` kept
   saying `Default`.
5. **Announce ownership.** The handshake must say `"owns_device_routing":true`, or flux will refuse to
   publish audio at all. Say it because it is true, not to get past the check.
6. **Probe before you trust it.** Answer `{"cmd":"level"}` with a real in-page RMS measurement of the
   *published* track. Audible measured ≈`0.12`; silence measured `0.0004`; the floor is `0.01`.
7. **Expect a cleared environment — and know that argv alone is not enough.** flux spawns the sidecar
   argv-only, cwd-pinned, with the environment cleared to a minimal allow-list; `DISPLAY`,
   `XDG_RUNTIME_DIR`, `PULSE_SERVER` and friends **do not** reach it, while `HOME`, `USER`, `PATH` and
   `TMPDIR` do. So the audio server rides in argv, which flux never interprets:
   `media { sidecar ["flux-room-media", "--audio-server", "unix:/run/user/1000/pulse/native"] }`.

   ⚠ **Measured 2026-08-02 (D-232): that is necessary but not sufficient.** `bubblewrap_argv` masks
   `/run` with `--tmpfs /run` — deliberately, since that is what keeps `docker.sock` and D-Bus
   unreachable — and the pulse socket lives at `/run/user/<uid>/pulse/native`. Inside the sandbox
   `pactl --server=unix:/run/user/1000/pulse/native info` fails `Connection refused` while succeeding
   outside it, so the path is right and the file is simply not there. The operator must **also** grant
   the socket's directory as a writable sandbox path, which re-exposes it past the mask:

   ```toml
   [sandbox]
   writable = ["/run/user/1000/pulse"]
   ```

   Both halves or no audio, and the argv-only half is the silent-failure trap: the sidecar starts, the
   handshake succeeds, and only the level probe tells you anything is wrong. **No env passthrough was
   added to `flux-system`** and none is needed — the sidecar re-exports `PULSE_SERVER`/`XDG_RUNTIME_DIR`
   into *Chrome's* environment, a child it owns, rather than asking flux for new public API.
8. **The sandbox is fine — Chrome does not need `--no-sandbox`.** ✅ **Measured 2026-08-02 (D-232),
   inside the exact argv `bubblewrap_argv` builds:** `--headless=new --dump-dom` rendered a page and
   exited 0, `unshare -U -r true` succeeded inside the sandbox (so a nested user namespace is creatable
   and Chrome's content sandbox has what it needs), and the full in-page level probe returned the same
   number sandboxed as unsandboxed (`rms 0.3550` for a 0.5-amplitude tone). Chrome prints a wall of
   D-Bus errors because `/run` is masked; they are noise, not failure. So `Confinement::Sandboxed`
   stands, **no new `Confinement::Exempt` seam is needed**, and `FLUX_SANDBOX=off` is not required. The
   sidecar keeps a `--no-sandbox` flag for hosts that refuse namespace nesting, off by default —
   forcing it would trade Chrome's purpose-built sandbox for a weaker generic one, the same trade
   `spawn_debug_pipe` declines.

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
2. **The agent answers only when addressed.** ✅ **Met (D-207).** A replayed transcript of two humans
   talking, N messages, none addressed to the agent → **zero** outbound messages and **zero**
   deliveries, which is where planner spend begins:
   `crates/flux-channels/tests/rooms.rs::unaddressed_room_chatter_stays_silent`.
3. **Reply is bounded.** ✅ **Met (D-207).** `crates/flux-channels/tests/rooms.rs::agent_pair_chatter_converges`
   drives a room whose other occupant answers every line flux says, and asserts the exchange stops
   well short of the double's own runaway cap. The peer's `OccupantKind` is `Unknown` on purpose —
   that is what a real MUC reports (D-205), so the arm under test is the budget rather than the
   agent-plain-text refusal, which is pinned separately in
   `rooms/driver.rs::a_declared_agents_plain_text_is_never_a_turn`.
4. **No unredacted publish.** A pane whose source text contains a credential shape publishes redacted
   (C-216 corpus, run against the render path).
5. **Self-announcement is not optional.** Join emits an identifying room message *before* the first inbound
   message is read.
6. **Text needs no browser.** ✅ **Met (D-205).** `crates/flux-channels/tests/xmpp_room.rs` drives the
   whole join → occupants → say → read → leave path against an in-process WebSocket double with no
   browser, no vendor SDK and no network; the media sidecar is feature-gated and skipped. The two
   RFC 7395 traps are regressed on the raw bytes:
   `every_stanza_the_xmpp_backend_emits_is_jabber_client_qualified` and
   `the_xmpp_keepalive_is_a_ping_iq_and_never_whitespace`. **Still met after D-208**, and now with a
   second guard: the media sidecar is behind an off-by-default cargo feature, so `cargo test --workspace`
   compiles none of it, and `crates/flux-channels/tests/rooms.rs::room_text_works_without_media_sidecar`
   asserts both halves — the full text+presence path runs with no browser and nothing spawned, *and* a
   `media` block declared on a build without the feature is refused by name rather than dropped on the
   floor. `scripts/check-feature-gated-tests.sh` is what stops the feature's own suite from rotting
   unexercised.
7. **Token refresh is transparent.** ✅ **Met (D-206), and the double it rests on now models the case
   that would have made it false.** `crates/flux-channels/tests/jaas_room.rs::jaas_session_survives_token_expiry`
   drives a 3-second TTL against a fake token service and the in-process XMPP double, asserting the
   re-join carried a *different* token, that a message said afterwards still lands, and that the
   consumer saw neither `Ended` nor a duplicate `Joined`. The claim is only worth as much as the
   double: an earlier version had no occupancy model, answered every MUC presence identically, and so
   would have passed a refresh that overlapped its sessions — which a real service refuses with
   `<conflict/>` (XEP-0045 §7.2.9), leaving the session dead against the vendor while the suite
   stayed green. The double now tracks nick ownership per connection and refuses a held nick;
   `tests/xmpp_room.rs::a_second_session_cannot_take_a_nick_the_first_still_holds` pins that arm so
   it cannot rot back into a permissive one.
8. **Published media carries signal.** A published track whose source is silence is reported as a failure,
   not as success — asserted with a level probe, because the spike watched the bridge elect a silent bot
   dominant speaker. ⚠ **Partly met (D-208), and the unmet half is the one that matters most.** The
   *enforcement* is in and tested: `MediaPeer::verify_audible` refuses anything at or below `0.01` RMS
   (and refuses an unmeasurable `NaN`, and refuses a `level` reply carrying no measurement at all), a
   sidecar that does not claim `owns_device_routing` may not publish audio, and both arms are driven over
   the real wire in `crates/flux-channels/tests/room_media.rs` —
   `a_published_track_that_carries_silence_fails_the_probe` scripts exactly the spike's shape: publish
   returns `ok`, mute reads `false`, and the probe reads `0.0004`.

   **D-232 closed the measurement half, and the last gap is now a room rather than a number.** The
   shipped harness (`crates/flux-channels/assets/room-media/`) measures a **real `MediaStreamTrack`**:
   `page.js` builds the outbound track as `AudioContext → MediaStreamDestination` and probes it by
   re-wrapping *the published track* as a fresh `MediaStreamSource` into an `AnalyserNode` on a second
   context, so the measurement path shares nothing with the publish path but the track itself. Against
   real Chrome 150 that reads `rms 0.3550` for a 0.5-amplitude tone (analytic `0.3536`) and `0.0000` for
   silence — including inside flux's bubblewrap policy. Two properties make this checkable rather than
   claimed: the probe was observed **lagging one chunk behind** the amplitude just pushed (a probe
   echoing its input would track instantly), and the arithmetic lives in `measure.js`, loaded by *both*
   the page and `tests/room_media_harness.rs`, whose analytic table (`a/√2` for amplitudes
   `1.0…0.0`) no constant-returning probe can satisfy.

   ⚠ **Still not met: the room.** Nothing has joined a live call, so `page.js::join` — the
   `lib-jitsi-meet` connection, the `JitsiLocalTrack` wrap of a synthesized stream, and whether the
   bridge accepts it — is code-reviewed and unexercised, and **no human has confirmed hearing audio**.
   Every browser-dependent test is `#[ignore]`d (CI has no browser and no network) with its by-hand
   command recorded. Same for invariant 5 (self-announcement), which the media plane still does not do.

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
