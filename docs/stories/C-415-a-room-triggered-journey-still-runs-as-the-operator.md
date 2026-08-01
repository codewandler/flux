---
id: C-415
title: "A room-triggered journey still authorizes and audits as the local operator"
pillar: Core
status: ready
priority: 7
epic: meeting-rooms
areas: [flux-app, flux-runtime]
note: "the other half of the review's F2. C-408 gave the AGENT path a request-owned identity; `run_journey` builds its context with no `.with_identity(…)`, so a room-triggered journey still runs as `local`/Privileged. Filed separately because `run_journey` is ALSO reached from `run_journey_for_spawn` — a model-chosen payload — which the agent path is not"
---

# The journey half of the room identity gap

## Goal

Give a room-triggered **journey** the same request-owned identity C-408 gave the agent path, or
state at the call site why it cannot have one.

[C-408](C-408-room-participants-share-one-privileged-identity.md) closed F2 for the agent path:
`run_turn_as` now carries a `TurnIdentity` derived from `speaker`, at `TrustLevel::Untrusted`.
⚠ **It did not close F2.** `run_journey` builds its `RuntimeTurnContext` from
`executor.context().runtime_turn_context().with_session(&session_id)`
(`crates/flux-app/src/app.rs:1216-1220`) with **no `.with_identity(…)`**, so with no outer scope
active `Executor::effective_identity` (`crates/flux-runtime/src/lib.rs:3647-3651`) falls back to the
assembly-time `local`/`Privileged`.

This is not a hypothetical path. `docs/designs/meeting-rooms.md:10` says a room event wakes "a
journey **or** an agent", and flux-channels' own room integration test drives exactly the journey
shape — it asserts on `r.journey == "clock"` (`crates/flux-channels/tests/rooms.rs:412-433`).

## Why this is a separate story, and the reason that actually matters

C-408's stated reason — "it changes authorization inputs for *every* delivery" — is weaker than it
looks: gating `run_journey` on the same `room_participant_identity(payload).is_some()` would scope it
identically to the agent path.

⚠ **The load-bearing difference is the second caller.** `run_journey` is also reached from
`run_journey_for_spawn` (`crates/flux-app/src/app.rs:1613`) — i.e. from a **model-chosen `spawn`
payload**. The agent path has no such caller. So deriving a principal from the payload here means
deriving it from something the model authored, which is a different forgeability profile and needs
its own failing-first test rather than an extension of C-408's.

## Acceptance

- [ ] **Failing-first**: a test asserting a room-triggered journey's op dispatches authorize and
      audit as the **speaker**, not `local` — failing at the merge base.
- [ ] ⚠ **The `spawn` path is decided explicitly and separately.** A payload the model authored must
      not be able to name a principal that buys authority. State the rule at the call site: either
      spawn-sourced deliveries never derive an identity, or they derive one that is provably no
      stronger than the turn that spawned them. "Same code path, so same behaviour" is not an answer.
- [ ] A journey has no turn gate and no `turn.identity` observation, so say **where** the attribution
      is recorded instead — the story is only met if an operator can tell after the fact who caused a
      journey's effects.
- [ ] The trust level matches C-408's decision (`TrustLevel::Untrusted`) and reuses
      `TurnIdentity::unauthenticated_participant` rather than deriving a second answer.
- [ ] Full gate green.

## Notes

- ⚠ Related shape on the same path, found by C-408's review and not fixed there: a room payload
  carries no `conversation`/`thread` key, so `session_for` (`app.rs:1566-1570`) mints a **fresh
  session per room message**. If a room ever gains a conversation key, two speakers would share one
  session under two identities — a different shape of the same problem. Decide whether this story
  should bound that or leave it.
- `TurnIdentity::unauthenticated_participant` (`crates/flux-runtime/src/lib.rs:739`) already exists
  and assigns `TrustLevel::Untrusted` in one place with the reasoning beside it. Do not add a second
  constructor.
- C-408's review also noted the same constructor is now available to the surfaces F3 names
  (`webhook`, `connector`), which build payloads from untrusted request bodies and likewise run at
  the assembly-time identity. Out of scope here; worth a story of its own.

## Progress

- Filed 2026-08-01 at C-408's integration, on its reviewer's explicit finding that C-408 closes the
  story but not the review's F2.
