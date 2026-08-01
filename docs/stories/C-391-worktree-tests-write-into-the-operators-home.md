---
id: C-391
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

- [x] **Failing-first**: a test that fails while `$HOME/.flux/worktrees` is unwritable (or that
      observes a directory appearing under a pinned fake home) at the merge base, and passes after.
      The point to prove is *where the write lands*, not that the worktree ops work.
- [x] No test in `flux-tools` (or anywhere else) allocates a worktree parent under the process's
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
- [x] **The seam is a value, not a `set_var`.** `crates/flux-system/src/lib.rs:2598-2603` already
      pins `FLUX_WORKTREE_DIR` under `sandbox::EnvGuard` — but `EnvGuard` is `pub(crate)`, so
      `flux-tools` cannot reuse it, and a bare `std::env::set_var` from a test is the *racy*
      anti-pattern this whole class exists to remove. Prefer threading the base directory as a value
      (the `DiscoveryEnv`/`HarnessEnv` shape) over exporting the lock.
- [x] The stale `~/.flux/worktrees/flux-worktree-*` trees quoted above are not something the fix can
      remove, but the story says plainly whether a *production* leak exists too — i.e. whether the
      `git_worktree_leave` cleanup-pending path can strand a directory for a real user, or whether
      this is test-only.
- [x] Full gate green in both workspaces.

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

## Progress

- **The seam** (`crates/flux-system/src/lib.rs`): `WorktreeBase` — a value holding one directory,
  built by `from_process()` (the unchanged `$FLUX_WORKTREE_DIR` → `$HOME/.flux/worktrees` → temp
  ladder) or `pinned(dir)`. Every `System` carries one; `System::allocate_worktree_dir` /
  `remove_worktree_dir` resolve *that* base, `with_worktree_base` pins it, and `rerooted` carries it
  into the entered worktree. The free `allocate_worktree_dir()`/`remove_worktree_dir()` stay, now
  delegating to `from_process()`, so the published crate's API is purely additive. Same value-held
  shape as `HarnessEnv` (C-213) and `DiscoveryEnv`/`load_config_in` (C-297/C-332) and
  `router_in` (C-392) — no `set_var`, no exported lock.
- **`flux-tools`**: the four production call sites (`git_worktree_enter` ×4 counting cleanup,
  `git_worktree_leave`, `fleet.isolate` ×2) go through the `System` they already hold. Every test
  `System` in the crate is pinned to a per-test-thread base under the temp dir — `ctx()` plus the
  eleven constructions in the sibling modules' test blocks — via `crate::test_worktrees`.
- **The count is 7, not 5.** Measured mechanically at the merge base `f9a3f93f` by running each of
  the 197 lib tests alone with its own `FLUX_WORKTREE_DIR` and asking whether that base was created:
  the five this story named, **plus `fleet_isolate_gives_concurrent_callers_disjoint_worktrees_and_leaves_the_root_alone`
  and `fleet_isolate_preflights_refuse_recoverably_and_create_nothing`**. The `fleet.isolate`
  tranche was missed because the census walked out from `GitWorktreeEnterTool`/`worktree_ctx()`,
  and `fleet.isolate` reaches the same allocator from a different op — structurally the same
  error C-332 diagnosed for the `flux-server` router tranche, in the story that diagnosed it. The
  three refuse-before-allocating tests were confirmed non-allocating by the same run.
- **Failing-first, at the merge base** (`f9a3f93f`, in a worktree with its own build cache):
  `git_worktree_enter_allocates_under_the_pinned_base_never_the_operators_home` fails with
  `the worktree parent must be allocated under this test's own pinned base, not
  /home/timo/.flux/worktrees` — and that single failing run **left a sixth tree**
  (`flux-worktree-1590375-0`) in the real `~/.flux/worktrees`, reproducing the defect while proving
  it. It passes after. Its source is byte-identical in both runs: it names no new API on purpose.
- **Panic path**: the pinned base is a `thread_local!` whose `Drop` removes it, so it runs while a
  panicking test thread unwinds. `a_panicking_test_thread_leaves_no_worktree_behind` allocates on a
  thread that then panics and asserts the tree is gone once that thread has ended.
- **The check** (`crates/flux-tools/tests/worktree_base_is_pinned.rs`): a census over this crate's
  test corpus — no free `allocate_worktree_dir(`/`remove_worktree_dir(`/`WorktreeBase::from_process(`,
  and every `System::new(`/`System::from_env(` followed by the pin — with vacuity floors and a unit
  test of its own scanner. Verified to fire by reintroducing each violation in turn.
- ⚠ **A production leak exists too; this is not test-only.** Three paths strand a directory under a
  real user's `~/.flux/worktrees`, and nothing in flux ever prunes that directory:
  1. `git_worktree_leave`'s **cleanup-pending** path. Once the merge has landed, a failing
     `git worktree remove`/`prune`/`branch -d`/directory removal returns `cleanup_pending` and
     deliberately keeps the session so a retry can finish. If the retry never comes — the session
     ends, the process exits, the user walks away — the parent stays on disk forever.
  2. `git_worktree_enter` **without a `leave`**: a crash, a cancel, or an agent that simply stops
     between the two leaves the parent behind. This is the same shape as the test-panic case, and
     it is the one that produced the five stale trees quoted above.
  3. `fleet.isolate` **by design** — cleanup is the caller's, because the worktree holds the only
     copy of a worker's unmerged diff.
  The blast radius differs from the test case in one way worth stating: for a real user those
  directories are the *product* of an operation they asked for and contain real work, so removing
  them automatically would be wrong. What is missing is not cleanup but **visibility** — nothing
  lists or prunes `~/.flux/worktrees`, so a leak is silent and unbounded. That is a separate story,
  not something this one should have fixed.
