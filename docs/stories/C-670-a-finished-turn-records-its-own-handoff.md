---
id: C-670
title: "A finished turn records its own handoff"
pillar: "Core"
status: done
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
note: "handoff-accepted is written only by the CLI verb, and no agent in the fleet can invoke it; ten workers ended their turns and the fleet recorded nothing"
---

# A finished turn records its own handoff

## Goal

Close the loop between a worker finishing and the fleet knowing it finished.

`handoff-accepted` is written in exactly one place — the `story.handoff.accepted` mutation — and it is
reachable only from the CLI verb `flux fleet handoff`. **No agent in the system can call it:**

- the **story worker** has `capabilities = ["read","edit","git","rust","node"]` and no fleet
  operations at all. Its own prompt tells it to *report* evidence "for `flux fleet handoff`", which is
  to say: for somebody else to run.
- the **coordinator** has sixteen native operations — `board.*` and
  `fleet.status/schedule/run/message/cancel/resume/integrate`. `fleet.handoff` is not among them.
- the **integrator** has `fleet.integrate`, which *refuses* unless every story is already
  `handoff-accepted`.

So the loop is open by construction. A turn ends, `agent.turn.completed` fires, the wave sits in
`accepted` forever, and only an operator or an external shell script can move it. The only script that
ever existed logged `HALT: fleet reports attention_required; not acting` 102 times and was stopped.

Measured 2026-08-07: ten wave-472 workers ran, ended their turns, and left **nine real commits on
branches the fleet was not tracking**. Every agent read `cancelled`; every story read `handoff: null`.
Recovering it took a hand-assembled candidate and a hand-written merge.

## Decision: a provisional record on turn end, not a coordinator operation

Both shapes were on the table. **A terminal turn records the handoff directly**, and here is why the
coordinator operation lost.

Exposing `fleet.handoff` as a native operation keeps the loop dependent on an agent being alive and
choosing to call it. That is the same dependency that failed: on the night this was found the
coordinator was stopped, and the only driver was a shell script that logged `HALT` 102 times. A fix
whose precondition is "somebody is watching" does not address a defect whose cause was that nobody
was. It also puts a mutation that advances a wave behind a model's judgement, where a missed call is
indistinguishable from a wave that legitimately has nothing to hand off.

Recording at turn end has neither problem: the code that already writes the turn's outcome writes
the handoff, in the same pass, from the worktree the turn just finished in.

What makes that safe is that the record is **provisional and says so**. It proves the git facts
through the *same* `verify_story_commit` the operator path uses — the commit is the worktree HEAD, the
branch points at it, it descends from the pinned base, the tree is clean, and the observed write set
is non-empty. It does **not** carry targeted validation evidence, because a turn that has already
ended cannot be asked to cite the argv it ran. So the entry records `provisional: true` with an empty
`test_argv`, and integration still runs the repository's full gate, which is what actually decides
whether the wave is green.

The story's own warning still holds and is respected: the worker is given nothing. It has no new
capability and no fleet operation. The fleet observes what the worker left behind — it does not ask
the worker to vouch for it.

## Acceptance

- [x] A worker's terminal turn produces a handoff record without an operator, derived from its own
      worktree — the branch, the commit, and the write set in `base..HEAD`.
- [x] The choice is stated and defended in the story: either `fleet.handoff` becomes a native
      operation some agent holds, or a terminal turn records a provisional handoff directly. Today it
      is neither, and "neither" is the defect.
- [x] A handoff recorded this way is distinguishable from an operator-verified one, so automatic
      acceptance never silently becomes the same claim as reviewed acceptance.
- [x] A turn that ends with no commit records that fact explicitly rather than nothing, so an empty
      worker is visible instead of indistinguishable from an unrecorded one.
- [x] The existing verification is unchanged: an observed write set that disagrees with a claimed one
      is still a refusal, not a warning.
- [x] Failing first, a test proves a worker that ends its turn with a commit leaves the wave able to
      advance with no operator command in between.

## Notes

- **Do not fix this by giving the worker the verb.** A writer that can accept its own handoff can mark
  its own work verified, which is the one thing the handoff exists to prevent. The credible shapes are
  a coordinator-held operation, or a provisional record the integrator must still verify.
- Depends in spirit on `C-641` (derive the write set and owning worker from the worktree), which
  landed with the wave-472 recovery — that is the derivation this story automates the calling of.
- `C-671` is the other half: even with an actor, a killed supervisor leaves no evidence to hand off.
