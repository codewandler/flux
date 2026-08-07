---
id: C-704
title: A killed worker turn still accounts for the calls it completed
pillar: "Core"
status: backlog
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-orchestrate, flux-cli, flux-flow]
note: "usage flushes on turn end, so a turn that never emits a terminal event records nothing: wave-472-worker-9 wrote 531 insertions and recorded zero tokens. C-632 hangs usage on the turn event this failure never produces"
---

# A killed worker turn still accounts for the calls it completed

## Goal

"What did that worker cost?" is a mechanical question about data the fleet already produces, and for
a worker whose turn did not finish the answer is always **zero** — not "unknown", not "still
running", but a number indistinguishable from a worker that spent nothing.

The findings below were measured on this workspace. Two of the three turned out to belong to
existing stories, and the section after them says which; what is left, and what this story is now
scoped to, is the first one.

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
*readable*, but only in a store that reclamation is entitled to delete at any time. This half is
[C-632](C-632-every-worker-turn-records-usage-including-failed-turns.md)'s — it is exactly the
"every turn event carries usage" it asks for, and the measurement above is evidence for it.

**3. There is no read surface.** `fleet logs <worker>` ignores its target and returns wave-level
lifecycle events. `fleet inspect worker` returns assignment, capabilities and loop binding, but no
transcript. The only way to read a running worker today is to point the ordinary session tools at
the store by hand:

    flux sessions --store <repo>/.git/worktrees/<story>/flux-fleet/sessions/<worker>

That works — it is a normal flux session — but it requires knowing the layout, it only works while
the worktree exists, and the session is flagged `interrupted` while live, so entering a turn on it
would resume the worker's own killed turn. An operator surface must not be a footgun.

This half belongs to [C-599](C-599-fleet-work-is-unobservable-while-it-runs.md) (the transcript
surface) and [C-712](C-712-fleet-logs-scopes-to-the-worker-it-was-given-or-says-what-it-can-scope-to.md)
(the CLI verb that answers a question it was not asked). The evidence is kept here because it was
measured together, not because this story claims it.

## What this story is, after the neighbours are accounted for

Three existing stories already own most of the surface above, and this one must not re-litigate them:

- [C-632](C-632-every-worker-turn-records-usage-including-failed-turns.md) puts usage on
  `agent.turn.completed`/`failed` in the durable log. That is the cost record, and because
  `events.ndjson` is not inside a worktree, it survives reclamation.
- [C-599](C-599-fleet-work-is-unobservable-while-it-runs.md) stage 1 is a transcript view over the
  worker's own `<store>/events.db`, read-only, from the Workers tab. That is the read surface.
- [C-602](C-602-fleet-workers-report-activity-back-to-the-coordinator.md) streams a bounded activity
  projection to the coordinator and persists it centrally. That is what makes any of it work for a
  worker that is not on this machine.

What none of them covers is the granularity, and that is what is left for this story. **C-632 hangs
usage on a turn event, and the failure measured here is a turn that never emits one.**
`wave-472-worker-9` has `turn_started = 1`, `turn_ended = 0`: no completed event, no failed event,
nothing for C-632 to attach to, 531 insertions written and zero tokens recorded. A per-turn record
is exactly as durable as the turn, and the turns that most need accounting are the ones that die.

So: usage must be durable **below** the turn — flushed per model call as the calls complete — so
that a killed, cancelled or still-running turn still accounts for what it already spent. Everything
else in the original draft of this story belongs to C-632, C-599 or C-602 and is dropped here.

### On relocating the store

An earlier draft of this contract proposed moving worker stores out of the worktree, and that is
**refused** — C-602 already reasoned it through and reached the opposite conclusion for a good
reason:

> A worker is intended to become containerisable — Docker, k8s, a remote runner. A remote worker
> cannot append to the coordinator's SQLite file at all, so "everyone writes the shared store" is a
> design that stops working at exactly the point the isolation was for. […] The separate store is
> justified by *location independence*, not by contention.

A relocation would fix the local case and quietly design out the remote one. The durable record that
must survive reclamation is the coordinator's, reached by C-602's projection and C-632's turn usage
— not the worker's own store moved somewhere safer. The worker's store stays isolated and stays
disposable; what must not be disposable is the accounting, and that is served by flushing it out as
it is produced rather than by finding it a better home.

## Acceptance

- [ ] Usage is durable per model call, not only at turn end. A worker turn that is cancelled, crashes or is still running accounts for every call it already completed, and a killed turn is no longer indistinguishable from a free one.
- [ ] The per-call record reaches somewhere that outlives the worktree, by the mechanism C-602 establishes rather than a second one invented here. If C-602 has not landed, this story states the interim path explicitly rather than assuming a copy step that reclamation could outrun.
- [ ] `wave-472-worker-9`'s shape is covered by a test: a turn that starts, does work, and is killed without emitting a terminal turn event still accounts for the calls it completed.
- [ ] A wave-level cost view answers "what did this wave cost" from the durable record after its worktrees are gone.
- [ ] Failing first: a test drives a worker turn killed mid-flight and asserts the completed calls' usage is still readable — today it reads zero.

## Notes

Found while extracting worker usage during `wave-602`. The measurement that motivated it is the one
that could not be made: `wave-472` dispatched ten workers at stories whose work was already in
`main`, and the cost of that wave — the thing that would have made the waste legible — was never
recorded and is now unrecoverable. `board reconcile` (C-637) closed the window that let it happen;
this closes the one that made it invisible afterwards.
