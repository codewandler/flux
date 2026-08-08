# Releasing Flux

How a Flux release actually happens, end to end. [crates/flux-sdk/PUBLISHING.md](../crates/flux-sdk/PUBLISHING.md)
covers the manual runbook and asset backfill; this file covers the automated flow and the states it
can get stuck in.

## The trigger is the `release` branch

A release starts when the remote `release` branch advances. Nothing else starts one — not a tag
push, not a push to `main`, and not a manual workflow dispatch, which is explicitly non-publishing
(`apply: false` previews, `apply: true` rehearses in the runner and promotes nothing).

Locally you are always on `main`. `release` is a publication trigger, not a working branch: never
develop on it and never check it out to do work.

## The two jobs, and why they are split

`.github/workflows/release-flow.yml` runs two jobs with different authority. The split is a safety
property, not an implementation detail — preserve it.

**`cut`** has `contents: read`, no long-lived secret, and runs no model. It executes
`examples/release-cut.flux`, a host-only Flux program with no `task`, network, general process, or
general write operation. That program calls `release_plan` to derive the bump, `release_verify_versions`
to check the independently versioned protocol crates, and `release_cut` to delegate to
`scripts/cut-release.sh`. Its entire product is a git bundle containing one cut commit and one
annotated local tag. **It cannot publish.**

**`release-control`** is the only core-release job that receives `RELEASE_TOKEN`. It builds nothing,
runs no model, and signs nothing. It trusts nothing the cut job reports: `scripts/promote-release-flow.sh`
re-derives the version, tag and every SHA from the bundle's own objects and checks them against live
GitHub state before any mutation.

## What promotion does, in order

1. Import and verify the bundle; refuse a dirty tree.
2. Confirm the cut's parent is the trigger commit, and that the trigger is a two-parent merge whose
   tree matches its second parent. `release` contributes ancestry only, never a second
   implementation line.
3. Confirm no `release-cuts/`, `release-candidates/` or target tag ref already exists.
4. Push `release-cuts/vX.Y.Z`, dispatch `ci.yml` against it, and watch it to green.
5. Three-way merge the cut onto live `main` in an isolated index, then **push that merge to `main`**.
   The pipeline writes `main` itself; do not race it.
6. Stage `release-candidates/vX.Y.Z`, dispatch `release.yml`, and verify its immutable receipt.
7. Create the annotated tag at the merged SHA and push it with the PAT. **The tag push is what
   starts publication** — it triggers `release.yml` and `crates-io.yml`.
8. Watch both, then run `scripts/verify-github-release.sh` and `scripts/check-release-tags.sh`.
9. Delete the candidate and cut branches.

## Version derivation

`derive_bump` reads the commit range since the last reachable version tag. A breaking marker in any
commit subject or footer means `minor` while the version is `0.y`, and `major` after 1.0. No
breaking marker means `patch`. **Minor is the breaking signal.**

The bump is computed from the `release` branch's own history and its own `Cargo.toml`. If `release`
is behind `main`, the derived version is wrong in a way nothing detects until step 5 fails on a
manifest conflict. **Confirm `release` is a descendant of `main` before advancing it.**

## Advancing `release` correctly

The trigger commit must be a two-parent merge whose tree equals its second parent's, and that parent
must be an ancestor of canonical `main`. A plain merge of `main` into `release` satisfies this
exactly:

```bash
git worktree add --detach <scratch dir> origin/release
cd <scratch dir>
git merge --no-ff origin/main
# verify before pushing: three parent fields, parent 2 is main, trees equal
git rev-list --parents -n1 HEAD
git rev-parse HEAD^{tree} origin/main^{tree}
git push origin HEAD:refs/heads/release
```

## When a release fails

Cleanup of `release-cuts/*` and `release-candidates/*` runs only on the success path, but the entry
checks refuse to start when those refs exist. **A failed release therefore blocks its own retry.**
The promotion script prints an exact resume command on failure; use it, or delete the leftover refs
for that version once you have confirmed their commits are contained in `main`.

Two states worth recognising:

- **A tag with no GitHub Release.** The `Release` workflow can succeed at *building* while its
  publish steps are skipped, leaving staged assets and no public release. `scripts/check-release-tags.sh`
  fails naming the tag. Either backfill from the runbook, or delete the tag and re-cut — the content
  ships in the next version either way.
- **A stale `release` branch.** Merging an old release-source branch cuts from stale content.
  Verify ancestry first; the conflict that eventually catches it costs a full CI run.

## Related

- `scripts/cut-release.sh` — the transactional cut. Stages only release files, so concurrent
  uncommitted work is never swept in. Safe to re-run after a red gate. Does not push.
- `scripts/verify-github-release.sh` — the closed-set check for the one tag being cut.
- `scripts/check-release-tags.sh` — the weaker fleet-wide audit that runs on every push to `main`.
- `plugins/` is not part of a Flux cut; those crates sit on an independent protocol line.
