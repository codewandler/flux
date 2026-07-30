---
id: C-249
title: "The git family's clean-tree preconditions are per-op accidents, and \"commit or stash them first\" is unactionable for untracked files"
pillar: Core
status: in-progress
priority: 8
areas: [flux-tools]
note: "surfaced by C-238's review: git_worktree_leave and git_revert each grew their own clean-tree guard for the same reason, git_merge had none, and three ops share advice that a plain `git stash` cannot carry out"
---

# The git family's clean-tree preconditions are per-op accidents, and "commit or stash them first" is unactionable for untracked files

## Goal
Two related inconsistencies in the guarded `git_*` family, both found by the independent review of
C-238 and both deliberately left out of that story's rework so its diff stayed scoped to the
demonstrated defect.

**1. The clean-tree precondition is decided per-op, by whoever wrote the op.**
`git_worktree_leave` refuses unless `git status --porcelain` is empty, precisely so its always-abort
discipline cannot destroy work it did not create (`crates/flux-tools/src/lib.rs:3778-3784`).
`git_revert` independently grew the same guard for the same reason (`:2795-2807`). `git_merge` had
none, which is how C-238's blocking defect existed at all. Three ops, one hazard, three separate
decisions — the next merging or aborting op will make a fourth. Decide the policy once and make it
structural, so an op that can abort or reset **cannot** be written without confronting the
precondition.

**2. `"commit or stash them first"` is advice the caller cannot follow.** The guard triggers on
`git status --porcelain`, whose output includes untracked `??` entries — and a plain `git stash` does
not clear those. So an agent that follows the message retries and fails identically. The wording is
shared by `git_revert`, `git_snapshot` and `git_worktree_enter`.

## Acceptance
- [x] The clean-tree policy is stated once and enforced structurally rather than restated per op —
      e.g. a shared precondition helper that abort-capable ops must call, with a test that fails if
      an op declaring a destructive/aborting path skips it. A comment convention is **not** enough;
      the point is that the next op cannot silently omit it.
- [x] **Failing-first test**: an abort-capable `git_*` op without the precondition is rejected by the
      suite. It fails today, because nothing notices that `git_merge` lacked one.
- [x] The refusal message distinguishes tracked modifications from untracked files and gives advice
      that actually clears the state it names (untracked needs `git clean` or an explicit
      `stash -u`, not a bare `stash`). Reconciled across `git_revert`, `git_snapshot` and
      `git_worktree_enter` so all three say the same true thing.
- [x] No behavioural weakening: every op that refuses a dirty tree today still refuses it.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — filed from the independent review of C-238. That review confirmed the blocking case
  (a pre-existing `MERGE_HEAD` plus an unconditional `git merge --abort` destroying hand-resolved
  work) and it is fixed in C-238 itself. This story is the *general* policy, which is a different and
  larger change.
- 2026-07-30 — implemented on `impl/C-249`.
  - **The policy is one block of code**: `crates/flux-tools/src/lib.rs`, section "The guarded git
    family's tree preconditions". Two parts, deliberately different questions: (1) *no operation of
    the same kind already in flight* (`MERGE_HEAD`/`REVERT_HEAD`/`CHERRY_PICK_HEAD`) — mandatory for
    every abort-capable op, and what licenses its abort; (2) *a clean tree* — required only where the
    abort restores the whole tree rather than what this call staged. Each op declares a
    `TreePrecondition` whose `CleanTree::Required(why)` / `NotRequired(why)` **carries its reason**,
    so declining the precondition is as explicit as requiring it. Per the story's Notes the policy is
    scoped to the mid-operation tree, not dirtiness in general: `git_merge` is
    `NotRequired`, and a test pins that it keeps merging over a dirty tree.
  - **Structural, not conventional**: `crates/flux-tools/tests/git_tree_policy.rs` scans the family
    and fails the suite for any `Git*Tool` that runs a blanket restore (`--abort`, `--hard`, `-fd`)
    without calling `require_tree_precondition`. At the merge base it fails naming `git_merge`,
    `git_revert` and `git_worktree_leave`.
  - **Wording reconciled** across `git_revert`, `git_worktree_enter`, both `git_worktree_leave`
    checkouts (`flux-tools`) and `git_snapshot` (`flux-eval`, which cannot depend on `flux-tools`, so
    the formatter is mirrored there rather than shared): tracked and untracked counted and listed
    separately, with `git stash -u` / `git clean -fd` named for the `??` entries a bare `git stash`
    leaves behind.
  - **Generalisation, not just consolidation**: `git_revert` gained the in-flight guard `git_merge`
    got in C-238 — a hand-resolved, uncommitted `git revert` is no longer abortable by a later
    `git_revert` call — and `git_worktree_leave` now proves the original checkout is not mid-merge
    before its always-aborted trial merge. Both are new refusals, no relaxations.
  - Preflight git failures inside the shared helper return a recoverable `ToolResult::error` instead
    of `?`-propagating a plan-halting raw error (the C-241 review shape).
- 2026-07-30 — merged `main` (27 commits) into `impl/C-249` and brought **`fleet.isolate`** (C-241,
  landed after the fork) under the policy. It aborts nothing, so no abort-based rule selects it, and
  it is named `Fleet*`, so no name-based rule selects it — but it ran its own `git status
  --porcelain` and refused with a hand-copied variant of `git_revert`'s pre-C-249 wording (its
  comment says so: *"Same wording care as `git_revert`"*). Excluding it would have left the family
  with two dirty-tree messages on day one, which is the drift this story exists to end. It now
  declares `FLEET_ISOLATE_TREE` — `in_flight: &[]`, `CleanTree::Required` for its own reason (it
  checks out HEAD for a worker, so uncommitted work would be missing from that copy). Membership is
  therefore **not** the same as being abort-capable, and the policy doc now says so.
- 2026-07-30 — the selector no longer depends on the `Git` name prefix, which was the part that
  would rot again. Sections are bounded by the file's own banner separators and selected purely on
  what an op's body invokes. Two rules now, because one is not enough:
  `abort_capable_ops_route_through_the_shared_tree_precondition` (blanket restore ⇒ must call the
  helper; the selected set is pinned by sentinels so a rotted selector cannot pass vacuously), and
  `no_op_hand_rolls_its_own_clean_tree_check` (`git status --porcelain` may appear only inside the
  shared `tree_status` helper). The second is the one that would have caught `fleet.isolate` the day
  it landed: name-agnostic, abort-agnostic, and — unlike the first — it does not erase itself as ops
  are converted. Verified against main's unmodified source: it names `lib.rs:4118`, `fleet.isolate`'s
  own probe.

## Notes
- Useful negative result from the same review, worth not re-deriving: an attempt to build a
  work-loss case from **ordinary** dirtiness failed. A dirty *index* makes `git merge` refuse to
  start, leaving no `MERGE_HEAD` and the tree untouched; unstaged unrelated edits survived
  `git merge --abort` intact. So the hazard is specifically the **mid-operation** tree
  (`MERGE_HEAD`/`REVERT_HEAD` present), not dirtiness in general. Scope the policy to what is
  actually dangerous rather than refusing every dirty tree reflexively — over-refusing would make the
  family unusable in exactly the multi-author situation C-92's hunk staging exists to serve.
- Related asymmetry, also from that review: `git revert` refuses a dirty **index** but will happily
  commit over unstaged or untracked changes. So flux's in-process check is deliberately *stricter*
  than git's. That is the right direction, but it means the precondition is flux policy rather than
  git behaviour, and it should be documented as such — a comment claiming git enforces it is wrong
  (that specific wrong comment is fixed in C-238).
- Seam: `crates/flux-tools/src/lib.rs`, the `git_*` family plus the shared `run_git` helper.
