---
id: C-594
title: "Make `flux fleet run --dry-run` validate a wave without a real worktree"
pillar: Core
epic: fleet-harness-throughput
status: ready
priority: 40
areas: [flux-cli, flux-orchestrate, flux-system]
note: "--dry-run plans the wave, deliberately skips worktree creation, then builds a Workspace on the directory it did not create"
---

# Make `flux fleet run --dry-run` validate a wave without a real worktree

## Goal

`flux fleet run --dry-run` should do what its help promises — "Validate and return the proposed
result without writing" — instead of failing on every invocation. Today it is unusable, so the only
way to find out whether a wave is dispatchable is to dispatch it for real.

## Acceptance

- [ ] **Outstanding:** a regression test for `--dry-run` returning a topology and exiting zero. The
      fix is verified by manual reproduction (the exact failing command now succeeds), not by a test —
      a fixture needs member repos and stories, so it is real work rather than an oversight.
- [x] The dry-run path no longer requires a per-story worktree it deliberately did not create.
- [ ] `--dry-run` still writes nothing: `worktree_root` gains no directory and fleet state's
      revision is unchanged across the call.
- [x] The live path is unchanged: the `!command.dry_run` guard only skips persisting the snapshot.

## Progress

- Fixed. `prepare_wave_worktrees` already skipped `git worktree add` under `--dry-run`; the write that
  broke it was `snapshot_fleet_loop_binding`, which opened a guarded workspace on a story worktree
  that deliberately did not exist. The binding is resolved and validated regardless; only persisting
  its snapshot is skipped, because that is a write.
- Diagnosis took two minutes rather than another strace session because `Workspace::new` now names the
  offending path.

## Notes

- Reproduced on flux 0.56.0 against `<fleet root>`:

  ```
  $ flux fleet run --dry-run flux/C-542
  error: config error: workspace root: No such file or directory (os error 2)
  ```

  Every `--dry-run` invocation fails this way, with any item, from any root.
- `strace` shows the planner getting all the way through topology selection — it allocates
  `wave-253`, resolves each repository's `base_commit` from its `canonical_ref`, and derives the
  integration and story worktree paths — and then stats a directory it never created:

  ```
  statx(".flux/fleet/worktrees/wave-253/flux/integration")     = -1 ENOENT
  statx(".flux/fleet/worktrees/wave-253/flux/stories/C-542")   = -1 ENOENT
  readlink(".flux/fleet/worktrees/wave-253")                   = -1 ENOENT
  ```

  The ENOENT surfaces as `Error::Config` from `Workspace::new`/`Workspace::with_root`
  (`crates/flux-system/src/lib.rs`), which is reached via `System::rerooted`.
- Diagnosing this took far longer than it should have because the error named no path.
  `Workspace::new` and `Workspace::with_root` now include the offending path, matching
  `Workspace::new_optional`, which always did. That change is part of
  [C-593](C-593-authored-segment-ceiling-escapes-router-family-cap.md)'s branch and stands on its own.
