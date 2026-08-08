---
id: C-657
title: "Split board_fleet_cmd so fleet verbs stop serialising on one file"
pillar: "Core"
status: ready
priority: 5
areas: [flux-cli]
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
note: "one 17k-line file holds all 52 FleetAction arms, so every story in the ops-CLI epic collides with every other one"
---

# Split board_fleet_cmd so fleet verbs stop serialising on one file

## Goal

Make the fleet's own backlog parallelisable. `crates/flux-cli/src/board_fleet_cmd.rs` is 16,986
lines and holds all 52 `FleetAction::` arms, the board planning actions, the native coordinator tool
surface, the TUI source and the projections. Every story that adds a fleet verb writes that file, so
no two of them can be built at the same time — integration refuses a wave in which two stories wrote
the same path.

This is not a style complaint. It is the measured reason the fleet cannot run wide:

- `flux board next --limit 8` on this board returns `C-618, C-637, C-635, C-636, C-641, C-639,
  C-638, C-642` — **eight stories, all declaring `flux-cli`**, all landing in this one file.
  Dispatching that as a wave buys eight workers and one guaranteed integration refusal.
- `flux board next --limit 8 --independent` returns **three**, and names the eleven it held back.
  Ten of those eleven are held back by `shared area flux-cli`.

So the ceiling on width is not the fleet, the disk or the model budget. It is this file. Until it is
split, the whole `recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven` epic — the
verbs that make the fleet operable at width in the first place — is strictly serial.

The fleet cannot do this work for itself: every wave collides on the file being split, so it takes a
single writer.

## Acceptance

- [ ] `board_fleet_cmd.rs` becomes a module directory whose largest file is under ~3,000 lines, with
      the split following the verb families rather than arbitrary line counts.
- [ ] Board planning, fleet configuration/state, wave execution, integration/apply, projections, the
      native coordinator tool surface and the TUI source each live in their own module.
- [ ] Adding one new fleet verb touches one module plus its dispatch arm, so two stories adding two
      different verbs have disjoint write sets.
- [ ] Failing first, a test proves the split: two representative fleet-verb stories declaring
      distinct modules are reported independent by `select_independent_batch`, where today they
      collide.
- [ ] No behaviour change. The full release gate passes, and the `flux.cli/v1` JSON for every board
      and fleet operation is byte-identical before and after.
- [ ] The story records the width measured before and after on the live board, so the claim that
      this raises the ceiling is evidence rather than an argument.

## Notes

- **Do this before the ops-CLI epic, not after.** `C-635`–`C-642` are eight stories that each add
  one verb. Split first and they are one wave; split after and they are eight sequential waves that
  each rewrite the file the next one is about to touch.
- **Areas are only as fine as the files behind them.** `areas: [flux-cli]` is an honest declaration
  today, because a story really can touch anything in that crate. Once the split lands, the stories
  should declare the module they own so the batch selector can see they are disjoint — a crate-level
  area is too coarse to be useful when one crate contains a 17k-line file.
- Child modules can read their ancestors' private items in Rust, so moving function bodies into
  submodules with `use super::*;` is mechanical; only items the parent then calls back into need
  `pub(super)`.
