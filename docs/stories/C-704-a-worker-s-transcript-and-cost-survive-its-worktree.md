---
id: C-704
title: "A worker's transcript and cost survive its worktree"
pillar: "Core"
status: backlog
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-orchestrate, flux-cli, flux-flow]
note: "usage flushes on turn end and the store lives inside the worktree, so an unfinished turn records nothing and `fleet reclaim` deletes what did get recorded; there is no read surface for either"
---

# A worker's transcript and cost survive its worktree

## Goal

"What did that worker do, and what did it cost?" is a mechanical question about data the fleet
already produces, and today it is unanswerable for almost every worker that has ever run. Three
findings, measured on this workspace, and they share one root: **worker observability lives inside a
disposable worktree and has no read surface.**

**1. Usage is flushed on turn end, so an unfinished turn records nothing.** The correlation is exact
across every surviving session store:

| store | `turn_started` | `turn_ended` | `call_usage` |
|---|---|---|---|
| `main` (coordinator) | 85 | 83 | 283 |
| `diagnostic` | 1 | 1 | 1 |
| `wave-472-worker-9` | 1 | 0 | **0** |
| `~/.flux` (CLI/TUI) | 2266 | 1913 | 6509 |

Every store with `turn_ended > 0` has usage; every store with `turn_ended = 0` has none. This is not
a fleet-versus-CLI split — the coordinator is a fleet agent and records fine. `wave-472-worker-9`
wrote **531 insertions** across three files and recorded **zero tokens**, because its turn died
before it ended.

`wave-602` then ran the experiment on purpose. Its two workers were measured mid-flight and again
after their turns ended, with nothing else changed — same machine, same hour, same stores:

| store | mid-flight (`turn_ended` = 0) | after the turn ended | recorded on the second reading |
|---|---|---|---|
| `wave-602-worker-1` (C-543) | 0 calls | 165 calls | 15.61M billed in · 138.2k out |
| `wave-602-worker-2` (C-631) | 0 calls | 116 calls | 11.61M billed in · 102.0k out |
| `wave-472-worker-9` (C-621) | 0 calls | *turn never ended* | **still nothing** |

281 model calls and 28.06M billed input tokens existed the whole time and were unreadable until the
turns closed. This is the load-bearing point: a store reading zero does not mean a cheap worker, it
means an unfinished one, and today those two are indistinguishable. The control is in the same
table — `wave-472-worker-9`'s turn never ended, so its cost is not merely unreadable but gone.

**2. The durable log never receives the usage, and `fleet reclaim` deletes the copy that has it.**
Worker stores live at `<repo>/.git/worktrees/<story>/flux-fleet/sessions/<worker>/events.db`.
Reclamation removes the worktree and git prunes that directory with it — `C-569`, `C-618`, `C-635`
and `C-637` are all confirmed gone. So the disk-recovery operation destroys the cost record as a
side effect. Across the fleet's durable log, **41 of 112** completed-or-failed turns carry usage: 37%.

`wave-602` shows this is not only a reclamation problem. Both its turns ended cleanly and their
stores hold 281 model calls and 28.06M billed input tokens — and the durable log has **no
`agent.turn.completed` for either worker at all**. It recorded only the wave-level roll-up:

    wave.agent-turns.delivered   agent=None  usage=absent
    wave.agent-turns.completed   agent=None  usage=absent

So the aggregate never leaves the worktree even on the success path. Ending the turn makes the cost
*readable*, but only in a store that reclamation is entitled to delete at any time; nothing copies it
anywhere that outlives the worktree. That is why the acceptance below asks for the copy to happen
before reclaim can run, and not merely for reclaim to be more careful.

**3. There is no read surface.** `fleet logs <worker>` ignores its target and returns wave-level
lifecycle events. `fleet inspect worker` returns assignment, capabilities and loop binding, but no
transcript. The only way to read a running worker today is to point the ordinary session tools at
the store by hand:

    flux sessions --store <repo>/.git/worktrees/<story>/flux-fleet/sessions/<worker>

That works — it is a normal flux session — but it requires knowing the layout, it only works while
the worktree exists, and the session is flagged `interrupted` while live, so entering a turn on it
would resume the worker's own killed turn. An operator surface must not be a footgun.

This is the same shape as the rest of this epic: recorded truth that only a hand-written expression
can reach. C-637 made "is this already built?" askable; this makes "what did this worker do, and
what did it cost?" askable.

## Where the store should live

**Relocate it; do not copy it out.** An earlier draft of this contract asked for the usage aggregate
to be copied into the durable record before reclamation. That is weaker than it looks: a copy step
has a window, and the failure this story exists to fix — a turn killed before it flushed — is
precisely a failure that lands inside such windows. If the store never lives in the worktree, there
is no copy to miss and no window to lose it in.

**Key it by `<wave>/<worker>`, not by pid.** A pid is recycled by the OS, means nothing once the
process exits, and the entire purpose here is reading the record *after* the worker is gone; it
would also need a pid→wave map, which is one more thing that can be lost. `<wave>/<worker>` is
already the identity `events.ndjson`, `state.json` and `fleet inspect worker` all use, which makes
"what did wave-602 cost" a directory listing rather than a join.

Two candidate roots, and the trade is real:

| root | for | against |
|---|---|---|
| `.flux/fleet/sessions/<wave>/<worker>/` | sits beside the `events.ndjson` and `state.json` that already key by wave and worker; `reclaim` only ever removes `worktrees/`, so it is already outside the blast radius | not on the default `flux sessions` search path, so the tooling must be pointed at it |
| `~/.flux/fleet/<workspace>/<wave>/<worker>/` | `flux sessions`/`replay`/`diff` already default to `~/.flux`, so they would find worker stores with no `--store` flag; survives deleting the workspace | must be keyed by workspace or two fleets collide, and it mixes worker sessions into the operator's own session list (already 2,266 turns) unless that list learns to filter |

The recommendation is the workspace-local root, because telemetry that outlives the fleet state
describing it is not much more useful than telemetry that was deleted — but the second row is a
legitimate choice if the no-flag `flux sessions` ergonomics are judged to matter more.

Whichever root is chosen, the store path must sit **outside the worker's own read fence**. Today
each store is inside the worktree the worker can read, and confinement is incidental to the layout;
under a shared parent it has to be deliberate, or one worker can read a sibling's transcript.

## Acceptance

- [ ] Usage is durable per model call, not only at turn end. A worker turn that is cancelled, crashes or is still running has the usage of every call it already completed, and a killed turn is no longer indistinguishable from a free one.
- [ ] A worker's session store does not live inside the worktree at all. It is keyed by the identity the fleet already uses everywhere else — `<wave>/<worker>` — so reclamation has nothing to destroy and no copy step can be missed. Reclaim's contract is unchanged otherwise: it still refuses a worktree holding uncommitted work.
- [ ] `flux fleet inspect worker <id>` exposes the recorded transcript for that worker — the plan/observation/run sequence — honoring the view's `--limit` and structural byte budget the way `inspect gate` does, tail-first where that is what an operator wants. `--output json` is the automation API.
- [ ] `flux fleet logs <worker>` either scopes to the named worker or refuses with a message naming what it can scope to. Silently returning wave-level events for a worker target is worse than an error, because it reads as "this worker produced nothing".
- [ ] Reading a live worker is safe by construction: an inspection path never enters a turn on the worker's session, and the documented operator command cannot resurrect the worker's interrupted turn.
- [ ] A wave-level cost view answers "what did this wave cost" from the durable record after its worktrees are gone.
- [ ] Failing first: a test drives a worker turn that is killed mid-flight and asserts the completed calls' usage is still readable, and a second asserts a reclaimed wave's cost survives the reclamation.

## Notes

Found while extracting worker usage during `wave-602`. The measurement that motivated it is the one
that could not be made: `wave-472` dispatched ten workers at stories whose work was already in
`main`, and the cost of that wave — the thing that would have made the waste legible — was never
recorded and is now unrecoverable. `board reconcile` (C-637) closed the window that let it happen;
this closes the one that made it invisible afterwards.
