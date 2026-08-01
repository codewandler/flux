---
id: C-412
title: "Nothing mechanically guarantees a published Release has assets, and v0.47.0 proved it"
pillar: Core
status: in-progress
priority: 5
epic: release-trust-residuals
areas: [ci, docs]
note: "F6 of the 2026-08-01 security-posture review at 0.47.1, with live proof. `dist host --steps=create` creates the Release in the FIRST job, before any artifact exists; the asset check runs in the LAST. Anything that skips in between leaves a published Release advertising 404s — which is exactly what v0.47.0 shipped"
---

# A Release is created before anything is built

## Goal

Make an asset-less published Release impossible, or detected — not allowlisted.

`dist host --steps=create` runs in the **plan** job, the first job, on both dispatch and tag events
(`.github/workflows/release.yml:113`), and creates the GitHub Release object **before a single
artifact exists**. `verify-github-release.sh`, which does check the asset set and the attestations,
runs only in the **host** job at the very end (`release.yml:479`). Anything that fails or skips in
between leaves a published Release advertising downloads that 404.

⚠ **This is not hypothetical — v0.47.0 shipped exactly that.** Candidate run `30700607303` concluded
**success** with `build-local-artifacts`, `build-global-artifacts`, `record-release-candidate` and
`host` all *skipped*, and the GitHub API reports no artifacts for that run at all. **A run whose
build jobs all skip has no failing job, so `success` is not evidence that anything was built.**

The fleet-wide audit cannot catch the result: `scripts/check-release-tags.sh` queries only
`.tag_name` (`:255`, `:257`, `:301`) — existence, never asset counts.

⚠ **The remediation applied at the time was an allowlist entry, not a guard**: `v0.47.0` was added to
`ALLOWED_WITHOUT_RELEASE` (`scripts/check-release-tags.sh:76`). That un-breaks the audit without
closing the hole. This story closes the hole.

**Already defended, so do not re-do it:** the promote side now requires the receipt plus
`artifacts-build-global` plus ≥5 `artifacts-build-local-*` before promoting
(`scripts/find-release-candidate.sh:38`), and running it against v0.47.0's SHA correctly returns
nothing.

## Acceptance

- [x] **Failing-first**: a check that fails against v0.47.0's shape — a Release whose asset set is
      empty or short of the expected count — and passes against v0.46.0's 28 assets.
      → `scripts/check-release-tags.sh` rule 3. Driven through a stub `gh` serving a two-release
      fleet, the pre-change script PASSes on `/releases/latest` = v0.47.0 with only
      `dist-manifest.json`; the post-change script FAILs naming the five missing install assets, and
      PASSes when the same tag is given v0.46.0's 28.
- [x] `check-release-tags.sh` compares **asset counts**, not just tag existence, so an already-published
      assetless Release is detected fleet-wide.
      → `install_asset_gaps` (`scripts/check-release-tags.sh:160`) compares asset *names*, and
      reports the count in the failure text. Names rather than a count because the shipped set has
      been 16, 27 and 28 assets across history and all three are installable; the failure line still
      leads with "1 asset(s)", which is the fact a human recognises.
- [x] A candidate run that builds nothing cannot report a usable `success`: either the workflow fails
      when its build jobs skip while `preparing`, or the receipt is the only signal anything trusts.
      → `record-release-candidate` now runs whenever the workflow is preparing and fails if either
      build job did not succeed (`.github/workflows/release.yml:347`). It previously gated *itself*
      on those builds, so a run that built nothing skipped it and concluded `success`.
- [x] ⚠ Decide whether the Release object should be created **after** artifacts exist rather than in
      the first job. That is the root cause; the checks above are detection.
      → **Decided: yes, moved.** `--steps=create` is gone from `plan` (`release.yml:122`) and joins
      `upload`/`release` in the `host` job (`release.yml:479`), after the artifacts are fetched and
      attested. See Progress for the reasoning and the residual risk.
- [x] `v0.47.0`'s allowlist entry is revisited — kept with a stated reason, or removed once the guard
      makes it unnecessary.
      → **Kept, and given the reason it never had.** The entry was undocumented; the comment block
      above it named only v0.11.1/v0.12.0/v0.17.0. It is load-bearing: v0.47.0's Release object was
      deleted by hand, so the tag genuinely has none, and no guard can retroactively build binaries
      that were never compiled.
- [x] The check has a `--self-test`, like its siblings, and is verified to fire.
      → `check-release-tags.sh --self-test` gains rule 3's fixtures, all taken from real listings
      (v0.47.0's 1, v0.3.0's 16, v0.27.0's 27, v0.46.0's 28). `check-release-integrity.sh --self-test`
      gains three bad fixtures that revert each half of the workflow fix; each was run standalone and
      shown to abort with its own message, not on incidental YAML damage.

## Notes

- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F6.
- The 0.47.1 CHANGELOG entry states the limitation honestly and is the narrative record of what
  happened; this story is the mechanical fix.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
- **The proximate cause of the skip was found in the live run log, and it is not what the review
  assumed.** `dist host --steps=create` emitted a perfectly good manifest — `artifacts_matrix.include`
  held all five targets. The manifest never reached the build jobs: the plan job's last log line is
  `##[warning]Skip output 'val' since it may contain secret`. The full manifest embeds the release
  body, i.e. the 0.47.0 CHANGELOG entry, which contains a string GitHub masks (`CHANGELOG.md:85`
  and `:103` render as `***` in the run log). Actions discards an output that matches a mask —
  as a **warning**, not an error — so `fromJson(needs.plan.outputs.val)` had nothing, every build
  job's `if` evaluated false, and the matrix job never expanded (it appears in the run as the
  unexpanded `build-local-artifacts (${{ join(matrix.targets, ', ') }})`).
  Consequence for this story: **the trigger is arbitrary prose in a CHANGELOG entry.** No targeted
  fix to the plan step could be trusted, which is why the fix is four independent layers rather
  than one.
- The tag run (`30700632185`) then did the rest: plan created/refreshed the Release again, builds
  skipped again, `host` ran because its `if:` tolerates `skipped`, downloaded nothing, and step 13
  `Create GitHub Release` **succeeded** — publishing the Release with `dist-manifest.json` as its
  only asset. Step 14, `verify-github-release.sh`, failed. The last line of defence fired exactly
  as designed and was still too late by one step.
- **The call on creating the Release after artifacts exist: yes.** `--steps=create` moved out of
  `plan` and into the `host` job's existing `dist host` invocation, beside `upload` and `release`.
  Reasoning: nothing on either path needs hosting state before `host` — the build matrix comes from
  the manifest, the build jobs take `--tag` directly, `record-release-candidate` only writes a
  receipt, and the installer download URLs are derived from the tag (github hosting), not from the
  Release object. Detection alone was rejected because it leaves the object published: a check can
  tell you `/releases/latest` is broken, it cannot un-point it.
  ⚠ **Residual risk, stated rather than hidden: this path cannot be executed without cutting a
  release.** What is verified is that `dist plan --tag=vX.Y.Z` yields the same 5-target
  `artifacts_matrix` the build matrix consumes, and that it keeps the tag/version validation
  (`--tag=v9.9.9` fails with *"This workspace doesn't have anything for dist to Release!"*). What is
  **not** verified is `dist host --steps=create --steps=upload --steps=release` as a single
  invocation. If dist rejects it, the failure mode is a red `host` job with **no Release object
  created at all** — the safe direction, and the one C-47 asks for.
- The `plan` job's `val` output now carries only `ci.github.artifacts_matrix` and
  `ci.github.pr_run_mode` — the only two fields any consumer reads — instead of the whole manifest.
  20884 bytes of manifest including the release prose becomes 1826 bytes with none, so it cannot
  collide with a secret mask again. The full manifest is still uploaded as the
  `artifacts-plan-dist-manifest` artifact, which is how the build jobs actually consume it.
- `scripts/check-release-integrity.sh` locks the new shape structurally. release.yml is
  cargo-dist-generated and `dist generate` writes `--steps=create` back into `plan`, so the ordering
  is a gate rather than a comment.
- Rule 3 has a version floor, `INSTALLABLE_SINCE=v0.3.0`, not an allowlist: eight pre-0.3 dev-era
  Release objects (v0.1.0, v0.1.1, v0.2.0, v0.2.5–v0.2.8, v0.2.16) carry zero assets and predate
  cargo-dist hosting. Every `vX.Y.Z` at or after v0.3.0 is held to the full rule with no exceptions
  and nowhere to add one; the live audit passes over all 107 released version tags.
