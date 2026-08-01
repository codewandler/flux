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
    shape. A forgery buys an *untrusted* named stranger in place of the local operator — strictly
    less authority in the trust dimension; see the review-round note below for the subject dimension,
    where that is not true. Carrying identity out of band would be the stronger boundary and is a
    change to `Deliverer` + `App::deliver` that this story does not sanction.
  - Tests: `two_room_speakers_are_two_caller_identities_in_the_evidence_record` and
    `an_event_that_names_no_principal_keeps_the_assembly_time_identity` (flux-app),
    `an_unauthenticated_participant_is_named_but_never_trusted` (flux-runtime), and the room-side
    supply pin `two_occupants_sharing_a_nick_still_deliver_two_speakers` (flux-channels).
- **2026-08-01 — review round.** Corrected one overclaim and pinned two properties that the
  implementation relied on but no test distinguished:
  - The doc claimed a forged `room` + `speaker` shape buys "strictly less authority, never more".
    True in the *trust* dimension, false in the *subject* one — `subject_matches` wildcards a `user`
    subject against `principal.id`, so a grant `subjects = [user "alice"]` at `required_trust:
    Untrusted` is satisfied by a claimed `speaker: "alice"` and is not satisfied by `local`. Not
    reachable in-tree (`flux app run` grants subject `user "*"`), reachable for an embedder with an
    id-keyed grant. The comment now separates the two dimensions.
  - `a_payload_that_is_not_the_room_shape_gets_no_participant_identity` pins that **both** fields are
    required and that empty/whitespace ids are rejected. Both clauses were verified by deletion:
    dropping `field("room")?` yields `caller: standup@rooms.example/ada`, dropping the filter yields
    `caller: ""`, and each fails the test.
  - The malformed-`speaker` path is the one input direction that fails **open** (back to
    `local`/`Privileged`); accepted rather than failing closed, because refusing a delivery would let
    one bad field silence a live meeting (D-205's posture), and now written down.
  - **Still open, and now a follow-up story:** the *journey* half of a room delivery. `run_journey`
    scopes a `RuntimeTurnContext` without an identity (`crates/flux-app/src/app.rs`), so a
    room-triggered journey's op dispatches still authorize as `local`/`Privileged` — and the existing
    room integration test drives exactly that shape. It has no engine turn and emits no
    `turn.identity` record, so no Acceptance item above can be met there. The load-bearing reason to
    treat it separately is **not** "it changes authorization inputs for every delivery" (gating it on
    the same `room_participant_identity(payload).is_some()` would scope it identically): it is that
    `run_journey` is also reached from `run_journey_for_spawn`, i.e. from a **model-chosen `spawn`
    payload**, which the agent path is not.
