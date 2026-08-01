---
id: C-416
title: "Identity is sniffed from payload field names, so Slack threads get none — an adapter should declare its principal and how well it is authenticated"
pillar: Core
status: ready
priority: 7
epic: connector-channels
areas: [flux-app, flux-channels, flux-runtime]
note: "raised by the owner: a Slack thread is a room scenario. C-407's fence already covers it (it names no field); C-408's identity does not, because `room_participant_identity` sniffs for `speaker`+`room`. The deeper point is that a Slack `user` is authenticated BY SLACK while a MUC `speaker` is self-asserted, and the current design cannot express that difference"
---

# The adapter knows who the principal is; the payload sniff does not

## Goal

Let a channel adapter **declare** the principal behind a delivery and how well that principal is
authenticated, instead of having downstream code guess from payload field names.

## Why the current shape does not generalize

[C-408](C-408-room-participants-share-one-privileged-identity.md) gave the room path a request-owned
`TurnIdentity` by deriving one from the payload when it carries **both** `speaker` and `room`. The
narrowness is deliberate and its comment says so — it stops a future surface that happens to name a
`speaker` from silently inheriting a room attribution.

The consequence is that **every other multi-principal surface still runs as `local`/`Privileged`**:

| adapter | payload principal | conversation key | allow-list | identity today |
|---|---|---|---|---|
| `room` | `speaker` (+ `room`) | ⚠ none — a fresh session per message | none | ✅ since C-408 |
| `slack` | `user` (+ `channel`, `thread`) | ✅ `conversation` = thread ts, else channel | ✅ `allow_users` / `allow_channels` | ❌ `local`/Privileged |
| `webhook` | whatever the body says | — | — | ❌ `local`/Privileged |
| `connector` | whatever the body says | — | — | ❌ `local`/Privileged |

Note Slack is **ahead** of rooms on two axes (it has a durable conversation key and an allow-list)
and behind on the one this story is about.

⚠ **What already generalized, and why:** C-407's untrusted-payload fence lives in `event_context`
and names no field, so it covers Slack, webhook and connector deliveries today with no change. That
is the model to copy — the fix was written against the shape of the problem, not against one adapter.

## The distinction the current design cannot express

**A Slack `user` is authenticated by Slack**, over an authenticated socket-mode connection. **A MUC
`speaker` is self-asserted** — anyone can join a guest room and choose an id. **A webhook body's
field is authenticated by nothing at all.**

These deserve different trust, and a payload-shape sniff cannot tell them apart: it sees three
strings. Only the adapter knows the provenance of the id it emits.

## Acceptance

- [ ] **Failing-first**: a test asserting two different Slack users in one thread produce two
      different caller identities — failing at the merge base, where both read `local`.
- [ ] A channel adapter declares its principal and that principal's **provenance** — at minimum
      "authenticated by the vendor" vs "self-asserted by the sender" vs "unauthenticated". The
      payload-shape sniff in `room_participant_identity` is replaced by, or delegates to, that
      declaration.
- [ ] The trust level follows from the declared provenance rather than being one constant. C-408
      chose `TrustLevel::Untrusted` for a self-asserted room id and that stays correct; a
      vendor-authenticated id is the case this story exists to let us rate differently. ⚠ **Do not
      raise any existing surface's trust as a side effect** — a change that makes something more
      authoritative than it is today must be argued on its own.
- [ ] `webhook` and `connector` are covered or explicitly excluded with the reason at the call site.
      They build payloads from untrusted request bodies and are the weakest case, so "declares
      nothing, therefore assembly-time identity" must be a stated answer rather than a gap.
- [ ] Reuses `TurnIdentity::unauthenticated_participant` (`crates/flux-runtime/src/lib.rs:739`) for
      the self-asserted case rather than adding a second constructor for the same meaning.
- [ ] Full gate green.

## Notes

- ⚠ **Ordering.** [C-415](C-415-a-room-triggered-journey-still-runs-as-the-operator.md) closes the
  journey half of the room path. This story generalizes across adapters. Doing this one first would
  mean generalizing a mechanism that is still missing on half its own path — prefer C-415 first, or
  say why not.
- The Slack payload is built at `crates/flux-channels/src/adapters/slack.rs:172-181`; the room's at
  `crates/flux-channels/src/adapters/room.rs:151-157`. Both reach the app through the same
  `Deliverer::deliver(label, payload)` seam, which is exactly why the identity question is common to
  them and the answer currently is not.
- Slack's allow-list (`allowed`, `slack.rs:184`) bounds **who** may wake the agent. It does not bound
  **as whom** they run — those are different controls and this story is the second one.
- 1:1 DMs are not a special case: the other party still is not the operator. A DM is a
  two-principal surface, not a single-principal one.

## Progress

- Filed 2026-08-01, from the owner's observation that a Slack thread is structurally a room. Verified
  against the tree before filing: C-407's fence is field-agnostic and already covers Slack;
  C-408's derivation is not and does not.
