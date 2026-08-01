---
id: C-402
title: "Nothing lists or prunes `~/.flux/worktrees`, so a stranded worktree is silent and unbounded"
pillar: Core
status: ready
priority: 12
epic: road-to-stable
areas: [flux-tools, flux-cli, flux-system]
note: "split out of C-391, which proved the leak exists in production rather than only in tests. C-391 fixed where the *test* write lands; it deliberately did not touch the production paths, because for a real user those directories are the product of an operation they asked for and hold real work — removing them automatically would be wrong. What is missing is visibility, not cleanup"
---

# Nothing lists or prunes the worktree parent

## Goal

Make a stranded worktree under `$FLUX_WORKTREE_DIR` / `$HOME/.flux/worktrees` **visible and
prunable by the operator**, without ever removing work automatically.

[C-391](C-391-worktree-tests-write-into-the-operators-home.md) established, as measured fact, that
the leak is not test-only. Three production paths strand a directory:

1. **`git_worktree_leave`'s cleanup-pending path.** Once the merge has landed, a failing
   `git worktree remove` / `prune` / `branch -d` / directory removal returns `cleanup_pending` and
   deliberately keeps the session so a retry can finish. If the retry never comes — the session
   ends, the process exits, the user walks away — the parent stays on disk forever.
2. **`git_worktree_enter` without a `leave`.** A crash, a cancel, or an agent that simply stops
   between the two leaves the parent behind. This is what produced the five stale trees C-391
   quoted.
3. **`fleet.isolate`, by design.** Cleanup is the caller's, because the worktree holds the only copy
   of a worker's unmerged diff.

Each of those leaves a directory that a `git worktree add` may have filled with a multi-GB `target/`.
Nothing in flux ever lists or prunes that parent, so the growth is silent and unbounded.

## Acceptance

- [ ] **Failing-first**: a test that strands a worktree by each of the three paths above and asserts
      the operator can *enumerate* it afterwards — failing at the merge base because no enumeration
      surface exists.
- [ ] An operator-facing way to **list** what is under the worktree parent, with enough per-entry
      detail to decide: age, size on disk, whether a branch still points at it, and whether it holds
      uncommitted or unmerged work.
- [ ] A **prune** that is safe by construction: it refuses any entry holding uncommitted changes or
      an unmerged branch, and reports what it refused and why rather than skipping silently.
      Never automatic — the operator asks for it.
- [ ] The CLI shape matches the repo's convention: an explicit subcommand, no implicit default-run.
- [ ] `crates/flux-orchestrate/src/worker.rs:505,1431` documents the same base-dir contract — check
      it still reads true, and update it if this story changes the contract.
- [ ] Full gate green in both workspaces.

## Notes

- The seam already exists after C-391: every `System` carries a `WorktreeBase`, so the enumeration
  resolves *that* base rather than re-deriving the ladder. Do not reintroduce a free function that
  reads the process environment.
- ⚠ The default parent is deliberately **not** `/tmp` (`crates/flux-system/src/lib.rs:352`'s doc
  comment): `/tmp` is commonly a RAM-backed tmpfs and a build inside an entered worktree would fill
  it. Whatever this story adds must keep a real on-disk default for production.
- `fleet.isolate`'s worktrees are the interesting case for the refusal rule — by design they hold
  the only copy of a worker's unmerged diff, so they are exactly what a prune must not take.

## Progress

- Filed 2026-08-01 at C-391's integration, from that story's measured finding.
