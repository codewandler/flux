---
id: C-749
title: "A ready item is the fleet's work whether or not a human wrote a lane for it"
pillar: "Core"
status: ready
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
depends_on: [C-728]
note: "104 ready, contract-carrying stories; 8 free worker slots; 0 dispatched. Every flux program lane in the active milestone is done, and dispatch reads only `[[waves]]`. The empty-board fallback makes a fresh board dispatch everything and a mature board dispatch nothing — the queue narrows as the project matures"
---

# A ready item is the fleet's work whether or not a human wrote a lane for it

## Goal

On 2026-08-08 the fleet had **104 `ready`, dependency-satisfied stories**, **8 free worker slots**,
and dispatched **0**. Not withheld with a reason — invisible. This is the standing condition, not an
incident: the 2026-08-07 run spent 85% of its ticks reporting an empty queue while the board held
roughly a hundred dispatchable stories.

Two gates produce it, and [C-728](C-728-a-program-eligible-item-outside-every-wave-is-dispatched-not-dropped.md)
removes only the first.

**Gate one — dispatch reads waves, nothing else.** `board_fleet_cmd.rs:16304`:

```rust
let waves = if config.waves.is_empty() && config.program.is_empty() {
    vec![json!({"id": "ready", "state": "active", "items": ready_refs})]   // everything ready
} else {
    config.waves.iter().map(/* only items a `[[waves]]` entry names */)
};
```

`schedule_eligible_items` reads `wave["eligible"]` and only that. The `program` array a few lines
above is computed, reported, and contributes nothing to dispatch. C-728 fixes this.

**Gate two — the program is exhausted and nothing can extend it.** Once C-728 lands, admission
becomes program membership. Every `flux` lane in the active milestone is `done` except two at
`backlog`. There is **no `flux board program` verb**: `[[program]]` is hand-authored TOML in
`.flux/board.toml` that no operation writes, so no agent, loop or driver can add a lane. The fleet
runs until a human stops typing TOML, and then idles forever with the work sitting in plain sight.

The fallback above makes this precise and slightly absurd: a board with **no** waves and **no**
program dispatches everything that is ready. Adding one lane switches admission off for everything
else. The queue gets narrower as the project matures, which is exactly backwards.

So the rule this story sets: **the program and the waves order and batch work; they do not decide
whether work exists.** An item that is `ready` and dependency-satisfied is the fleet's work. Where a
lane or a wave speaks, it still wins — this widens admission, it does not overrule a sequencing
decision anyone made deliberately.

## Acceptance

- [ ] A `ready`, dependency-satisfied item named by no `[[program]]` lane and no `[[waves]]` entry is
      dispatched. `C-643`, `C-647` and `C-657` are the fixtures: all three are `ready`, carry
      contracts, and are named nowhere in `.flux/board.toml`.
- [ ] Ordering still comes from the program where it speaks. An item with a lane is ordered by
      `order` and gated by `depends_on` exactly as today; an item without one sorts after every
      programmed item, by board priority. A test asserts a programmed item is dispatched ahead of an
      unprogrammed one at equal readiness, so widening admission cannot silently reorder a milestone.
- [ ] A hand-written `[[waves]]` entry still composes its own batch, per C-728's fourth criterion.
      This story must not make a deliberately composed wave equivalent to an ad-hoc one.
- [ ] The bounds hold for the widened pool: one repository per wave, at most `max_wave` items,
      the configured worker width, and every existing withhold reason. The pool gets larger; nothing
      about the ceiling changes.
- [ ] Every `ready` item is accounted for in the tick output as dispatched, withheld-with-reason, or
      not-yet-reached-because-the-width-is-full. Zero items may leave the pool unrecorded — that is
      C-723's rule and this is the last path that evades it.
- [ ] `flux fleet doctor` reports the count of `ready`, dependency-satisfied, contract-carrying items
      against the count dispatched over the last N ticks, and flags a fleet that is idle while work
      is available. An empty queue and a full-but-invisible one must never again read the same.
- [ ] Regression test on a fixture board holding both shapes — a programmed-and-waved item and a bare
      `ready` one — asserting a dry-run tick dispatches both and orders them as above.
- [ ] Full gate green: `scripts/release-full-gate.sh`.

## Notes

- This is the second half of C-728 and only makes sense after it. C-728 makes an unwaved
  *program-eligible* item dispatchable; this makes an unprogrammed *ready* item dispatchable. Landing
  C-728 alone leaves the fleet exactly as idle as it is today, because the program is empty of
  anything not already `done`.
- The alternative design — a `flux board program` verb so the coordinator's planner loop authors
  lanes — was considered and is worse as the primary fix. It keeps admission gated on a generated
  artifact, so the failure mode becomes "the planner did not run" instead of "the human did not
  type", which is the same availability ceiling wearing a different hat. A `program` verb is still
  worth having for *ordering*; it is not what makes the fleet non-idle.
- `flux board next` already returns exactly the set this story wants dispatched — "dependency-satisfied
  ready items in priority order". The queue the fleet needs is already computed by a verb the fleet
  does not consult.
- Related: [C-723](C-723-a-withheld-story-states-its-evidence.md) established that silence is the
  defect and every non-dispatch must state its reason; this closes the last path that produces no
  record at all.
