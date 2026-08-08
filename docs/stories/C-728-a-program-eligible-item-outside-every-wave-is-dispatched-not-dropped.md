---
id: C-728
title: "A program-eligible item outside every wave is dispatched, not dropped"
pillar: "Core"
status: done
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

- [x] A program-eligible, dependency-satisfied item that no configured wave names is dispatched
      rather than dropped. `flux/C-600`, `flux/C-601` and `flux/C-622` are the fixtures: all three
      were `eligible: true` in the milestone program and invisible to the driver until a wave was
      written by hand.
- [x] No item leaves the schedulable pool without a record. If an item is not dispatched the tick
      says which and why, in the same `withheld` shape C-723 established — silence is the defect.
- [x] Items without a configured wave are grouped into a dispatchable unit that respects the
      existing bounds: one repository per wave, at most `max_wave` stories, and the configured
      worker width.
- [x] A hand-configured wave still wins where one exists. This widens dispatch; it does not
      reinterpret a wave an operator deliberately composed.
- [x] `flux fleet doctor` reports a ready, dependency-satisfied item that no tick has dispatched
      across N consecutive ticks, so a permanently invisible item cannot masquerade as an empty
      queue.
- [x] Regression test: a fixture whose program marks an item eligible while no `[[waves]]` entry
      names it is dispatched by a dry-run tick, and the tick's output accounts for every eligible
      item either as dispatched or as withheld with a reason.

## Progress

`fleet_schedule` now composes the leftovers instead of reading only `[[waves]]`. Every eligible item
no configured wave names is grouped by repository into a synthesized wave (`unplanned-<repository>`,
`"synthesized": true`) carrying the same bounds a configured one does: exactly one repository and at
most `max_wave` stories. An item a `[[waves]]` entry names stays that wave's to schedule whatever
state the wave is in, so a composed wave is never reinterpreted.

Whatever still reaches no wave leaves `fleet_schedule` under a new `unschedulable` key with a
reason — `wave-capacity`, `wave-dependencies`, `wave-done`, `repository-unconfigured` — and
`drive_tick_plan` turns each into a `withheld` record in C-723's shape. A second, unconditional pass
over `program_items` names anything the schedule failed to account for as `unscheduled`, so the
"dispatched or explained" invariant is enforced where dispatch is decided rather than trusted to the
code that computed the schedule.

Same defect, second instance, fixed alongside: the synthetic `ready` wave both fallback branches
emit carried `items` and no `eligible`, and `eligible` is the only key dispatch reads — a fleet with
no `.flux/board.toml`, or one with neither program nor waves, offered dispatch nothing at all.

`fleet doctor` gained `item-never-dispatched` (`FLEET_RUNTIME_CHECKS` is now 11). Its streak is
counted from the eligible pool minus what the tick sent, not from the withhold map, so it still
fires for an item no tick ever explained; an item `item-withheld-persistently` already names is left
to that check rather than reported twice.

Verified against the artifact, not the intent: a dry-run tick over a fixture with eight free slots
and eight eligible items — five in a hand-written wave, three in none — dispatches all eight; the
same fixture at `max_wave = 2` plus a member with no `[[repositories]]` entry dispatches four and
names the other five (`wave-capacity` ×4, `repository-unconfigured` ×1).

Not addressed, and left as findings rather than fixed: the driver still flattens every eligible
wave into one `FleetAction::Run`, so a dispatch spanning two repositories still produces one wave
instance; and a `plan.width` above `max_wave` still lets `run` refuse the whole dispatch.
