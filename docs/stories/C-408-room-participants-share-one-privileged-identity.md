---
id: C-408
title: "Every room participant shares one `local`/Privileged identity, so a stranger is attributed to the operator"
pillar: Core
status: in-progress
priority: 6
epic: meeting-rooms
areas: [flux-app, flux-flow, flux-channels]
note: "F2 of the 2026-08-01 security-posture review at 0.47.1. AGENTS.md states multi-principal surfaces pass a request-owned TurnIdentity; the room path uses plain `run_turn`, so every occupant's text is attributed to `local` at Privileged trust in the evidence record. Inert today only because no app-path grant keys on principal or trust"
---

# A room is multi-principal and is not treated as one

## Goal

Give the room path the request-owned identity the safety invariant already requires of
multi-principal surfaces.

`AGENTS.md:117`: *caller identity is immutable for a live turn, and multi-principal surfaces pass a
request-owned `TurnIdentity` through `run_turn_as`/`run_turn_cancellable_as`.*

There is **exactly one caller** of those entry points — `crates/flux-server/src/lib.rs:318` and
`:341`. The room path uses plain `run_turn` (`crates/flux-app/src/app.rs:1592`), which snapshots the
executor's assembly-time identity (`crates/flux-flow/src/engine.rs:736`), installed by the CLI as
`ExecutionAuthorization::local()` (`crates/flux-cli/src/app_cmd.rs:339`) → `Caller { id: "local",
kind: User }` at `Trust { level: Privileged }` (`crates/flux-policy/src/lib.rs:95`).

The immutability half of the invariant holds. The **request-owned half is simply unexercised** — and
a stable per-occupant `speaker` id already exists and is already carried in the payload
(`crates/flux-channels/src/adapters/room.rs:154`).

⚠ **Currently inert, which is exactly why it is worth doing now.** No grant on the app path keys on
trust level or principal id — so the first grant that *does* will silently treat a room stranger as
the local operator at Privileged trust.

## Acceptance

- [x] **Failing-first**: a test asserting two different room speakers produce two different caller
      identities in the evidence record — failing at the merge base, where both read `local`.
- [x] The room path passes a request-owned `TurnIdentity` derived from `speaker`, through
      `run_turn_as`/`run_turn_cancellable_as`.
- [x] The trust level a room participant receives is **decided explicitly**, not inherited. A remote
      occupant at `Privileged` is the thing this story exists to remove.
- [x] The evidence record attributes the turn to the speaker, not to `local`.
- [x] Full gate green.

## Notes

- No story mentions per-speaker identity: D-207 is addressing and budget, D-219 is allow-lists.
- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F2.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
- **2026-08-01 — implemented.** `run_agent` (`crates/flux-app/src/app.rs`) now runs a room delivery
  through `run_turn_as` under the speaker's own identity; every event source that names no principal
  still uses plain `run_turn` and keeps the executor's assembly-time identity, the same `Some`/`None`
  shape `flux-server` uses.
  - **The trust decision, made explicitly: `Untrusted`.** Assigned in exactly one place —
    `TurnIdentity::unauthenticated_participant` (`crates/flux-runtime/src/lib.rs`) — with the
    reasoning beside it: a room occupant presented no credential, so the surface can say *which*
    stranger spoke and nothing more. `kind` deliberately stays `User`, because `subject_matches`
    discriminates on it and flipping it would be a capability change disguised as an attribution
    change. Against `default_local_grants` (subject `User`/`*`, `required_trust: Untrusted`) this
    authorizes exactly what the local profile did, so the change is attribution-only today.
  - **The id is an attribution, not an authentication.** It is derived from the payload — the only
    thing `Deliverer::deliver` carries — so another surface can present the same `room` + `speaker`
    shape. A forgery buys an *untrusted* named stranger in place of the local operator: strictly less
    authority, never more. Carrying identity out of band would be the stronger boundary and is a
    change to `Deliverer` + `App::deliver` that this story does not sanction.
  - Tests: `two_room_speakers_are_two_caller_identities_in_the_evidence_record` and
    `an_event_that_names_no_principal_keeps_the_assembly_time_identity` (flux-app),
    `an_unauthenticated_participant_is_named_but_never_trusted` (flux-runtime), and the room-side
    supply pin `two_occupants_sharing_a_nick_still_deliver_two_speakers` (flux-channels).
  - **Still open, deliberately out of scope:** the *journey* half of a room delivery. `run_journey`
    scopes a `RuntimeTurnContext` without an identity (`crates/flux-app/src/app.rs`), so a
    room-triggered journey's op dispatches still authorize as `local`/`Privileged`. It has no engine
    turn and emits no `turn.identity` record, so no Acceptance item above can be met there, and its
    forgeability profile differs (a nested `spawn` payload is model-chosen). Worth its own story.
