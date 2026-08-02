---
id: C-489
title: Reject tracked Cargo build output
pillar: Core
status: done
note: "make the codegate reject force-added target/ and target-* trees before binary garbage reaches main"
---

# Reject tracked Cargo build output

## Goal

Prevent Cargo build directories from entering repository history even when an author bypasses
`.gitignore` with a forced add.

## Acceptance

- [x] A failing-first detector test covers root, nested-workspace, and suffixed Cargo target trees
      without rejecting ordinary paths that merely contain the word "target".
- [x] `flux-codegate` reads Git's tracked-file index and fails when any tracked path is inside a
      `target/` or `target-*` directory.
- [x] The binary-bearing local branches are rewritten without `target-int/`, and no branch or
      reflog retains those objects.
- [x] The codegate and repository gate pass.

## Progress

- 2026-08-02: opened after a disk audit found 20.35 GiB of `target-int/` files force-added in an
  unmerged commit despite the existing ignore rule.
- 2026-08-02: captured the target-directory predicate failing first, then added the Git-index
  census and generalized both workspace ignore files to `target-*`.
- 2026-08-02: rewrote `impl/C-312`, `impl/C-391`, and `impl/C-393` with identical non-target trees;
  expired unreachable reflogs and reduced the repository pack from 4.18 GiB to 32 MiB.
- 2026-08-02: workspace build, tests, clippy, formatting, and all 46 codegate checks passed using a
  disposable target directory.

## Notes

- Cleanup rewrites only the three local branches that contained the bad commit; `main` and the
  owner's dirty worktree remain untouched.
