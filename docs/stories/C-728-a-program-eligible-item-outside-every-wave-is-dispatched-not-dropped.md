---
id: C-728
title: "A program-eligible item outside every wave is dispatched, not dropped"
pillar: "Core"
status: ready
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "A tick with eight free slots and eight program-eligible dependency-satisfied items planned four dispatches and recorded no reason for the other four. Dispatch draws from configured waves, so an item named by no wave never reaches the withhold logic and leaves the schedulable pool with no record at all. C-723 made a withhold state its evidence; this is the path that produces no withhold to state"
---

# A program-eligible item outside every wave is dispatched, not dropped

## Goal

A tick with eight free slots and eight program-eligible, dependency-satisfied items planned four
dispatches and recorded **no reason** for the other four. Dispatch draws from configured `[[waves]]`,
so an item named by no wave never reaches the withhold logic at all — it leaves the schedulable pool
with no record, which is indistinguishable from a pool that never held it.

C-723 made a withhold state its evidence. This is the path that produces no withhold to state, and
it is the reason an operator must hand-write a wave for every story before the fleet can see it.

## Acceptance

- [ ] A program-eligible, dependency-satisfied item that no configured wave names is dispatched
      rather than dropped. `flux/C-600`, `flux/C-601` and `flux/C-622` are the fixtures: all three
      were `eligible: true` in the milestone program and invisible to the driver until a wave was
      written by hand.
- [ ] No item leaves the schedulable pool without a record. If an item is not dispatched the tick
      says which and why, in the same `withheld` shape C-723 established — silence is the defect.
- [ ] Items without a configured wave are grouped into a dispatchable unit that respects the
      existing bounds: one repository per wave, at most `max_wave` stories, and the configured
      worker width.
- [ ] A hand-configured wave still wins where one exists. This widens dispatch; it does not
      reinterpret a wave an operator deliberately composed.
- [ ] `flux fleet doctor` reports a ready, dependency-satisfied item that no tick has dispatched
      across N consecutive ticks, so a permanently invisible item cannot masquerade as an empty
      queue.
- [ ] Regression test: a fixture whose program marks an item eligible while no `[[waves]]` entry
      names it is dispatched by a dry-run tick, and the tick's output accounts for every eligible
      item either as dispatched or as withheld with a reason.
