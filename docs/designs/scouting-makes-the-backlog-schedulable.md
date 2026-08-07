# Design — Scouting makes the backlog schedulable

## Why

The fleet can only run wide over work whose shape it knows, and it does not know that shape.

Of **1185 stories, 471 declare `areas`.** `wave_conflict` reads an empty `areas` as
`"undeclared write set"` and holds the item out of every independent batch, so the remaining ~711 are
unschedulable at width — not because they collide, but because nobody said what they touch. Nothing
records which other repositories an item reaches, what kind of work it is, or who should do it: there
are exactly two agent templates and every story goes to the same one.

An item's write set can also only be declared once. `flux board create` takes `--area`; `flux board
update` does not. So the 711 cannot be fixed by any existing verb, which is why this needs a
mechanism rather than an afternoon of editing.

### What this does **not** fix, measured

The two boards give different answers and it matters which one is quoted:

| board | width at `--limit 8 --independent` | pool | why held back |
|---|---|---|---|
| workspace (what the fleet schedules from) | **3** | 13 | `shared area flux-cli` ×7 — **none** undeclared |
| repository | **8** | 101 | — |

So the binding constraint on scheduling *today* is **`C-657`**, not scouting: seven of the ten
held-back items collide on one 17k-line file, and every item in the active program already declares
its areas. Scouting will not move the width from 3.

Scouting is for the **long tail** — the 711 undeclared stories that are not yet in an active program
lane and would each be held back the moment they entered one. It is a precondition for widening later,
not the fix for the width today. Anyone reading this design as "the scout unblocks the fleet" has read
it wrong, and the table above is here so that misreading is hard.

## Approach

A **scout**: a cheap, repeated, read-only pass that inspects a ticket and answers *what does this
touch, what discipline does it need, and who should do it* — plus a rule that an unscouted item is
not schedulable.

The scheduling half needs nothing new. `crates/flux-channels/src/adapters/schedule.rs` is a real cron
timer, `TriggerDecl` binds an event label to a journey (optionally as a named agent), `flux app run`
serves declared channels, and `cron_tick_runs_journey_via_app` already proves a tick wakes an app and
runs its journey. So the scout is a `.flux` program on a schedule channel, modelled on
`examples/channels-app.flux`, calling board operations the way `.flux/fleet/loops/main-coordinator.flux`
already does.

Four commitments shape the rest:

- **Frontmatter, not a sidecar.** `areas` and `depends_on` stay where `select_independent_batch`
  already reads them. A second home means a second copy of the collision logic, and the two will
  disagree.
- **Debt is visible, so a stopped scout is loud.** Staleness is derived from a body digest and from
  whether declared paths still exist; unscouted-plus-stale is reported by `board next` and
  `fleet status`. The failure mode this design most wants to avoid is the one that already cost a
  wave: something stops, and nothing says so.
- **The scout observes; the verb writes.** `capabilities = ["read"]`, no `edit`, no `git`, no
  `shell`. It returns structured JSON and a board operation applies it under validation.
- **Personas are proposed, never granted.** An `agent_templates` entry carries capabilities, fences
  and a model — an authority grant. A read-only scout that could mint one could grant itself
  anything. Proposals go through decision records, which already have proposed/accepted/superseded
  semantics, already surface in `fleet status`, and already respect `decision_mode = "human"`.

Discipline, `task_kind` and persona stay distinct: discipline describes the work, `task_kind` selects
the loop, the persona selects the agent. A `discipline → task_kind` map in `fleet.toml`, shaped like
the existing `[loop_policy]`, joins them without fusing them — which is what lets a new persona be
requested without touching a loop.

**The number to hold this to** is the width available on the *repository* board's undeclared tail —
how many of the 711 become batchable once scouted — not the workspace board's current 3, which is
`C-657`'s to move. Measuring the wrong one would let this epic take credit for a fix it did not make,
or be judged a failure for not making one it never claimed.

## Stories

- `C-665` — the board records what a scout saw, and what it still owes
- `C-666` — a scheduled scout app inspects tickets on a timer
- `C-667` — backfill the undeclared backlog and measure the width it unlocks
- `C-668` — an unscouted item cannot become ready
- `C-669` — a scout proposes a persona as a decision, never grants one

Order matters: the gate (`C-668`) comes after the backfill (`C-667`), because turning it on while the
debt is outstanding would freeze the backlog behind the thing that clears it.
