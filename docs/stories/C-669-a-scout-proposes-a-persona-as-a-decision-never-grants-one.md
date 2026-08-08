---
id: C-669
title: "A scout proposes a persona as a decision, never grants one"
pillar: "Core"
status: backlog
priority: 15
epic: scouting-makes-the-backlog-schedulable
areas: [flux-cli]
depends_on: [C-666]
note: "an agent_templates entry carries capabilities, fences and a model; a read-only scout must not be able to mint one"
---

# A scout proposes a persona as a decision, never grants one

## Goal

Let the scout say "this ticket needs a persona that does not exist yet" without letting it create one.

There are exactly two agent templates today — `story-worker` and `wave-integrator` — and every story
goes to the same one regardless of whether it is architecture, research or prose. The scout is the
first thing in the system positioned to notice that mismatch, because it is the first thing that
reads a ticket in order to classify it.

But a persona is an `[[agent_templates]]` entry, and that entry carries `capabilities`, `fences` and
a model. It is an authority grant. A read-only scout that could write one could grant itself
anything, which would make its own `capabilities = ["read"]` decorative.

So the scout proposes and a human disposes, through machinery that already exists: `flux board
decision create` / `open`, records that are already `proposed | accepted | superseded`, already
surface in `fleet status` under `open_decisions`, and already respect `decision_mode = "human"`.

## Acceptance

- [ ] When no existing template fits an item's discipline and required capabilities, the scout emits
      a persona proposal naming the discipline, the capabilities, the fences and why the existing
      templates do not fit.
- [ ] The proposal lands as a decision record in `proposed`, linked to the items that prompted it, so
      the reason for a persona is traceable to concrete work rather than to a hunch.
- [ ] Work **cannot** be dispatched to a proposed persona. Dispatch resolves templates only from
      accepted configuration, and a test proves a proposed one is refused.
- [ ] Accepting the decision is what makes the template dispatchable, and the path from accepted
      decision to live template is written down rather than implied.
- [ ] The scout cannot write `fleet.toml`, and a test pins that its capability and fence set makes it
      impossible rather than merely unlikely.
- [ ] Duplicate proposals collapse: a second item needing the same missing persona joins the existing
      decision instead of opening another.
- [ ] Failing first, a test proves a scout run that needs a missing persona produces a `proposed`
      decision and changes no configuration.

## Notes

- **Why a decision record and not a new approval flow.** Decisions already block linked stories, are
  already surfaced to the operator, and already have accept/supersede semantics. Inventing a parallel
  approval path would mean a second thing that can be forgotten in a different place.
- The scout classifies into a discipline; it does not invent the discipline vocabulary. If the fixed
  list turns out to be wrong, that is a decision of its own and should be recorded as one.
