---
id: C-408
title: "Every room participant shares one `local`/Privileged identity, so a stranger is attributed to the operator"
pillar: Core
status: ready
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

- [ ] **Failing-first**: a test asserting two different room speakers produce two different caller
      identities in the evidence record — failing at the merge base, where both read `local`.
- [ ] The room path passes a request-owned `TurnIdentity` derived from `speaker`, through
      `run_turn_as`/`run_turn_cancellable_as`.
- [ ] The trust level a room participant receives is **decided explicitly**, not inherited. A remote
      occupant at `Privileged` is the thing this story exists to remove.
- [ ] The evidence record attributes the turn to the speaker, not to `local`.
- [ ] Full gate green.

## Notes

- No story mentions per-speaker identity: D-207 is addressing and budget, D-219 is allow-lists.
- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F2.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
