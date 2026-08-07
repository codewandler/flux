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
| `wave-602-worker-1` | 1 | 0 | **0** |
| `wave-602-worker-2` | 1 | 0 | **0** |
| `wave-472-worker-9` | 1 | 0 | **0** |
| `~/.flux` (CLI/TUI) | 2266 | 1913 | 6509 |

Every store with `turn_ended > 0` has usage; every store with `turn_ended = 0` has none. This is not
a fleet-versus-CLI split — the coordinator is a fleet agent and records fine. `wave-472-worker-9`
wrote **531 insertions** across three files and recorded **zero tokens**, because its turn died
before it ended.

**2. `fleet reclaim` deletes the record.** Worker stores live at
`<repo>/.git/worktrees/<story>/flux-fleet/sessions/<worker>/events.db`. Reclamation removes the
worktree and git prunes that directory with it — `C-569`, `C-618`, `C-635` and `C-637` are all
confirmed gone. So the disk-recovery operation destroys the cost record as a side effect, and
telemetry survives only for waves that both finished their turns *and* have not been reclaimed.
Across the fleet's whole durable log this leaves **41 of 235 turns** carrying usage: 17%.

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

## Acceptance

- [ ] Usage is durable per model call, not only at turn end. A worker turn that is cancelled, crashes or is still running has the usage of every call it already completed, and a killed turn is no longer indistinguishable from a free one.
- [ ] A worker's usage aggregate is copied into the fleet's own durable record before reclamation can remove the worktree that holds its store, so `reclaim` never destroys cost data. Reclaim's contract is unchanged otherwise — it still refuses a worktree holding uncommitted work.
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
