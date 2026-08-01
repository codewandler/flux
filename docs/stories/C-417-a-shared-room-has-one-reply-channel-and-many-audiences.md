---
id: C-417
title: "A shared conversation has one reply channel and several audiences, so authorizing the asker is not enough"
pillar: Agent
status: ready
priority: 8
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-app, flux-channels, flux-policy, flux-runtime]
note: "raised by the owner: a room holds an operator AND an audience. D-213 sets the floor (co-presence grants no authority) and C-408/C-416 give per-speaker identity — but every authorization decision in flux is a function of the ASKER, and a room reply is broadcast to everyone present. So an operator cannot ask anything in a room that the audience may not see"
---

# The asker is not the audience

## Goal

State, and then enforce, the rule for a conversation that contains **more than one kind of person** —
an operator and an audience — where the agent has exactly **one** reply channel.

## The two halves, and why only one of them is owned

**Half one: asymmetric authority.** [D-213](D-213-room-safety-envelope.md) establishes the correct
floor — *joining grants no authority* — and this story must not weaken it. But the floor makes
everyone in the room equally powerless, **including the operator**. There is no way to say "this
speaker is the operator, and their ask carries the weight it would carry at the terminal".
[C-408](C-408-room-participants-share-one-privileged-identity.md) and
[C-416](C-416-a-channel-adapter-should-declare-its-principal.md) supply the *mechanism* (a
request-owned identity, and provenance-based trust). Nobody has decided the *policy*: may an operator
present in a shared room exercise operator authority there at all?

**Half two, which nothing owns: the audience of the answer is not the asker.** Every authorization
decision in flux is a function of the **caller**. In a room, the *reply* goes to everyone present. So
even a perfectly authorized ask produces an answer that is broadcast — and "the operator asked" does
not make it safe for the audience to read.

⚠ **Redaction does not solve this, and it is important not to mistake one for the other.** D-213's
"redaction on every outbound surface" is **content-based**: it removes registered secrets regardless
of who is listening. This is **audience-based**: the same sentence may be fine for the operator and
wrong in front of customers, with no secret in it anywhere. A deployment summary, a cost figure, an
internal ticket title, another customer's name — none of those are secrets, and all of them are
disclosures.

The practical consequence today: **there is no safe way for an operator to ask a sensitive question in
a room**, other than not asking. That is a usable answer, but it should be a stated one rather than an
accident of the design.

## Acceptance

- [ ] The rule is **written down before it is coded**, in `docs/designs/meeting-rooms.md`: what an
      operator may do in a shared conversation, what an audience member may do, and what happens to a
      reply whose content is authorized for the asker but not for the audience.
- [ ] **Failing-first**: a test in which an operator-identified speaker asks something an audience
      member may not see, asserting the outcome the rule chooses — failing at the merge base, where
      the reply is broadcast unconditionally.
- [ ] The chosen outcome is one of: refuse in a shared context and say so; answer on a private
      channel (DM/thread) rather than the shared one; or answer publicly having stated the audience.
      ⚠ **Silently answering the room is the outcome this story exists to remove.** Whichever is
      chosen, the reason lives at the definition.
- [ ] D-213's floor is **not weakened**: co-presence still grants no authority, and no path added here
      lets an audience member summon an approval surface or inherit an operator's grant.
- [ ] The rule is expressed so it holds for **any** shared conversation, not just XMPP rooms — a Slack
      channel or thread is the same shape (see C-416). An answer that only works for `room` deliveries
      fails this item.
- [ ] Full gate green.

## Notes

- ⚠ **Ordering.** This is the *policy* story; C-408 landed the mechanism for rooms, C-415 completes it
  for room-triggered journeys, and C-416 generalizes provenance across adapters. Doing this first
  would mean writing a rule with nothing to key it on. Prefer C-415 and C-416 first, or argue
  otherwise.
- D-207 (addressing and reply budget) decides **when** the agent speaks. This decides **what it may
  say and to whom**. They meet at the same outbound seam and should not grow two separate answers.
- The identity floor C-408 chose (`TrustLevel::Untrusted` for a self-asserted room id) is what makes
  half one non-trivial: an operator in a guest room is *also* self-asserted unless something
  out-of-band identifies them. C-416's provenance question is the same question from the other side —
  a Slack `user` is vendor-authenticated, a MUC `speaker` is not.
- 1:1 DMs are not exempt: the other party still is not the operator. What changes is the size of the
  audience, not its existence.

## Progress

- Filed 2026-08-01 from the owner's observation that a room contains an operator *and* an audience,
  and that this should bound what an agent will disclose or do depending on who asks. Checked against
  D-213 and D-207 before filing: D-213 owns the floor and content redaction, D-207 owns when to
  speak; neither owns audience-scoped disclosure.
