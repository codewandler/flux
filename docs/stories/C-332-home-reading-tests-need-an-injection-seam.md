---
id: C-332
title: "53 of 73 `HOME`-reading tests have no injection seam and no story"
pillar: Core
epic: road-to-stable
status: in-progress
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

- [x] **The three follow-ups C-297 recorded become real, ranked stories** (or are closed here with
      evidence they are no longer needed):
      `load_config_in(cwd, env)` → ~40 tests, `detect_signals_in(cwd, env)` → ~7 tests, and a
      `FLUX_WORKTREE_DIR` pin for flux-tools → ~6 tests. Re-derive the counts rather than trusting
      the numbers above — C-297's census is over a year of drift old in repo terms.
      → re-derived below; filed as [C-343](C-343-worktree-tests-write-into-the-operators-home.md),
      [C-344](C-344-server-router-tests-read-the-operators-config.md),
      [C-345](C-345-detect-signals-has-no-injection-seam.md).
- [x] **Close the largest tranche in this story**: `load_config_in(cwd, env)`. Reuse the shape
      C-297 already established with `DiscoveryEnv` and C-213 established with `HarnessEnv` (a
      value-held env, which is why `flux-capabilities` is clean *by construction*) — do not invent a
      third injection idiom.
      → the seam is built and the two crates that own the readers are converted (27 tests);
      `flux-server`'s 43 need a *router-level* entry point and are C-344. See Deviations.
- [x] **Failing-first**: a test that today passes or fails depending on the contents of the running
      user's `$HOME`, shown doing so, and pinned afterwards. Prove it by running with a
      deliberately-populated `HOME` and an empty one.
- [x] ⚠ **~20 `flux-tools` tests create and delete real directories under the developer's
      `~/.flux/worktrees`** because they never set `FLUX_WORKTREE_DIR`. That is not only a verdict
      hazard — it is a test suite writing into the operator's home. Treat it as the sharpest item
      even though it is the smallest count.
      → measured (5 allocating tests, not ~20) **and evidenced**: five abandoned trees found in the
      real `~/.flux/worktrees`. Filed as C-343 at the top rank; not half-fixed here, because the
      only in-repo precedent is a `pub(crate)` env lock `flux-tools` cannot reach and a bare
      `set_var` would reintroduce the very race this class exists to remove.
- [x] The remaining tranches are filed with their counts and their seam, so the next agent inherits
      the analysis instead of redoing it.
- [x] Full gate green in both workspaces.

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

## The re-derived census (2026-08-01)

C-297's numbers were estimates written a year of drift ago. Measured against the tree at
`b56f1057`, by walking every test fn body and following in-file helper calls transitively
(`#[test]`/`#[tokio::test]` → reader), then reading each hit:

| tranche | seam | C-297 said | **actual** | disposition |
|---|---|---|---|---|
| `flux-runtime` config half | `load_config_in` | 5 | **9** | **closed here** |
| `flux-config` test `load()` | value-held home | 10 | **18** | **closed here** |
| `flux-server` router | `router_in` | 25 | **43** | filed → C-344 |
| `flux-flow` engine turns | `detect_signals_in` | 4 | **26** | filed → C-345 |
| `flux-runtime` direct | `detect_signals_in` | 2 | **3** | filed → C-345 |
| `flux-app` | `detect_signals_in` | 1 | **1** | filed → C-345 |
| `flux-tools` worktree | worktree base dir | 6 | **5** (+3 that refuse before allocating) | filed → C-343 |
| `flux-agent` skill discovery | `DiscoveryEnv` | 2 | **2** | noted in C-345 |

**Where C-297 was most wrong, and why it matters:** it sized the `detect_signals` tranche at 7 by
counting direct callers. The real number is 30, because `flux_flow::engine::surfaced_op_names` runs
`detect_signals` on **every turn** — so the surfaced op catalog 26 engine tests assert against is
partly a function of the developer's `~/.claude/commands`. Under-sizing a tranche by 4× is exactly
how "the next person redoes the analysis from scratch" happens.

**A seam C-297 did not name at all**, found while measuring: `Config::skill_dir_paths()`,
`workspace_add_dirs()`, `skill_dirs_with_origin()` and `sandbox_writable()`
(`crates/flux-config/src/lib.rs:947,963,992,1028`) expand a leading `~/` by reading process `HOME`
in **production** code. That is why three `flux-config` tests still hold `HOME_LOCK` after this
change — their config *layer* is pinned by value now, but the tilde expansion they assert genuinely
exercises process `HOME`. Recorded in C-344's Notes.

## Progress

- **The seam** (`crates/flux-runtime/src/metadata.rs`): `DiscoveryEnv` grew `home()` and a private
  `flux_root()`; `trusted_flux_root` and `read_config_texts` take it. Four additive public entry
  points — `load_config_in`, `config_layers_in`, `load_groups_in`, `persist_user_theme_in` — with
  the four existing ones delegating via `DiscoveryEnv::from_process()`. No caller outside the crate
  changed; nothing about production behaviour changed.
- **`flux-runtime`'s 9 config tests** now pin an env. Two of them
  (`load_config_managed_layer_precedes_user_and_project_for_defaults`,
  `config_layers_returns_each_raw_unmerged_layer`) no longer repoint process `HOME` at all and no
  longer take `HOME_LOCK`.
- **`flux-config`'s 18** now go through a value-held home: the `#[cfg(test)]` `load()`/`load_groups()`
  helpers gained `load_in(cwd, Option<&Path>)` / `load_groups_in(..)`, and plain `load(cwd)` now
  means *no user layer* rather than *whatever the operator has*. Five tests dropped their
  `set_var("HOME", ..)`; three keep it, for the tilde-expansion reason above.
- **Failing-first, at the merge base** (`b56f1057`): with a fixture `HOME` whose
  `~/.flux/config.toml` sets `model`, `load_config_reads_flux_managed_config_env_override` fails
  `left: Some("from-the-operators-home") / right: Some("org-default")`; with an empty `HOME` it
  passes. After the change its verdict is identical under both.
- **Two new pins**: `load_config_in_reads_the_pinned_home_and_never_the_process_home` (both
  directions — a pinned home *is* read, an empty one is *not*) and
  `persist_user_theme_in_writes_into_the_pinned_home` (the write counterpart).
- ⚠ **Trap for a resuming agent.** `crates/flux-config/src/lib.rs`'s `HOME_LOCK` is now held by
  exactly three tests. If C-344 removes the tilde-expansion dependency, delete the lock in the same
  commit — a lock nothing takes is worse than no lock, because the next test to repoint `HOME`
  copies the pattern from a test that no longer needs it.
