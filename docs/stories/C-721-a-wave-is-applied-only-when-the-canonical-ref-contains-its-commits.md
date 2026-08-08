---
id: C-721
title: "A wave is applied only when the canonical ref contains its commits"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "wave-649 is recorded applied while X-139's two commits exist only on fleet/wave-649/exchange/story/X-139: absent from origin/main, absent from local main, and absent from the wave's own integration branch. apply never pushes, yet exchange and connectors declare canonical_ref = origin/main, so apply cannot reach their canonical ref by construction and still reports success"
---

# A wave is applied only when the canonical ref contains its commits

## Goal

`applied` is the fleet's word for "this work is delivered". For `wave-649` it is false in every
sense that matters: X-139's two commits are absent from `origin/main`, absent from local `main`,
and absent from the wave's own integration branch — they exist only on
`fleet/wave-649/exchange/story/X-139`. The status was recorded without its preconditions ever
holding, and the fleet now contradicts itself: `flux fleet apply wave-649` refuses with "no
recorded green final gate" and `flux fleet integrate wave-649` refuses with "not ready for
integration", while `fleet status` reports the wave as `applied`.

The work is stranded in a way no CLI operation can undo, which is the part that makes this a
harness defect rather than a bad run: there is no supported path back.

## Acceptance

- [ ] `applied` is recorded only after re-reading each repository's canonical ref and confirming it
      contains every accepted story commit. The check is commit containment, not a status field.
- [ ] A repository whose `canonical_ref` is a remote-tracking ref is handled honestly. `apply`
      never pushes, so for `exchange` and `connectors` (`canonical_ref = "origin/main"`) it cannot
      reach the canonical ref by construction. Either config validation refuses that combination,
      or `apply` states that delivery requires a push it will not perform — and in neither case
      does the wave become `applied`.
- [ ] `flux fleet doctor` reports any wave recorded `applied` whose canonical ref lacks its
      commits, naming wave, repository, story and the missing commit.
- [ ] The recorded status and the operations' preconditions can never disagree: no wave is
      `applied` without a recorded green final gate.
- [ ] There is a CLI path to move a wave out of a status whose preconditions never held, so
      stranded work can be re-delivered without hand-editing state or hand-merging branches.
- [ ] Regression test: `wave-649` is the fixture — a wave marked `applied` whose story commits
      reach neither the integration branch nor the canonical ref must be reported, and must be
      recoverable. See [[C-720]] and [[C-722]].
