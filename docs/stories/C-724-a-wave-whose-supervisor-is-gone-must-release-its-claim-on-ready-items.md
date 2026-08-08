---
id: C-724
title: "A wave whose supervisor is gone must release its claim on ready items"
pillar: "Core"
status: done
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

- [x] A claim is held against a live supervisor. When the supervisor process is gone, the claim is
      released — by the next tick, without an operator command.
- [x] Releasing a claim never discards work: the story branch, its worktree and any uncommitted
      contents survive untouched. See [[C-722]].
- [x] `doctor`'s prescribed fix for `agent-supervisor-gone` actually resolves the finding. Today it
      names the workers, and cancelling them leaves the wave still claiming; the prescription must
      either name the wave or the release must be automatic.
- [ ] An item whose claiming wave is not in flight is schedulable, and re-dispatch starts from the
      story's committed state rather than from scratch.
- [x] `flux fleet doctor` reports any claim held by a wave with no live supervisor, naming wave,
      item and the dead pid.
- [x] Regression test: a wave recorded in flight whose supervisor pid does not exist releases its
      items on the next tick, and a dry-run tick then dispatches them. `wave-745` with pid 3513527
      is the fixture. See [[C-720]].

> **Acceptance 4 (second half) moved to [[C-726]].** The work landed here is complete and gated;
> that criterion is tracked separately rather than left as an open box on a delivered story.

## Progress

Implemented and gated on `impl/C-724`, integrated as part of the `delivery-is-verified` wave.

- A supervisor counts as gone only when every worker recorded a pid and **every** one is provably
  absent, and absence means `errno == ESRCH` specifically — not `kill(pid, 0) != 0`, which conflates
  a dead process with one owned by another user. A process this user may not signal therefore reads
  as *alive*, and no claim is released on a platform without signals.
- The release runs last in the tick's delta, after handoff reconstruction and wave advancement,
  because those phases can turn an interrupted wave into one holding delivered work.
- Releasing is state-only: `abandoned` is excluded from `wave_worktrees_are_removable` and
  `wave_is_reclaimable`, so the story branch, its worktree and any uncommitted contents survive.
- Residual risk is pid reuse, which holds a claim too long — the same direction as today's
  behaviour, and the safe one.

**Outstanding — the second half of criterion 4.** Re-dispatch still starts from the canonical ref,
not from the abandoned attempt's committed state. Deliberately not done here: `plan_wave_topology`
pins `base_commit` from the canonical ref, and handoff verification requires that the cited test
fails at that pinned base. Seeding a new wave from an old attempt's branch — which carries a
failing-first test and a stale base — inverts that evidence. It needs its own story and a design
decision rather than a side effect of this one.
