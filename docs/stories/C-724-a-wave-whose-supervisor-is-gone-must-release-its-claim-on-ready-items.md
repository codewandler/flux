---
id: C-724
title: "A wave whose supervisor is gone must release its claim on ready items"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "wave-745 kept an exclusive claim on C-575 and C-519 after its supervisor pid 3513527 died. doctor detected agent-supervisor-gone and prescribed cancelling the two workers, but cancelling the workers did not release the wave's claim; only cancelling the wave itself did, taking dispatch from 1 item to 3. Nothing reclaims a claim held by a dead supervisor, so a driver crash removes its items from the schedulable pool permanently"
---

# A wave whose supervisor is gone must release its claim on ready items

## Goal

A claim exists so two waves never write the same story at once. It must not outlive the thing
that holds it. `wave-745`'s supervisor — pid 3513527 — died overnight, and the wave went on
claiming `C-575` and `C-519` against a process that no longer existed. Those two stories sat at
the top of a ready board that the driver reported as having nothing to dispatch.

`doctor` already detects the condition: it reported `agent-supervisor-gone` for both workers and
prescribed cancelling them. But cancelling both workers did **not** release the wave's claim. Only
`flux fleet cancel wave-745` did, and dispatch immediately went from one item to three. The
detection was right, the prescribed remedy was insufficient, and nothing closed the gap on its own.

A driver crash therefore removes its in-flight items from the schedulable pool permanently, which
is the opposite of what an unattended machine should do with an interrupted turn.

## Acceptance

- [ ] A claim is held against a live supervisor. When the supervisor process is gone, the claim is
      released — by the next tick, without an operator command.
- [ ] Releasing a claim never discards work: the story branch, its worktree and any uncommitted
      contents survive untouched. See [[C-722]].
- [ ] `doctor`'s prescribed fix for `agent-supervisor-gone` actually resolves the finding. Today it
      names the workers, and cancelling them leaves the wave still claiming; the prescription must
      either name the wave or the release must be automatic.
- [ ] An item whose claiming wave is not in flight is schedulable, and re-dispatch starts from the
      story's committed state rather than from scratch.
- [ ] `flux fleet doctor` reports any claim held by a wave with no live supervisor, naming wave,
      item and the dead pid.
- [ ] Regression test: a wave recorded in flight whose supervisor pid does not exist releases its
      items on the next tick, and a dry-run tick then dispatches them. `wave-745` with pid 3513527
      is the fixture. See [[C-720]].
