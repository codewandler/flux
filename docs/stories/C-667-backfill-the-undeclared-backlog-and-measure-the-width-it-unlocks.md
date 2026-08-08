---
id: C-667
title: "Backfill the undeclared backlog and measure the width it unlocks"
pillar: "Core"
status: backlog
priority: 13
epic: scouting-makes-the-backlog-schedulable
areas: [docs]
depends_on: [C-666]
note: "measure the undeclared tail becoming batchable, NOT the workspace board's width of 3 — that one is C-657's to move"
---

# Backfill the undeclared backlog and measure the width it unlocks

## Goal

Clear the existing scouting debt, and prove the epic did what it claimed.

`C-665` makes the debt visible and `C-666` builds the thing that pays it down, but neither one
touches the 711 stories that are already `ready` with no declared write set. Those never cross a
transition again, so no gate reaches them. They need one deliberate sweep.

The sweep is the same scout at higher width, run until the debt stops falling — not new machinery.

**This story exists mainly to carry the measurement — and to measure the right thing.** Two boards
give different answers:

| board | width at `--limit 8 --independent` | pool | held back by |
|---|---|---|---|
| workspace | **3** | 13 | `shared area flux-cli` ×7 — **none** undeclared |
| repository | **8** | 101 | — |

The workspace board is capped by `C-657`, not by scouting: every item in the active program already
declares its areas. **So this story must not be judged on moving 3.** What it measures is the
undeclared tail: how many of the ~711 become batchable once scouted, and what they collide on when
they do.

## Acceptance

- [ ] The scouting debt reported by `board next` reaches zero, or every remaining item names a
      concrete reason it could not be scouted.
- [ ] The count of previously-undeclared items that become batchable is recorded **before and after**,
      from the live board, with the exact command and its output — evidence, not assertion. The
      workspace board's width is recorded too, and explicitly **not** claimed as this story's result.
- [ ] Held-back reasons are re-counted after the sweep, so it is visible whether items moved from
      `undeclared write set` to a genuine collision, or became schedulable.
- [ ] A sample of scouted items is checked by hand against the code they name. A scout that declares
      confident, wrong areas is worse than one that declares none, because a wrong area produces a
      wave that integration refuses after the work is done.
- [ ] The sweep's cost is recorded — items scouted, tokens, wall-clock — so the cost of keeping the
      board scouted is known rather than guessed.

## Notes

- **The honest failure mode.** If scouted items simply move from `undeclared write set` to
  `shared area flux-cli`, then scouting bought visibility but no width, and the real ceiling is
  `C-657` — one 17k-line file that half the backlog writes. Report that outcome plainly if it
  happens; it is a useful result, not a failed story.
- Crate-level areas are too coarse when one crate contains a file every fleet story touches. Expect
  the honest answer to be "batchable, but only against work outside flux-cli" until `C-657` lands.
- Run the sweep against a copy first, or with `--dry-run`, before it writes to 711 real stories.
