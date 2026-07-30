---
id: D-207
title: Addressing and reply budget — the agent answers only when spoken to
pillar: Agent
status: backlog
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

- [ ] An `address_rule` on the room channel settings: nick mention, private whisper, or a configured wake
      phrase. Unaddressed turns update context and produce **no** output.
- [ ] Failing-first test `unaddressed_room_chatter_stays_silent`: a replayed transcript of two humans
      talking (N messages, none addressed to the agent) yields **zero** outbound messages **and zero
      planner calls** — the planner-call count is the assertion that matters, since a silent-but-thinking
      agent still burns spend.
- [ ] A per-room reply budget per unit time. Failing-first test `agent_pair_chatter_converges`: two mock
      agents that both reply to mentions terminate instead of running to the agent cap.
- [ ] An agent never auto-replies to another agent's **plain text** — only to a structured A2A envelope
      (the D-212 seam).
- [ ] Attributed context: the accumulated transcript records who said what, so an eventual answer can
      refer to "what Timo asked" rather than a flat blob.

## Progress
- (not started)

## Notes
- The existing seam has no speaker at all: `VoiceTurnHandler::turn(&self, user_text: &str)`. D-204 adds
  the `OccupantId`; this story is what makes it mean something.
- Live evidence that this is needed: in the 2026-07-30 spike the bot replied to *every* inbound line,
  which read as spam within three messages.
