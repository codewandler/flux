---
id: C-720
title: "A reclaimed wave must prove its worktrees and build output are gone"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "flux fleet reclaim printed 'reclaimed 39 wave(s)' and exit 0 while freeing 0 bytes: 39G of worktrees, 34 directories and 72 fleet branches all survived, and fleet doctor still reported the same 36 branch-without-unique-work findings whose prescribed fix is that exact command"
---

# A reclaimed wave must prove its worktrees and build output are gone

## Goal

`flux fleet reclaim` must report what it verifiably removed, not what it considered removing.
Today it prints a count and exits 0 having changed nothing, which is worse than failing: the
operator believes disk was freed, `doctor` keeps prescribing the same command, and the loop is
silent. Reclaim is the fleet's only bounded-disk mechanism, and disk is what caps fleet width.

## Acceptance

- [ ] A wave counts as reclaimed only after reclaim re-reads the filesystem and git and confirms
      each target worktree directory is absent and each target branch is gone. The reported count
      equals the number of verified removals, never the number of candidates considered.
- [ ] When a removal is attempted and the target still exists afterwards, the command names the
      path, states why, and exits non-zero. Errors from `git worktree remove` are surfaced, never
      swallowed.
- [ ] A worktree holding uncommitted work — tracked modifications **or** untracked files not
      covered by `.gitignore` — is never removed, and is reported as retained with its path and
      file count. See [[C-722]].
- [ ] Reclaim is per-worktree, not all-or-nothing per wave: a wave holding one branch with unique
      commits and three without reclaims exactly the three. `wave-745` is the fixture — its
      `story/C-575` branch holds work while its `story/C-519`, `integration` and `verify` branches
      do not.
- [ ] `flux fleet doctor` and `flux fleet reclaim` cannot disagree: after a successful reclaim, the
      `branch-without-unique-work` findings that prescribed it are gone.
- [ ] Regression test: a fixture workspace with N wave worktrees of which M hold work (one via an
      untracked file only) removes exactly N-M, reports N-M, and leaves the M intact.
