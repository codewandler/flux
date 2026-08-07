---
id: C-713
title: "Concurrent story workers do not serialize on one cargo package cache"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli, flux-orchestrate]
note: "wave-602: a cargo_test took 126s wall against 25s of work, its log full of 'Blocking waiting for file lock on package cache' while the sibling worker built"
---

# Concurrent story workers do not serialize on one cargo package cache

## Goal

Fleet isolates workers by worktree and by target directory, but every worker still shares one
`CARGO_HOME` package cache, and cargo takes a global lock on it. So the isolation that makes
parallel workers safe does not make them parallel: the moment two workers reach a build at the same
time, one waits.

Measured on `wave-602` with `max_workers` well above 2 and only two workers live:

    step_failed  step_cargo_test_c8cda6068ef1aa61
      Blocking waiting for file lock on package cache
      Blocking waiting for file lock on package cache
      Blocking waiting for file lock on package cache
      Blocking waiting for file lock on package cache
       Compiling flux-tui v0.58.0 (…/wave-602/flux/stories/C-543)

That invocation recorded **126.4 s** of wall time. The compile it was blocking on was the sibling
worker's. Two other invocations in the same wave recorded 25.0 s and 61.0 s with the same banner.
This is dead time inside a worker's model-round budget — the worker is not thinking, it is queued —
and it scales the wrong way: the more parallelism the disk-width calculation grants, the more of
each worker's budget goes to waiting.

The cost is worse than the seconds suggest, because the wait is charged against the thing the fleet
is actually short of. A worker's ceiling is model rounds
([C-603](C-603-implementation-loop-checkpoints-against-its-goal.md)), and a blocked `cargo_test` is
one round that bought nothing.

## Acceptance

- [ ] Concurrent story workers do not serialize on a shared cargo package cache. Whether that is per-worker `CARGO_HOME`, a pre-warmed read-only registry, or vendoring is the story's to decide and record.
- [ ] The chosen approach does not multiply disk by the number of workers without saying so — `fleet`'s disk-width calculation accounts for whatever per-worker cost the fix introduces, so headroom checks stay honest.
- [ ] Registry contention is distinguishable from a real build failure in the recorded evidence, so `Blocking waiting for file lock` never reads as a test result.
- [ ] Failing first: a test runs two fenced workers through a typed cargo operation concurrently and asserts neither blocks on the other's package cache.

## Notes

Found by reading `wave-602`'s worker transcripts. Adjacent to
[C-628](C-628-typed-cargo-operations-run-offline-by-default-in-fenced-worktrees.md), which is about
typed cargo operations reaching for the *network* in a fenced worktree; this is about them reaching
for the same *lock*. C-628's "pre-warm or vendor the registry host-side" is a plausible shared
mechanism for both, and whichever lands first should be built with the other in mind.
