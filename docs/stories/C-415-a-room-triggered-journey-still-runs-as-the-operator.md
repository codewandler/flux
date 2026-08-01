---
id: C-415
title: "A room-triggered journey still authorizes and audits as the local operator"
pillar: Core
status: in-progress
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

- [x] **Failing-first**: a test asserting a room-triggered journey's op dispatches authorize and
      audit as the **speaker**, not `local` — failing at the merge base.
- [x] ⚠ **The `spawn` path is decided explicitly and separately.** A payload the model authored must
      not be able to name a principal that buys authority. State the rule at the call site: either
      spawn-sourced deliveries never derive an identity, or they derive one that is provably no
      stronger than the turn that spawned them. "Same code path, so same behaviour" is not an answer.
- [x] A journey has no turn gate and no `turn.identity` observation, so say **where** the attribution
      is recorded instead — the story is only met if an operator can tell after the fact who caused a
      journey's effects.
- [x] The trust level matches C-408's decision (`TrustLevel::Untrusted`) and reuses
      `TurnIdentity::unauthenticated_participant` rather than deriving a second answer.
- [x] Full gate green.

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
- **2026-08-01 — implemented** (`crates/flux-app`). `run_journey` now takes a `JourneyOrigin`, and
  the whole rule lives on that enum's two variants so it is readable at both call sites rather than
  inferred from a `payload` that looks identical in each.
  - **`JourneyOrigin::Delivery`** (from `run_triggers`) derives the identity on exactly C-408's
    terms: `room_participant_identity`'s `room` + `speaker` shape, through the one
    `TurnIdentity::unauthenticated_participant` constructor, at `Untrusted`. No second constructor,
    no second trust decision.
  - **`JourneyOrigin::Spawn`** (from `run_journey_for_spawn`) **never derives one.** A `spawn`
    payload is an ordinary flow value, model-chosen on an agent-driven path — a strictly worse
    forgeability profile than a bus payload, because it is authored inside the turn that would
    benefit. What the child gets instead is what it already inherits: `runtime_turn_context()` reads
    the active lexical scope first and `SpawnOp::execute` is awaited inline inside the spawning
    turn's `scope_runtime_turn` (no `tokio::spawn`, no task-local boundary), so the child's snapshot
    IS the parent's. "No stronger than the turn that spawned it" is therefore a property of the
    scope's nesting, not a check that could be forgotten — to widen its own authority a spawned
    journey would have to install an identity, and that variant installs none.
  - **Where the attribution is recorded:** `JOURNEY_AUDIT_STREAM` (`"journey-audit"`), public from
    `flux-app`. One `journey.identity` observation per journey run in the App's `EventStore` —
    journey, run session, caller/source/trust, plus an `attribution` saying how the principal was
    obtained (`delivery` / `inherited` / `resumed` / `assembly`). Read back with
    `EventStore::observations(JOURNEY_AUDIT_STREAM)`. It is a shared stream because a journey run
    session (`<name>#<n>`) is not a registered store session and so never appears in
    `EventStore::list` — an operator would have no way to enumerate per-run streams to read. Written
    through `flux_core::redact_json_total`, the same total scrub the engine's evidence flush uses,
    and recorded *before* the drive so a run that dies or parks forever is still attributed.
  - **Also closed: the park.** `ParkedAsk` now carries the run's identity, so a continuation
    authorizes as the principal the run *started* as. Both alternatives were wrong: falling back to
    assembly time would let a stranger's journey finish as the operator merely because it asked a
    question, and adopting the *replier's* speaker would be the outer-surface swap of a live turn's
    caller that the identity invariant forbids outright.
  - Tests (flux-app `tests/integration.rs`, all four failing at the merge base):
    `a_room_triggered_journeys_op_authorizes_and_audits_as_the_speaker`,
    `a_journey_for_an_event_that_names_no_principal_keeps_the_assembly_time_identity`,
    `a_parked_room_journey_resumes_as_the_speaker_that_started_it`,
    `a_spawned_journey_inherits_the_spawning_turn_and_cannot_be_told_who_it_is`. They assert on
    `tool_call.caller` — the record the dispatcher itself writes before calling `execute` — so what
    is pinned is what the safety envelope authorized under, not a restatement of it.
- **The `session_for` note above: deliberately LEFT, and the story does not depend on it.**
  `run_journey` never calls `session_for` — it mints its own `<name>#<n>` run session — so the
  fresh-session-per-room-message shape is a `run_agent` property untouched here. Leaving it is also
  the safer side: a fresh session per message means no two speakers ever share one, so nothing can
  undermine the identity this story installs. Bounding it would mean inventing a room conversation
  key in the room adapter (flux-channels), which is what would *create* the two-speakers-one-session
  shape the note warns about. It wants its own story, with the adapter in scope.
- **Known residual, unchanged from C-408 and deliberately not re-answered here.**
  `JourneyOrigin::Delivery` covers everything routed through the bus — an adapter's external event
  *and* a journey's own `emit` cascade — so a model that can call `emit` can present a `room` +
  `speaker` shape to a journey trigger exactly as it already can to an agent trigger. That residual
  is analyzed in one place, on `room_participant_identity`, including why the fix is out-of-band
  identity rather than a narrower payload predicate; giving the journey path a second, divergent
  answer would be worse than sharing the documented one. `spawn` is closed here because it is a
  *direct* re-entry that never crosses the bus, so closing it costs nothing and diverges nothing.
