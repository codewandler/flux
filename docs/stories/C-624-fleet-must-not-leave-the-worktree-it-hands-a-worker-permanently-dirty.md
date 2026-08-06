---
id: C-624
title: "Fleet must not leave the worktree it hands a worker permanently dirty"
pillar: "Core"
status: ready
priority: 5
epic: fleet-harness-throughput
areas: [flux-cli]
---

# Fleet must not leave the worktree it hands a worker permanently dirty

## Goal

Hand a worker a clean worktree. Fleet snapshots its resolved loop binding *into* the worktree it is about
to hand over, so in any repository whose ignore rules do not already cover `.flux/`, the worker starts —
and stays — with an uncommitted change it did not make.

## Acceptance

- [ ] A freshly prepared story worktree reports no uncommitted changes, in every configured repository.
- [ ] The loop-binding snapshot stays readable by the worker at the path its `--loop` argument names — the
      workspace guard confines reads to the worktree, so it cannot simply move outside.
- [ ] Handoff and reclamation decisions are unaffected by harness-owned paths, whichever repository the
      story belongs to.
- [ ] Failing first: a test prepares a worktree in a repository that does not ignore `.flux/` and asserts
      `git status --porcelain` is empty.

## Notes

**Found by a wave stalling, not by inspection.** `snapshot_fleet_loop_binding` writes
`.flux/fleet/agent-loop.flux` and `.flux/fleet/agent-loop-binding.json` inside the story worktree.
`flux/.gitignore` happens to carry `.flux/*`, so those files are invisible there — but flux-exchange has
no such rule, so `exchange/X-138`'s worktree reported:

```
?? .flux/
```

That single untracked entry is enough to break the pipeline, because "is this worktree clean?" is a
load-bearing question asked in two places:

- a handoff is only recorded for a worktree with no uncommitted changes — so the worker committed real
  work and its handoff was silently skipped;
- reclamation treats any dirt as work that exists nowhere else, so the worktree is retained forever and
  the wave is parked for a human.

The result is a worker that did its job correctly, committed, and was then treated as unfinished — in one
repository but not another, purely because of an unrelated `.gitignore` line.

**Why this is Fleet's bug and not the repository's.** Requiring every managed repository to ignore
`.flux/` makes Fleet's internals a precondition of participating in a fleet, and silently changes
behaviour for anyone who does not. Fleet controls both the write and the cleanliness test, so it should
either keep its artifacts out of the tracked tree or exclude them explicitly — for example through the
worktree's own exclude file at preparation time, which touches nothing the repository tracks.

**Interim mitigation, outside the binary.** The autopilot's cleanliness tests now exclude the harness's
own `.flux/` entry, so handoff and reaping work. That unblocks the pipeline but leaves the underlying
condition: an operator running `git status` in a worker worktree still sees a stray untracked directory,
and any other consumer of "is it clean?" is still wrong.

- Related: [C-617](C-617-fleet-reclaims-wave-storage-and-refuses-to-dispatch-without-disk-headroom.md) —
  reclamation asks the same cleanliness question and is misled by the same artifact.
- Related: [C-616](C-616-a-story-worker-authors-its-own-handoff-instead-of-a-third-party-transcribing-it.md)
  — a worker-authored handoff would report its own write set and not depend on inferring cleanliness from
  git at all.
