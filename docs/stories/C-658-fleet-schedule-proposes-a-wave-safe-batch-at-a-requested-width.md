---
id: C-658
title: "fleet schedule proposes a wave-safe batch at a requested width"
pillar: "Core"
status: ready
priority: 6
areas: [flux-cli]
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
depends_on: [C-657]
note: "board next --independent picks the set; nothing yet asks it for a wave, and the board cannot see fleet claims"
---

# fleet schedule proposes a wave-safe batch at a requested width

## Goal

Let the coordinator ask for a dispatchable wave in one call. `flux board next --independent` now
returns the largest wave-safe set, but nothing consumes it: `fleet schedule` takes no width, so the
agent that actually dispatches still has to choose a batch by hand — which is how the fleet ended up
being handed prefixes of the priority order that could not integrate.

There is also a real gap the board verb cannot close on its own. A board knows what is *ready*; it
does not know what a live wave already *owns*. Dispatching a second worker at a story another wave
holds is how the same commit gets implemented twice — the concrete failure that `board reconcile`
(`C-637`) exists to catch after the fact, and that this catches before.

## Acceptance

- [ ] `flux fleet schedule --width N` returns a proposed wave of at most N mutually independent
      items, using the same selection as `board next --independent` rather than a second
      implementation of it.
- [ ] Items owned by a wave that has not reached a terminal state are excluded, and each exclusion
      names the wave holding it.
- [ ] A wave recorded `agent-turn-failed` with no live supervisor does not hold its items forever;
      the rule is stated where an operator reads it and shares the liveness derivation with
      `worker_activity`, so status and scheduling cannot disagree.
- [ ] `--dry-run` proposes without writing, and the proposal names, per excluded item, what blocked
      it — collision, claim or width.
- [ ] The proposal reports the achievable width against the requested one, so "the fleet only ran
      three workers" is answerable without reading state by hand.
- [ ] Failing first, a test proves a claimed item is excluded from a proposal while remaining
      `ready` on the board.

## Notes

- **Separation is deliberate.** `board next --independent` stays free of fleet concepts: a board
  answers what is ready and independent, and a fleet answers what is free. Teaching the board about
  waves would put fleet state behind a planning verb.
- **Width is a ceiling, not a target.** Reporting `3 of 8` is the useful output; silently returning
  three and calling it a wave is how a backlog-shaped constraint gets mistaken for a fleet problem.
- Depends on `C-657`: at crate-level areas the answer is three regardless of how good the scheduler
  is, so shipping this first would produce a well-engineered verb that cannot demonstrate its value.
