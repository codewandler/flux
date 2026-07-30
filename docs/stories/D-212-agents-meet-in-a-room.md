---
id: D-212
title: Agents meet in a room — the room as an A2A transport with humans present
pillar: Agent
status: backlog
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels, flux-a2a]
note: "the framing that motivated the epic: a room as the substrate where a fleet convenes, humans watching the same conversation; structured A2A envelopes ride the room while humans see plain text"
---

# Agents meet in a room — the room as an A2A transport with humans present

## Goal

Let two or more flux agents be co-present in one room and coordinate there, with humans in the same room
seeing it happen. The room becomes a *meeting point* for a fleet — an addressable place rather than a
point-to-point connection — which is the property no existing flux transport has.

## Acceptance

- [ ] A room is reachable as an `AgentAddress`, so fleet dispatch (A-111 / A-119 / A-120) can target
      "whoever is in this room" instead of a fixed endpoint.
- [ ] Agent-to-agent traffic travels as a **structured A2A envelope** distinguishable from human text;
      humans still see a legible plain-text rendering of what was exchanged (a room where agents talk in
      opaque blobs defeats the point of humans being present).
- [ ] Failing-first test `two_agents_exchange_a2a_in_one_room`: two in-process agents join a `MockRoom`,
      exchange a task envelope, and a human-visible summary line is emitted for each exchange.
- [ ] The D-207 reply budget and the no-auto-reply-to-plain-agent-text rule hold: adding a second agent
      cannot produce an unbounded exchange.
- [ ] Occupant identity distinguishes agent from human, and an agent cannot claim to be a human.

## Progress
- (not started)

## Notes
- Brave's free tier caps a room at 4 participants and our guest token carried
  `x-brave-features.group-room: "false"` — so multi-agent rooms want an own JaaS tenant (D-206) or a
  self-hosted XMPP MUC (D-205), where there is no such cap.
- A-112 (per-delivery bus isolation) matters more here than anywhere else in the epic: several agents in
  one room means genuinely concurrent deliveries.
