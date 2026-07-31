---
id: C-332
title: "53 of 73 `HOME`-reading tests have no injection seam and no story"
pillar: Core
epic: road-to-stable
status: ready
priority: 4
areas: [flux-runtime, flux-tools, flux-cli]
note: "C-297 enumerated 73 hazardous HOME-reading tests and wrote its three follow-ups in its Notes only — they were never filed, so 53 known-hazardous tests are invisible to the board. This story makes them visible and closes the largest tranche"
---

# The `HOME`-reading tests C-297 measured but never filed

## Goal

C-297 built `DiscoveryEnv` (`crates/flux-runtime/src/metadata.rs`) and fixed the test that read the
operator's real `~/.claude/skills`. In doing so it **enumerated 73 tests that read `HOME`** and wrote
three concrete follow-ups — and wrote them **in its Notes section only**. They were never filed as
stories.

So 53 known-hazardous tests are invisible to the board: they do not appear in any backlog, no
priority ranks them, and the next person to trip over one rediscovers the analysis from scratch.
**This is the largest unfiled gap in the repo**, and it is bookkeeping debt before it is engineering
debt.

The hazard is the one C-307 named and C-319 paid for: *a regression gate's verdict must depend only
on its fixture, never on the machine it runs on.* A test that reads the developer's `HOME` passes or
fails for reasons that have nothing to do with the diff — and the cost is not the failure, it is the
**diagnosis**, because it looks exactly like a real regression in whatever story is in flight.

## Acceptance

- [ ] **The three follow-ups C-297 recorded become real, ranked stories** (or are closed here with
      evidence they are no longer needed):
      `load_config_in(cwd, env)` → ~40 tests, `detect_signals_in(cwd, env)` → ~7 tests, and a
      `FLUX_WORKTREE_DIR` pin for flux-tools → ~6 tests. Re-derive the counts rather than trusting
      the numbers above — C-297's census is over a year of drift old in repo terms.
- [ ] **Close the largest tranche in this story**: `load_config_in(cwd, env)`. Reuse the shape
      C-297 already established with `DiscoveryEnv` and C-213 established with `HarnessEnv` (a
      value-held env, which is why `flux-capabilities` is clean *by construction*) — do not invent a
      third injection idiom.
- [ ] **Failing-first**: a test that today passes or fails depending on the contents of the running
      user's `$HOME`, shown doing so, and pinned afterwards. Prove it by running with a
      deliberately-populated `HOME` and an empty one.
- [ ] ⚠ **~20 `flux-tools` tests create and delete real directories under the developer's
      `~/.flux/worktrees`** because they never set `FLUX_WORKTREE_DIR`. That is not only a verdict
      hazard — it is a test suite writing into the operator's home. Treat it as the sharpest item
      even though it is the smallest count.
- [ ] The remaining tranches are filed with their counts and their seam, so the next agent inherits
      the analysis instead of redoing it.
- [ ] Full gate green in both workspaces.

## Notes

- Found by [C-319](C-319-strict-review-test-depends-on-tree-dirtiness.md)'s mandated census of tests
  reading live machine state, which surfaced that C-297's follow-ups were never filed.
- The sibling story is [C-326](C-326-update-env-var-rewrites-goldens.md) — the same class, but the
  *silent* variant: three golden guards that rewrite their goldens and pass having compared nothing
  when `UPDATE` is merely present in the environment.
- Related seams that already exist and should be reused rather than duplicated: `HarnessEnv`
  (C-213), `DiscoveryEnv` (`crates/flux-runtime/src/metadata.rs`, C-297), `PinnedRepositoryRead`
  (`crates/flux-sdk/tests/strict_review.rs`, C-319).
- ⚠ **Two dangling references to fix while you are here:** `C-297:52` and `C-209:104` both cite a
  root `CLAUDE.md` that does not exist in this repo. They are the exact stories a future implementor
  would follow.
- The general guard for this class is C-333 (a codegate lint banning ambient reads in test code);
  this story is the data it will need to size its initial waiver set.
