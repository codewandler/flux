---
id: C-120
title: "Allocate agent worktrees on real disk, not /tmp"
pillar: Core
status: done
epic: context-local-git-worktrees
design: docs/designs/context-local-git-worktrees.md
note: "base = $FLUX_WORKTREE_DIR else ~/.flux/worktrees; /tmp is commonly a RAM-backed tmpfs a worktree build would fill"
---

# Allocate agent worktrees on real disk, not /tmp

## Goal
Move `allocate_worktree_dir`'s base from the system temp dir to real disk. `/tmp` is commonly a
RAM-backed tmpfs (32 GB on the dev machine); the first thing an agent does after
`git_worktree_enter` on a Rust repo is build it, and a multi-GB `target/` in tmpfs starves every
process that needs `/tmp` — observed live during the epic's merge verification.

## Acceptance
- [x] Base resolution: `$FLUX_WORKTREE_DIR` (if set, non-empty) → `$HOME/.flux/worktrees` → system
      temp dir as last resort; base and entries created `0o700` on Unix; entry naming unchanged.
- [x] `remove_worktree_dir` stays fail-closed against the *resolved* base (prefix + direct-child
      checks), refusing entries under any other base — `worktree_dir_alloc_and_guarded_removal`
      extended, plus `worktree_base_prefers_home_flux_over_tmp`.
- [x] Design doc, op catalog rows, and CHANGELOG wording no longer promise `/tmp`.

## Progress
- 2026-07-28 implemented: `worktree_base_dir()` resolution + hardened removal guard in
  flux-system; tests hermetic via `FLUX_WORKTREE_DIR` under the sandbox env lock.

## Notes
- Prompted by the tmpfs incident during the epic merge verification (design doc Risks has the
  detail).
