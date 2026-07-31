---
id: C-343
title: "The test suite creates and abandons real directories in the operator's `~/.flux/worktrees`"
pillar: Core
epic: road-to-stable
status: ready
priority: 3
areas: [flux-tools, flux-system]
note: "C-332's census, tranche C. Not merely a verdict hazard — five abandoned `flux-worktree-848868-*/checkout` trees were found in the operator's real ~/.flux/worktrees, dated to a test run three days earlier. `allocate_worktree_dir()` reads FLUX_WORKTREE_DIR else $HOME/.flux/worktrees, and no flux-tools test pins it"
---

# The worktree tests write into the operator's home

## Goal

`flux_system::allocate_worktree_dir()` (`crates/flux-system/src/lib.rs:352-375`) allocates under
`$FLUX_WORKTREE_DIR`, else **`$HOME/.flux/worktrees`**, else the system temp dir. No `flux-tools`
test sets that variable, so every test that drives `GitWorktreeEnterTool` creates a real directory
tree in the developer's home — and when the test fails or panics between `enter` and `leave`, leaves
it there.

This is the sharpest item in [C-332](C-332-home-reading-tests-need-an-injection-seam.md)'s census
even though it is the smallest count, because it is not only a *verdict* hazard: a unit test suite
is writing into the operator's home directory. **Evidence, not inference** — measured on this
machine on 2026-08-01:

```
$ ls -la ~/.flux/worktrees
drwx------ 3 timo timo 4096 Jul 29 12:42 flux-worktree-848868-0
drwx------ 3 timo timo 4096 Jul 29 12:42 flux-worktree-848868-1
drwx------ 3 timo timo 4096 Jul 29 12:42 flux-worktree-848868-2
drwx------ 3 timo timo 4096 Jul 29 12:42 flux-worktree-848868-3
drwx------ 3 timo timo 4096 Jul 29 12:42 flux-worktree-848868-4
$ ls -la ~/.flux/worktrees/flux-worktree-848868-0
drwxr-xr-x 2 timo timo 4096 Jul 29 12:42 checkout
```

Five abandoned trees from one test process (pid 848868), three days stale. A `git worktree add`
inside one of those is where a multi-GB `target/` would land.

⚠ **Measured nuance, so the next implementor does not chase the wrong thing:** a *clean*
`cargo test --workspace` on 2026-08-01 added no new directories — the `leave` path removes what
`enter` created. The five above are the residue of a run that died between the two. So the fix is
about **where the write lands**, not about adding cleanup: a test must not be able to strand
anything in the operator's home no matter how it dies.

## Acceptance

- [ ] **Failing-first**: a test that fails while `$HOME/.flux/worktrees` is unwritable (or that
      observes a directory appearing under a pinned fake home) at the merge base, and passes after.
      The point to prove is *where the write lands*, not that the worktree ops work.
- [ ] No test in `flux-tools` (or anywhere else) allocates a worktree parent under the process's
      real `$HOME`. The five sites are in `crates/flux-tools/src/lib.rs`, all reached through the
      `worktree_ctx()` helper (`:6756`):
      `git_worktree_enter_leave_round_trip` (`:6768`),
      `git_worktree_enter_rejects_nested_sessions` (`:6875`),
      `git_worktree_leave_rejects_dirty_worktree` (`:6897`),
      `git_worktree_leave_rejects_moved_main` (`:6940`),
      `git_worktree_leave_trial_merge_conflict_aborts_cleanly` (`:6984`).
      (`git_worktree_enter_rejects_dirty_main` `:6849`, `git_worktree_enter_rejects_non_main_branch`
      `:6862` and `git_worktree_leave_requires_a_session` `:6929` refuse *before* allocating, so they
      are on the list only if the seam changes their construction.)
- [ ] **The seam is a value, not a `set_var`.** `crates/flux-system/src/lib.rs:2598-2603` already
      pins `FLUX_WORKTREE_DIR` under `sandbox::EnvGuard` — but `EnvGuard` is `pub(crate)`, so
      `flux-tools` cannot reuse it, and a bare `std::env::set_var` from a test is the *racy*
      anti-pattern this whole class exists to remove. Prefer threading the base directory as a value
      (the `DiscoveryEnv`/`HarnessEnv` shape) over exporting the lock.
- [ ] The stale `~/.flux/worktrees/flux-worktree-*` trees quoted above are not something the fix can
      remove, but the story says plainly whether a *production* leak exists too — i.e. whether the
      `git_worktree_leave` cleanup-pending path can strand a directory for a real user, or whether
      this is test-only.
- [ ] Full gate green in both workspaces.

## Notes

- Parent census: [C-332](C-332-home-reading-tests-need-an-injection-seam.md). C-297 estimated this
  tranche at 6 tests; the re-derived count is **5 that allocate + 3 that refuse first**.
- The reader is `worktree_base_dir()` / `allocate_worktree_dir()`,
  `crates/flux-system/src/lib.rs:352` and `:373`. The call site is
  `crates/flux-tools/src/lib.rs:3893`, inside `GitWorktreeEnterTool::execute` — which is why the
  injection point is not obvious: there is no env parameter anywhere on that path today.
- ⚠ The default is deliberately **not** `/tmp` (`:352`'s doc comment): `/tmp` is commonly a
  RAM-backed tmpfs and a build inside an entered worktree would fill it. Any fix must keep a real
  on-disk default for production and only redirect *tests*.
- `crates/flux-orchestrate/src/worker.rs:505,1431` documents the same base-dir contract; check it
  still reads true after the change.
