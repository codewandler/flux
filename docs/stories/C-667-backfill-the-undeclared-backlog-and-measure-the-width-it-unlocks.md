---
id: C-667
title: "Backfill the undeclared backlog and measure the width it unlocks"
pillar: "Core"
status: backlog
priority: 13
epic: scouting-makes-the-backlog-schedulable
areas: [docs]
depends_on: [C-666]
note: "board next --limit 8 --independent returns 3 today; if scouting the 711 does not move that number, the epic did not work"
---

# Backfill the undeclared backlog and measure the width it unlocks

## Goal

Clear the existing scouting debt, and prove the epic did what it claimed.

`C-665` makes the debt visible and `C-666` builds the thing that pays it down, but neither one
touches the 711 stories that are already `ready` with no declared write set. Those never cross a
transition again, so no gate reaches them. They need one deliberate sweep.

The sweep is the same scout at higher width, run until the debt stops falling — not new machinery.

**This story exists mainly to carry the measurement.** Every claim in this epic reduces to one
number: how many mutually independent stories the board can offer at once. Today
`flux board next --limit 8 --independent` returns **3**, and holds back eleven, ten of them for
`shared area flux-cli`. If that number does not move after the backfill, the diagnosis was wrong and
the epic should be reconsidered rather than extended.

## Acceptance

- [ ] The scouting debt reported by `board next` reaches zero, or every remaining item names a
      concrete reason it could not be scouted.
- [ ] The achievable independent width is recorded **before and after**, from the live board, with
      the exact command and its output — evidence, not assertion.
- [ ] Held-back reasons are re-counted after the sweep, so it is visible whether items moved from
      `undeclared write set` to a genuine collision, or became schedulable.
- [ ] A sample of scouted items is checked by hand against the code they name. A scout that declares
      confident, wrong areas is worse than one that declares none, because a wrong area produces a
      wave that integration refuses after the work is done.
- [ ] The sweep's cost is recorded — items scouted, tokens, wall-clock — so the cost of keeping the
      board scouted is known rather than guessed.

## Notes

- **The honest failure mode.** If width stays at 3 after the backfill, the ceiling was never
  `areas` — it is `crates/flux-cli/src/board_fleet_cmd.rs` being one 17k-line file that half the
  backlog writes. That is `C-657`, and this measurement is the thing that tells the two apart.
- Expect the number to be limited by `C-657` regardless: crate-level areas are too coarse when one
  crate contains a file that every fleet story touches. Report both the width and the reason.
- Run the sweep against a copy first, or with `--dry-run`, before it writes to 711 real stories.
