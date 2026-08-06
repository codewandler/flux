---
id: C-619
title: "Applying a wave must advance its Board items, or autonomy re-implements the same story forever"
pillar: "Core"
status: done
epic: fleet-harness-throughput
areas: [flux-cli]
done_override: "Implemented and tested in main: applying a wave accepts the candidate with an annotated fleet/accepted/<wave>/<repo> tag instead of merging in a detached source checkout where the merge landed on no branch; delivered items are also filtered from re-dispatch. Tests updated to the tag contract."
---

# Applying a wave must advance its Board items, or autonomy re-implements the same story forever

## Goal

Close the loop between a delivered wave and the Board that selects work. A wave can pass its full gate
and be applied while its Board item stays `ready`, so the next dispatch selects the same story again —
indefinitely.

## Acceptance

Resolved by operator decision: **main is written exactly once, by the gated accumulation snapshot**, so
`apply` accepts a candidate rather than merging it.

- [x] Dispatch refuses an item a `green`/`applied` wave already delivered, naming the wave — Fleet state
      is the authority, since the Board cannot be relied on (below).
- [x] `apply` no longer merges. It pins the candidate with an annotated tag
      (`fleet/accepted/<wave>/<repo>`) that outlives the wave, the integration branch and worktree
      reclamation, and reports the tag so accepted-but-unmerged work can always be found.
- [x] Reclamation no longer depends on a merge that never happens: a commit named by any branch or tag
      survives `git worktree remove`, so that — not canonical-ref ancestry — is the safety test.
- [x] Failing first: a test proves `green`/`applied` items are recognized as delivered while
      `accepted`/`agent-turn-failed`/`cancelled` items stay dispatchable.
- [ ] Board items still transition out of `ready` once their checkout is back at its canonical ref.
      **Blocked, and not by this story** — see below.

## Notes

**Observed as an infinite loop, not a theory.** With `flux/C-569` implemented (`204ac6fd`), gated green
(candidate `d4164f13`, full release gate `exit_code: 0`) and applied, the autonomous driver's very next
tick did this:

```
13:32:09  dispatch flux/C-569
13:32:16  dispatch flux/C-569
13:32:20  dispatch flux/C-569
```

Three waves for the same finished story in eleven seconds (`wave-338`, `wave-340`, both cancelled by
hand). `flux board get flux/C-569` still reports `ready`, and `board next` still ranks it first at
priority 0, so an unattended driver re-implements the same story until something external stops it.
Every repetition costs a full worker turn and ~6 GB of build output.

**Two independent causes, both required for the loop:**

1. **No Board transition on apply.** `apply_wave` updates Fleet state (`status: applied`,
   `apply_eligible: false`) and never touches the Board. Fleet and Board disagree about whether the work
   exists.
2. **`apply` merges into a detached HEAD.** The merge target is `.flux/fleet/sources/<repo>`, which is a
   git worktree of the real repository checked out **detached** at the pinned base. So
   `git merge --no-ff <candidate>` produced `cf1b60e9` on no branch at all: `main` is untouched, the
   merge is reachable only from that worktree's HEAD, and any future `fleet refresh` would strand it.
   The message "applied locally; nothing was pushed" overstates what happened — nothing was *published*
   either.

Because of (2), reclamation also cannot work as designed: a story worktree's commits are never
reachable from the canonical ref, so every worktree is retained forever and the disk grows without
bound (see [C-617](C-617-fleet-reclaims-wave-storage-and-refuses-to-dispatch-without-disk-headroom.md)).

**Why this is the top blocker for unattended operation.** The other gaps degrade throughput; this one
inverts it — the pipeline gets *busier* the more work it completes, spending every turn on a story it
has already delivered.

**Implementation constraints found while scoping it.** Fleet does not touch the Board anywhere today —
no board root is resolved and no story is written in `board_fleet_cmd.rs`'s Fleet half — so this adds a
new dependency direction rather than extending an existing one. Two things it must respect:

- **Go through the Board's own transition path** (`BoardAction::Transition` / `transition()`), never a
  frontmatter write. Hand-editing status bypasses validation and desynchronizes the coordinator.
- **Resolve the item's board per repository.** Items are namespaced (`flux/C-569`), each
  `[[repositories]]` entry carries its own `board` binding, and the board root is the repository root —
  not the fleet root. A single fleet spans several boards.

**Interim backstop in place, deliberately outside the binary.** The autopilot refuses to dispatch an
item that any `green`/`applied` wave already delivered, reading Fleet state rather than the Board:

```
SKIP flux/C-569: a green/applied wave already delivered it (C-619) — refusing to re-implement
```

That stops the loop but leaves the two systems disagreeing, so `board next` still ranks a delivered
story first and any other driver — or an operator following the Board — is still misled.

- Related: [C-616](C-616-a-story-worker-authors-its-own-handoff-instead-of-a-third-party-transcribing-it.md),
  [C-618](C-618-fleet-resume-must-not-replay-the-coordinator-inbox-into-a-story-worker.md) — the other
  two boundaries an autonomous pipeline crosses on every wave.

## Why the Board half is blocked

A workspace board refuses every mutation while its member checkout is off the configured canonical ref:

```
conflict/precondition: workspace member flux checkout is not at configured canonical ref origin/main
```

That is not incidental. Delivering work necessarily moves the member checkout ahead of `origin/main`, and
nothing in this pipeline pushes — so the guard is tripped *by success*. Advancing the Board and delivering
work are mutually exclusive under the current configuration, which makes a Board transition unusable as
the fix for the dispatch loop. Hence the loop is closed from Fleet state instead, which is always
available and always accurate about what Fleet itself delivered.

Resolving the Board half needs an operator decision, not more code: either planning mutations run against
a checkout pinned at the canonical ref (a dedicated board worktree), or the canonical-ref expectation is
reconfigured, or delivered work is pushed so `origin/main` advances. Until then a delivered story reads
`ready`, which is wrong for anyone reading the Board directly even though the pipeline no longer acts on
it.

## Shipped state

Installed and gated (371 tests, clippy, fmt). Verified live:

```
$ flux fleet run flux/C-569
error: conflict/precondition: already delivered by a gated wave, so this dispatch would re-implement
finished work: flux/C-569 (wave wave-302).
```

The accept-and-tag path is gated and installed but **not yet exercised on a live wave** — it needs the
next wave to reach `green`, since `wave-302` was applied under the old merging code and cannot be
re-applied.
