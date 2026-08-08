# Design — Recovery and inspection have no CLI, so every failure is hand-driven

## Why

The happy path has verbs. `fleet run`, `handoff`, `integrate`, `apply`, `reclaim` each do one thing well.
The moment anything goes wrong, the operator falls off a cliff into `git`, `jq` and `/proc`.

This is not hypothetical. Harvesting six delivered stories in one evening required, by hand:

- editing a driver text file to unpark a wave
- `git worktree add` to rebuild an integration worktree reclamation had removed
- `git reset --hard <base>` on integration worktrees, repeatedly, before handoffs would verify
- reading `state.json` to find which worker owned a story, for every handoff
- deriving each handoff's write set from `git diff base..HEAD`
- a Python expression to pull a gate's failure text out of
  `topology.repositories[].gate.evidence.stdout`
- scanning `/proc` to tell a live worker from a dead one
- Python over 686 local branches to find which 40 were empty scaffolding
- reading state to notice four separate waves each holding an attempt at the same story
- nine `board start` + `board done --override-reason` pairs for stories whose implementation was already
  in `main`

Every one is a mechanical question with a single correct answer, asked of data the fleet already owns. Two
caused real loss rather than friction:

1. Nothing reported that a story's work was already committed while its status still read `ready`, so the
   coordinator dispatched a worker to implement it again. A full worker turn reproduced code committed
   hours earlier.
2. Nothing reported a worker recorded `working` with no process behind it, so a wave sat undispatchable and
   the driver grew its own `/proc` scanner — reimplementing, badly, a fact the supervisor knew and did not
   write down.

The pattern is consistent: **the fleet records enough to answer these questions and offers no way to ask
them.** So an operator reaches for `jq`, and an autonomous driver grows a shell script — which is how a
driver ends up keeping a park list in a text file and a liveness heuristic that matches its own command
line.

## Approach

Three groups, in the order they earn their keep.

### Inspection: make the recorded truth askable

Nothing here changes state; each replaces a `jq` expression an operator currently writes under pressure.

- **`fleet doctor`** already exists for configuration. Extend it to the runtime: agents recorded active
  whose supervisor is gone, waves wedged in a transient state, worktrees a topology names that are absent
  from disk, items claimed by more than one live wave, branches holding nothing unique.
- **`fleet inspect gate <wave> [--repository]`** prints a gate's own output, tail first. A failing gate's
  reason is the single most-wanted fact in the system and currently requires knowing the shape of
  `state.json`.
- **`board reconcile`** reports stories whose implementation is present while their status says the work is
  outstanding. Detection is the whole value: the fix is a transition anyone can make once they know.

### Recovery: make the repair a verb

- **`fleet repair <wave>`** recreates structure the topology names and disk lacks — a removed worktree, one
  left off its pinned base — and refuses anything that would discard work.
- **`fleet park <wave> --reason` / `unpark`** make parking a lifecycle state with a recorded reason. Today
  it is a line in a driver-owned file, invisible to `fleet status`, which is why a parked wave could be
  re-decided every minute and why unparking meant editing text.
- **`fleet land <wave>`** merges accepted candidates into the canonical branch, **re-gating against
  whatever that branch has become**. Acceptance pins a candidate against the base it was gated on; landing
  is a separate act with its own verification, and it is currently a bash approximation in the driver.

### Ergonomics: stop asking for what can be derived

- **`fleet handoff --from-worktree`** derives the write set from `base..HEAD` and defaults the worker to the
  wave's owner for that item. Both are recorded; requiring them by hand invites a wrong answer, and a wrong
  write set is not a typo — it is false evidence.
- **`fleet quiesce` / `resume`** stop dispatch and confirm nothing is in flight, so installing a new binary
  cannot race one. Doing this by hand went wrong twice in one evening, once corrupting a full workspace
  test run.

## What this is not

Not a general admin console, and not an excuse to let the happy path stay fragile. Every verb here exists
because a specific failure had no answer, and each should be deletable if that failure becomes impossible.

## Stories

- `fleet doctor` reports runtime health: dead-supervisor agents, wedged waves, missing worktrees,
  double-claimed items, branches holding nothing unique.
- `fleet inspect gate` prints a repository gate's own output for a wave, tail first.
- `board reconcile` reports stories whose work is already present while their status says otherwise.
- `fleet repair` rebuilds structure a topology names and disk lacks, refusing anything that discards work.
- `fleet park`/`unpark` make parking a recorded lifecycle state with a reason.
- `fleet land` merges accepted candidates into the canonical branch and re-gates against it.
- `fleet handoff --from-worktree` derives the write set and the owning worker.
- `fleet quiesce`/`resume` make installing a new binary safe by construction.
