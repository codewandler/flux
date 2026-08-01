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
      (the D-212 seam).
- [x] Attributed context: the accumulated transcript records who said what, so an eventual answer can
      refer to "what Timo asked" rather than a flat blob.

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
  addressed delivery as the payload's `context` (`{speaker, nick, text}` per line). Inside C-407's
  fence, which renders the whole payload, so nothing about that framing is unpicked.

**Read before changing:** the "zero planner calls" assertion is spelled as **zero `Deliverer::deliver`
calls**. That is the seam where a room message becomes a journey run and therefore spend, and it is
reachable from `flux-channels`; counting model calls would mean reaching into `flux-app`'s
provider. Anyone tightening this should tighten it there, not weaken it here.

**Not in scope, and still open:** the *journey* path (`run_journey`) is untouched — it remains
`local`/Privileged (C-415), and no rule here widens or narrows it.

## Notes
- The existing seam has no speaker at all: `VoiceTurnHandler::turn(&self, user_text: &str)`. D-204 adds
  the `OccupantId`; this story is what makes it mean something.
- Live evidence that this is needed: in the 2026-07-30 spike the bot replied to *every* inbound line,
  which read as spam within three messages.
