---
id: D-207
title: Addressing and reply budget — the agent answers only when spoken to
pillar: Agent
status: in-progress
priority: 5
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels, flux-flow]
note: "the real design problem, not the plumbing: N speakers, and the agent is the addressee of almost none of it; without an address rule it answers every sentence two humans say to each other, and two agents ping-pong forever"
---

# Addressing and reply budget — the agent answers only when spoken to

## Goal

Make a room agent socially correct: it hears every turn, attributes each to a speaker, accumulates
context silently, and **speaks only when addressed** — and it can never run away in agent-to-agent
chatter. This is what separates a usable meeting participant from a bot that talks over everyone.

## Acceptance

- [x] An `address_rule` on the room channel settings: nick mention, private whisper, or a configured wake
      phrase. Unaddressed turns update context and produce **no** output.
- [x] Failing-first test `unaddressed_room_chatter_stays_silent`: a replayed transcript of two humans
      talking (N messages, none addressed to the agent) yields **zero** outbound messages **and zero
      planner calls** — the planner-call count is the assertion that matters, since a silent-but-thinking
      agent still burns spend.
- [x] A per-room reply budget per unit time. Failing-first test `agent_pair_chatter_converges`: two mock
      agents that both reply to mentions terminate instead of running to the agent cap.
- [x] An agent never auto-replies to another agent's **plain text** — only to a structured A2A envelope
      (the D-212 seam). — **with the caveat that this arm is unreachable today on every backend**:
      `OccupantKind::Agent` is only ever assigned to *ourselves*, so no peer is currently classifiable
      as an agent and the refusal cannot fire for one. The rule is correct and pinned, and the reply
      budget is what actually bounds agent-to-agent chatter until a backend can tell peers apart
      (D-205 for XMPP, D-212 for the declared case).
- [x] Attributed context: the accumulated transcript records who said what, so an eventual answer can
      refer to "what Timo asked" rather than a flat blob. — **with the caveat that it reaches the
      model on the journey path only**: `flux-app`'s `run_agent` collapses the payload to its `text`
      field, so an `agent`-bound room drops the `context` before the turn. See the Progress note.

## Progress

Landed in `crates/flux-channels/src/rooms/{address,budget}.rs`, applied in that module's `driver.rs`,
with the attributed half in `crates/flux-flow/src/voice/room_transcript.rs`.

- **`AddressRule`** (`address.rs`) — a comma-separated list of `mention` (default), `wake: <phrase>`,
  `always` or `never`. It governs **public** text only: a private message is addressed by
  construction, which is what `MessageScope` was carried through the port for. Mention matching is
  boundary-checked against **our own** nick (an "influx of tickets" is not an address); it never
  identifies a *speaker* by nick, which stays `OccupantId`'s job. A token outside the vocabulary is a
  **load error** — D-204 carried the field unvalidated because the vocabulary was unchosen, and a
  typo degrading to "answer everything" is the failure the rule exists to prevent.
- **`ReplyBudget`** (`budget.rs`) — a per-room sliding window, 12 turns per 60 s by default, tunable
  via `reply_budget` / `reply_window_secs`. It gates the **turn**, not the outbound line, and an
  exhausted budget is silent: announcing exhaustion is itself a reply, and two agents announcing it
  at each other is the same runaway one layer up. A refused turn consumes nothing, so a quiet room
  recovers on the window alone.
- **Two bounds on agent-to-agent chatter, deliberately, because only one is structural.** A declared
  `OccupantKind::Agent`'s plain text is refused at any scope — only a structured A2A envelope gets
  through, recognized in its JSON-RPC 2.0 shape (the D-212 seam). But XMPP presence carries no
  human-or-bot signal, so a real MUC reports `Unknown` for everyone and that arm never fires there;
  the budget is what holds the case flux cannot see. `agent_pair_chatter_converges` drives the
  `Unknown` arm on purpose, so it tests the bound that actually applies in production.
- **Attributed context** — `VoiceTurnHandler` gained a defaulted `overheard`; the room adapter
  accumulates unaddressed lines in a bounded `RoomTranscript` and drains them onto the *next*
  addressed delivery as the payload's `context` (`{speaker, nick, text}` per line).
- **Addressing follows the nick the *service* gave us**, not the configured one. A MUC may seat us
  under a different nick on a collision, and `<status code='110'/>` is what names us afterwards
  (D-205). The driver tracks our room-visible nick from self-presence — which necessarily precedes
  our first message, the same ordering the echo check relies on — and passes that to `classify`.
  Matching `RoomSettings.nick` after a reassignment made the agent **permanently silent**: occupants
  type the name they can see. Pinned by `a_reassigned_nick_is_the_one_the_room_must_type`.
- **A mention has to be shaped like an address, not merely contain the nick.** Our name turns up in
  URLs, log paths, JIDs and prose about the product, so `addresses_by_name` requires `@nick`, or a
  whitespace/line opening closed by end-of-line or `:,?!.;`, or a line-initial vocative. Pinned in
  both directions (`our_nick_merely_occurring_in_a_line_is_not_an_address` and
  `the_spellings_people_actually_use_to_address_a_bot_all_land`). Wake phrases stay match-anywhere on
  purpose and say so: the operator picks those and can make them as distinctive as they like.
- **Every silent refusal explains itself once per session** on stderr (the crate's logging
  convention — `flux-channels` carries no `tracing` dependency, and adding one is a fenced change).
  One line per distinct reason, so a busy room cannot turn the log into the spam D-207 removed from
  the room itself, and it names the nick we are answering to — the value most often at fault.

**Read before changing:** the "zero planner calls" assertion is spelled as **zero `Deliverer::deliver`
calls**. That is the seam where a room message becomes a journey run and therefore spend, and it is
reachable from `flux-channels`; counting model calls would mean reaching into `flux-app`'s
provider. Anyone tightening this should tighten it there, not weaken it here.

**Known gap, not fixed here — the context does not reach an `agent`-bound room turn.**
`flux-app`'s `run_agent` (`crates/flux-app/src/app.rs:1586-1589`) uses the payload's `text` when it is
non-empty and synthesizes an event context only otherwise; for an addressed room line the text is
always non-empty, so the whole payload — `context` included — is dropped before the model. It
survives on the **journey** path, which takes the payload whole. Acceptance item 5's stated purpose is
therefore unreachable for a room bound to an agent. `flux-app` is outside this story's
`areas: [flux-channels, flux-flow]`, and the fix is a judgement about how *every* channel's payload
should reach an agent turn — not a room-specific patch — so it needs its own story rather than a
drive-by widening here.

**Not in scope, and still open:** the *journey* path (`run_journey`) is untouched — it remains
`local`/Privileged (C-415), and no rule here widens or narrows it.

## Notes
- The existing seam has no speaker at all: `VoiceTurnHandler::turn(&self, user_text: &str)`. D-204 adds
  the `OccupantId`; this story is what makes it mean something.
- Live evidence that this is needed: in the 2026-07-30 spike the bot replied to *every* inbound line,
  which read as spam within three messages.
