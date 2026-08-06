# A fleet pauses, resumes, and recovers from shutdown and cancellation

Scope note: the run-control epic (A-140 pause a live run, A-141 what pause means for an effect in
flight, A-142 inspect a paused run) defines pausing **one** turn honestly. This epic is the fleet
scope: pause and resume the whole thing, shut it down on purpose, and recover it — from shutdown,
from crash, and from cancellation — without losing work. Single-run semantics are composed here,
never redefined.

## Why

Every need below occurred in the 2026-08-05/06 dogfood run; none is hypothetical.

- **The most repeated operator act had no primitive.** Installing a harness change requires "no
  wave in flight → stop everything → install → restart". This ran ~24 times, implemented by
  process-table scans; a `pgrep` that matched its own shell answered "idle" while three workers
  ran, which resumed a live worker, burned its budget and parked a healthy wave. Stopping the
  driver does not stop workers (separate detached processes), and three driver instances once ran
  concurrently. "Quiesce the fleet" must be a verb, not a procedure.
- **Terminal states outlive their evidence.** The prior coordinator died on a provider usage-limit
  (HTTP 429) and its `failed` status persisted across every restart, blocking the operator surface
  ("Fleet main is failed") long after the condition had passed. A usage-limit is a reason to
  *pause until reset*, not a permanent verdict.
- **Crash recovery is a human reading worktrees.** A disk-full event killed dispatch with a
  zero-byte log and no journal event; `state.json` said `working=1` for seven hours with zero live
  processes. Power loss and reboot have the same shape. Nothing reconciles persisted state against
  processes and worktrees on start.
- **Cancellation buries committed work.** 38 of 50 admitted agents ended cancelled. Waves killed
  by runtime errors held clean, complete commits (three separate waves held the same finished
  story) and were parked as failures; a human harvested them days later. A failed turn is not a
  failed delivery — and a cancelled or killed turn is not cancelled work.
- **Kill loses staged work.** A worker's staged-but-unfinalized batch dies with its process (one
  wave lost its commit exactly this way). The only alternatives today are "let it run" or "kill
  and hope" — there is no boundary-respecting stop.

## The lifecycle model

One durable, revision-guarded fleet lifecycle state, journalled with every transition and its
cause. Pause is a **recorded state, not process absence** — today the two are indistinguishable.

```
running ──pause──▶ draining ──all at boundary──▶ paused ──resume──▶ resuming ──▶ running
   │                                               ▲                       (re-admit from
   └──shutdown──▶ draining(deadline) ──▶ stopped ──┘── start                admitted bindings)
                                            │
                            start with divergent ground truth
                                            ▼
                                        recovering ──reconciled──▶ paused | running
```

- **draining**: no new admissions, dispatches or spawns anywhere (coordinator, driver, ad-hoc);
  running turns continue to their next safe boundary — a C-570 cooperative yield, a checkpoint
  edge, or turn end — then hold.
- **paused**: no fleet processes; all state durable; worktrees intact; cause recorded (operator,
  usage-limit, disk floor, decision pending). A paused fleet consumes no model budget and no new
  disk, and its ticks are report-only.
- **recovering**: entered automatically when `fleet start` finds persisted state disagreeing with
  ground truth; ends in `paused` (default) or `running` (`--resume`).

## Approach

**`fleet pause [--now]`.** The durable pause flag freezes admission and dispatch immediately;
draining workers reach their boundary and hold. `--now` composes the run-control pause (A-140)
over every live turn, with A-141's honesty contract intact: an effect already in flight cannot be
un-sent, so the fleet reports *pausing* — per agent, naming what is still moving — and only then
*paused*. Staged batches are persisted, never dropped. A pause that reports stopped while effects
continue is worse than no pause.

**`fleet resume`.** The inverse, agent by agent: re-admission reuses the admitted loop binding,
digest-verified (C-627), never replays the coordinator inbox (C-618), and carries budgets and
counters over (reservations settle at pause and re-reserve at resume, per C-571). Before any agent
is resumed, its worktree ground truth is checked: an assignment already satisfied on disk — clean
worktree, commit ahead of base — goes to handoff, not back to a model. Harvest-before-resume is
the R-10 driver rule, made native.

**`fleet shutdown [--deadline S] [--now]`.** Drain with a deadline, stop every process, journal a
shutdown record listing what was preserved: per agent, the worktree state (committed / dirty /
clean), staged-batch disposition, and session store location. Uncommitted worktrees are always
preserved and always listed — a shutdown that hides unfinished work is a data-loss event with
better manners.

**Recovery on start.** A reconciliation pass over four sources of truth — journal, live
processes, worktrees, session stores — that is idempotent and safe to re-run:

- Orphan processes (running with no journal claim, or claimed by another store) are adopted or
  terminated, never double-counted.
- An agent persisted as `working` with no process is re-derived from its worktree: commit ahead of
  base → handoff-eligible; dirty → resumable, with a salvage note; clean at base → cancellable.
- Stale terminal statuses are re-evaluated against their recorded cause — a usage-limit failure
  whose reset time has passed becomes resumable, not inherited as `failed`.
- Every divergence found is journalled as its own event; recovery that silently repairs is
  indistinguishable from corruption.

**Cancel, at every scope, with a harvest report.** Cancelling a turn, an agent, a wave or the
fleet is bounded and observable (C-601's contract, widened), always preserves worktrees, and ends
with a harvest report: committed work found inside the cancelled scope, listed and left
handoff-eligible rather than buried under the cancellation. Reaping stays what it is today — a
separate act, permitted only when every worktree is clean at its pinned base.

**Condition-scoped pauses.** Provider usage-limit exhaustion and resource floors (disk headroom,
C-617) pause the fleet with that cause and auto-resume when the condition provably clears (reset
time reached, space reclaimed) — bounded by an operator knob, and always journalled. The 429 that
ended a coordinator session should have been a pause with a timestamp, not a persistent verdict.

## Invariants

1. **The worktree is the ground truth of unfinished work.** No lifecycle transition deletes,
   hides, or overwrites one. Committed work survives pause, shutdown, crash and cancel; harvest is
   checked before every destructive decision.
2. **Nothing is reported that is not true.** `paused`/`stopped` only after every effect is
   accounted for, per A-141.
3. **Terminal states never outlive their evidence across a restart.** Causes are recorded so they
   can be re-evaluated.
4. **Lifecycle transitions are revision-guarded journal writes; reads never mutate.** A status
   question must not be able to destroy a wave again (C-598).
5. **One lifecycle authority per store.** A second `start`, driver or writer refuses loudly.
6. **A paused fleet is free.** No model calls, no new disk, no budget burn.

## Relationship to existing work

Builds on: A-140/A-141/A-142 (single-run pause mechanics, honesty, inspection) · C-570
(cooperative yield = the drain boundary) · C-571 (reservations settle/re-reserve across
pause/resume) · C-601 (cancellation bounds and visibility) · C-618 (resume without inbox replay) ·
C-627 (binding digest verification at re-admission) · C-633 (a killed worker's session store as
recovery evidence) · C-617 (disk floor as a pause cause).

Subsumes, once native: the roadmap driver's interim rules R-10 (harvest before resume/park), R-13
(single instance, counter hygiene), and the install choreography's process-scan quiescence test.
C-631 (`fleet drive`) calls these verbs instead of reimplementing them. A-128's A2A-era monitor
journey is superseded in scope by the native fleet surface.

## Stories

Coarse boundaries, to be cut when the epic is scheduled — deliberately not filed yet:

- A durable fleet lifecycle state with cause, honoured by every admission, dispatch and spawn path.
- `fleet pause` drains to safe boundaries; `--now` composes the run-control pause over every live
  turn with honest effect accounting.
- `fleet resume` re-admits from digest-verified bindings and harvests satisfied assignments
  instead of re-running them.
- `fleet shutdown` drains against a deadline and journals exactly what it preserved.
- Startup reconciliation across journal, processes, worktrees and session stores — idempotent,
  every divergence journalled, stale terminal states re-derived from their cause.
- Cancel at any scope ends with a harvest report and preserved worktrees.
- Condition-scoped pauses (usage-limit, disk floor) with provable auto-resume.
- The lifecycle state is first-class on the operator surface: a paused fleet cannot read as a
  crashed one (a dead pane once did).

## Non-goals

- No scheduler redesign: draining honours existing wave and dependency semantics.
- No cross-machine migration of a paused fleet (relocatable state is C-604's track).
- No force-kill of long-running effects under `--now`: honesty over force, per A-141.
