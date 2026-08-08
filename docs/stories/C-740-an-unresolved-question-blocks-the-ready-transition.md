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

- [x] `[NEEDS CLARIFICATION: ...]` anywhere in a story is a hard refusal of the `backlog -> ready`
      transition, naming each occurrence.
- [x] It is not an error at `backlog`, so drafting with open questions is normal and the marker is
      usable rather than something authors avoid.
- [x] `board check` reports the count of open questions per story without failing on them.
- [x] A dispatched story can never contain one, because it could not have reached `ready`.
- [x] Regression test: a story with a marker is refused promotion and the same story passes once the
      question is answered and the marker removed.

## Progress

- The refusal is attached to the transition edge, not to the story type: `transition_with_override`
  guards **every** edge whose target is `ready`, not only `backlog -> ready`. `blocked -> ready`
  reaches the same dispatchable state, and an invariant with one unguarded entrance is not one.
- `board create` generates the story body itself, so a story cannot be created at `ready` carrying a
  marker. Together with the guarded edges that is what makes the dispatch invariant hold.
- Markers inside a code span or a fenced block are documentation, not questions. Without that
  exclusion this story's own Acceptance would make it unpromotable by its own rule. Verified against
  the real board: `board check` over 1,260 stories reports zero open questions and the one
  pre-existing `C-320` warning.
- The escape hatch is `board transition <id> ready --override-reason "<why>"`, which writes
  `ready_override:` into the story's frontmatter — the `done_override:` precedent. A story can
  therefore reach `ready` with an open question, but never silently.
