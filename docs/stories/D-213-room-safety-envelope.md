---
id: D-213
title: The room safety envelope — untrusted co-presence, consent, and approved publication
pillar: Agent
status: ready
priority: 32
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels, flux-policy]
note: "a room is untrusted MULTI-party input and anyone with the link can put a client in it — proven by the spike itself, which joined with no account; this story owns the invariants the rest of the epic must not break"
---

# The room safety envelope — untrusted co-presence, consent, and approved publication

## Goal

Establish and *test* the safety posture for rooms: co-presence grants no authority, the agent's presence
is disclosed, and anything the agent publishes into a room full of people is an approved, redacted act.
This story owns the invariants; the other stories must not break them.

## Acceptance

- [ ] **Joining grants no authority.** Failing-first test `room_message_cannot_escalate`: an op requiring
      approval, triggered by a room message, is **denied** absent approval — asserting denial, not an
      approval prompt rendered into the room (a room participant must not be able to summon an approval
      surface).
- [ ] **Self-announcement is not optional.** Test `room_join_announces_agent`: joining emits an
      identifying message *before* the first inbound message is read.
- [ ] **Consent for transcription.** The vendor token carries `transcription: false`, which does not
      govern flux transcribing locally; the agent discloses that it is listening, and a room transcript is
      evidence-stamped like any other artifact.
- [ ] **Publication is approved.** Joining, publishing audio, and publishing video are three distinct
      approvable acts, not implicit capabilities.
- [ ] **Redaction on every outbound surface** — text, spoken audio, and screenshare (C-215/C-216 corpus).
- [ ] The threat is documented where operators will read it: **anyone holding a room link can put an agent
      or a listener in the room without an account**, so a room link is a credential and should be treated
      as one.

## Progress
- (not started)

## Notes
- The spike is the evidence for the threat model: `PUT /api/v1/rooms/<room>` returned a valid 3 h
  room-scoped token to an anonymous `curl`. Nothing about that is a flux defect — it is the environment
  flux must assume.
- Pairs with the existing fail-closed doctrine: absent an approver, a room agent should refuse to publish
  rather than publish unapproved.
