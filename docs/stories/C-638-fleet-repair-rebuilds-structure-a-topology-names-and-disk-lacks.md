---
id: C-638
title: "fleet repair rebuilds structure a topology names and disk lacks"
pillar: "Core"
status: ready
priority: 12
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
design: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
note: "reclamation removed an integration worktree an unfinished wave still needed; rebuilding it took a hand-written git worktree add"
---

# fleet repair rebuilds structure a topology names and disk lacks

## Goal

Make rebuilding a wave's structure a verb. Reclamation removed an integration worktree an unfinished
wave still needed, and putting it back took a hand-written `git worktree add` against a base read out
of `state.json` — the same class of hand repair as the `git reset --hard <base>` an integration
worktree needed, repeatedly, before handoffs would verify. Every input is already recorded, so both
are mechanical; the only thing missing is somewhere to ask for them.

## Acceptance

- [ ] `flux fleet repair <wave>` recreates every worktree the wave's topology names and disk lacks,
      checking out the **recorded branch** so committed work returns with the checkout, and creating
      the branch at the pinned base only when it is gone too.
- [ ] A derived worktree — the integration assembly and the pinned verification checkout — that has
      drifted off its base is returned to it, and the report names the commit it left.
- [ ] A story worktree is never rewound: being ahead of its base is the deliverable, not damage.
- [ ] Repair refuses, with a recorded reason rather than an action, anything that would discard work:
      an uncommitted change, a checkout git cannot inspect, and the commit a gate recorded as its
      candidate. An applied or cancelled wave is refused outright.
- [ ] The verb is a mutation in `flux fleet schema`, honours `--dry-run`, and journals `wave.repaired`.
- [ ] Failing first, a test proves a removed story worktree comes back holding its delivered commit,
      that a pinned worktree holding an uncommitted change is refused untouched, and that a clean one
      is returned to its base.
