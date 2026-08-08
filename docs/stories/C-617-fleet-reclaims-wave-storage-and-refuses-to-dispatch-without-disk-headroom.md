---
id: C-617
title: "Fleet reclaims wave storage and refuses to dispatch without disk headroom"
pillar: "Core"
status: ready
priority: 8
epic: fleet-harness-throughput
areas: [flux-cli]
---

# Fleet reclaims wave storage and refuses to dispatch without disk headroom

## Goal

Stop Fleet from filling the host disk. Every wave creates worktrees, every worker and every final gate
builds in them, and nothing is ever reclaimed — so throughput has a hard, silent stop.

## Acceptance

- [ ] Wave worktrees share one build-artifact directory per repository rather than one per worktree.
- [ ] A terminal wave's worktrees are reclaimable by an explicit command, which refuses to remove a
      worktree holding uncommitted changes or commits not reachable from the canonical ref unless
      forced.
- [ ] Wave preparation refuses up front, with a named required amount, when free space is below a
      declared floor — instead of failing partway through a write.
- [ ] A dispatch that fails on `ENOSPC` reports that as the reason. Today it produces an empty log and
      a bare exit 1.
- [ ] Failing first: a test proves preparation refuses on insufficient headroom, and that reclamation
      declines a worktree with unmerged work.

## Notes

**Measured, not projected.** 24 waves in one session accumulated **66 GB** in
`.flux/fleet/worktrees` and took an 848 GB volume to **0 bytes free**, which broke the next dispatch.
Distribution:

| path | size |
| --- | --- |
| `wave-302/flux/integration/target` | 27 GB |
| `wave-286/flux/stories/C-562/target` | 8.4 GB |
| `wave-281/flux/stories/C-562/target` | 6.2 GB |
| `wave-302/flux/stories/C-569/target` | 6.0 GB |
| `wave-257/flux/stories/C-562/target` | 5.7 GB |
| `wave-308/flux/stories/C-562/target` | 4.1 GB |
| `wave-286/flux/integration/target` | 3.7 GB |
| `wave-299/flux/stories/C-569/target` | 2.2 GB |
| `wave-302/flux/integration/plugins/target` | 1.2 GB |
| `wave-302/flux/integration/website/node_modules` | 790 MB |

**65 of the 66 GB was `target/` and `node_modules`** — regenerable artifacts, no work product. Deleting
only those recovered 136 GB of headroom and left every one of the nine work-bearing worktrees intact
(verified: 4 dirty worktrees kept their files, 5 kept their commits, and both candidate commits still
resolve). Source and commits are not the problem; duplicated build trees are.

**Why sharing the artifact directory is the primary fix.** The integration worktree alone built 27 GB
because the configured final gate is a full release gate. Each concurrent story worker builds the same
workspace independently. `max_workers = 5` plus an integration worktree means six full builds of one
Rust workspace per wave, none shared. A per-repository `CARGO_TARGET_DIR` collapses that to one, and is
a smaller change than any pruning policy.

**The failure mode is worse than the disk cost.** `flux fleet run` exited 1 with a **zero-byte log** and
no Fleet event; the only evidence of `ENOSPC` was an unrelated shell write error. A coordinator or an
operator sees "dispatch failed" with nothing to act on — the same masking class as
[C-615](C-615-refuse-an-unrunnable-fleet-loop-at-validate-time-not-inside-a-spawned-worker.md), where
the real diagnostic existed but never reached the surface. Fleet state writes on a full disk are also
unprotected; `state.json` is 5.7 MB and rewritten per revision.

- Related: [C-615](C-615-refuse-an-unrunnable-fleet-loop-at-validate-time-not-inside-a-spawned-worker.md)
  — refuse knowable failures before dispatch rather than after.

## The sharing tradeoff, stated before it is built

A shared `CARGO_TARGET_DIR` is the right fix, but it is not free and the cost should be measured rather
than discovered: **cargo takes a lock per target directory**, so concurrent workers stop building in
parallel and serialize on it (`Blocking waiting for file lock on build directory`).

What that buys and costs, using measured numbers from this session:

| | per-worktree target (today) | one shared target |
| --- | --- | --- |
| disk, 5 workers + integration | ~30–60 GB, and it grew to 66 GB | ~6 GB |
| dependency compilation | repeated once per worktree | once |
| concurrency | builds run in parallel | builds serialize on the lock |

The dependency graph is identical across story worktrees of the same repository, so today's parallelism
is largely spent recompiling the same crates — five times. Sharing converts that waste into a single
cold build plus fast incremental builds for everyone after it. The pathological case is the cold cache:
the first worker compiles the workspace while the rest block on the lock.

Two things follow for the implementation:

- **Share per repository, never across repositories.** Different workspaces must not contend, and their
  artifacts are not interchangeable.
- **Measure the cold-start serialization before widening the fleet.** If first-build blocking dominates,
  the answer is a warm shared target seeded once (build at the pinned base before dispatching a wave),
  not a return to per-worktree directories.

Mechanically it needs two changes, not one: `CARGO_TARGET_DIR` is absent from `SAFE_ENV`
(`flux-system/src/lib.rs`), so it does not currently propagate into a spawned worker at all, and the
sandbox writable set must include the shared directory or every build fails closed.

**Interim in place.** The autopilot derives wave width from free space
(`width = (free − floor − gate_reserve) / story_build_gb`, capped at `max_workers`) and refuses to start
work below the floor. That bounds the damage but does not remove the duplication, so width stays small
for a disk reason rather than a throughput one.

