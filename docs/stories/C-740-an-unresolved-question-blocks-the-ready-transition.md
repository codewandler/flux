---
id: C-740
title: "An unresolved question blocks the ready transition"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-cli]
---

# An unresolved question blocks the ready transition

## Goal

An agent will never ask for clarification. It will guess, implement the guess, and commit it — and
the result compiles and passes its own tests. A greppable marker that makes not-asking illegal is the
cheapest available defence against that failure mode, and it is what Spec Kit's
`[NEEDS CLARIFICATION]` exists for.

## Acceptance

- [ ] `[NEEDS CLARIFICATION: ...]` anywhere in a story is a hard refusal of the `backlog -> ready`
      transition, naming each occurrence.
- [ ] It is not an error at `backlog`, so drafting with open questions is normal and the marker is
      usable rather than something authors avoid.
- [ ] `board check` reports the count of open questions per story without failing on them.
- [ ] A dispatched story can never contain one, because it could not have reached `ready`.
- [ ] Regression test: a story with a marker is refused promotion and the same story passes once the
      question is answered and the marker removed.
