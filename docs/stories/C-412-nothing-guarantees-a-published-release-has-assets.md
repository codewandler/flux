---
id: C-412
title: "Nothing mechanically guarantees a published Release has assets, and v0.47.0 proved it"
pillar: Core
status: done
epic: release-trust-residuals
areas: [ci, docs]
note: "F6 of the 2026-08-01 security-posture review at 0.47.1, with live proof. `gh release create` publishes the Release and the asset check runs one step LATER in the same job, so a run that built nothing publishes first and discovers it afterwards — which is exactly what v0.47.0 shipped. (The review's `dist host --steps=create` mechanism was measured during implementation and is inert on dist 0.32.0's GitHub backend.)"
---

# A Release is created before anything is built

## Goal

Make an asset-less published Release impossible, or detected — not allowlisted.

`dist host --steps=create` runs in the **plan** job, the first job, on both dispatch and tag events
(`.github/workflows/release.yml:113`), and creates the GitHub Release object **before a single
artifact exists**. `verify-github-release.sh`, which does check the asset set and the attestations,
runs only in the **host** job at the very end (`release.yml:479`). Anything that fails or skips in
between leaves a published Release advertising downloads that 404.

> ⚠ **CORRECTION, measured during implementation — the paragraph above is wrong about the
> mechanism, and the conclusion survives it.** `dist host --steps=create` does **not** create the
> Release: in the pinned dist 0.32.0 its `HostingStyle::Github` arm is empty, and `--steps=upload`
> and `--steps=release` are never read at all. `dist host --steps=create --tag=v0.47.1` and
> `dist plan --tag=v0.47.1` emit manifests differing by two lines, both the build-id string, and
> running the former against the live repo left v0.47.1's Release untouched. The Release is created
> by **`gh release create` inside the `host` job**, which then verifies the result in the *same job,
> one step later* — so the ordering defect is real but sits one level down. v0.47.0's tag run has
> `run_attempt: 2`; attempt 1's `host` ran 12:54:47 → 12:55:08 and the Release published at 12:55:07
> inside it. Everything the Goal asks for still holds: a published Release must not be able to
> advertise 404s. See Progress.

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
      → `install_asset_gaps` (`scripts/check-release-tags.sh:174`) compares asset *names* for the
      exactly-named requirements and *counts* the platform archives (`5 of 5`, measured: 99 of the
      107 Releases carry exactly five and the other eight carry zero, all below the floor). Names
      rather than one total count because the shipped set has been 16, 27 and 28 assets and all
      three install; the failure line still leads with "1 asset(s)".
- [x] A candidate run that builds nothing cannot report a usable `success`: either the workflow fails
      when its build jobs skip while `preparing`, or the receipt is the only signal anything trusts.
      → `record-release-candidate` now runs whenever the workflow is preparing and fails if either
      build job did not succeed (`.github/workflows/release.yml:347`). It previously gated *itself*
      on those builds, so a run that built nothing skipped it and concluded `success`.
- [x] ⚠ Decide whether the Release object should be created **after** artifacts exist rather than in
      the first job. That is the root cause; the checks above are detection.
      → **The premise was wrong, and the answer is better than the one it asked for.** The Release
      was never created in the first job: `dist host --steps=create` is inert on dist 0.32.0's GitHub
      backend. It is created by `gh release create` in `host` — which already ran after the artifact
      download, and then verified the result *afterwards*. So the ordering defect is one level down,
      inside `host`, and the fix is to verify the artifact set **before** publishing it:
      `scripts/verify-github-release.sh --staged artifacts` now runs ahead of both the attestation
      and the create step. See Progress for the evidence and for the earlier wrong answer.
- [x] `v0.47.0`'s allowlist entry is revisited — kept with a stated reason, or removed once the guard
      makes it unnecessary.
      → **Kept, and given the reason it never had.** The entry was undocumented; the comment block
      above it named only v0.11.1/v0.12.0/v0.17.0. It is load-bearing: v0.47.0's Release object was
      deleted by hand, so the tag genuinely has none, and no guard can retroactively build binaries
      that were never compiled.
- [x] The check has a `--self-test`, like its siblings, and is verified to fire.
      → `check-release-tags.sh --self-test` gains rule 3's fixtures, all taken from real listings
      (v0.47.0's 1, v0.3.0's 16, v0.27.0's 27, v0.46.0's 28). `verify-github-release.sh --self-test`
      gains staged-mode fixtures driven through the real entry point against real directories on
      disk. `check-release-integrity.sh --self-test` gains five bad fixtures; each was run standalone
      and shown to abort with **its own** message over a YAML file that still parses, changing 2, 4,
      8, 27 and 2 lines respectively — no incidental damage.

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
- ⚠ **The first attempt at this story blamed the wrong mechanism, and the correction is the useful
  part of the record.** It claimed the Release was created in the `plan` job by
  `dist host --steps=create` and "fixed" it by moving that flag. **In dist 0.32.0 that call creates
  nothing**: its `HostingStyle::Github` arm is empty, and `upload`/`release` are never read at all.
  Measured on this branch — `dist host --steps=create --tag=v0.47.1` and `dist plan --tag=v0.47.1`
  emit manifests that differ by **two lines, both the build-id string** (`plan:all:` vs
  `host:create:all:`); hosting URLs, `artifacts_matrix` and the announcement block are identical.
  Running it against the live repo left v0.47.1's Release untouched (28 assets, `published_at`
  unchanged). Moving an inert flag fixed nothing, and asserting otherwise in a permanent gate would
  have misled the next person to trip it.
- **The real publisher is `gh release create` in the `host` job, and it runs before the verifier.**
  The v0.47.0 tag run (`30700632185`) has `run_attempt: 2`; attempt 1's `host` ran 12:54:47 →
  12:55:08, and the Release published at **12:55:07 — inside it**. So the sequence is: `host`
  downloads artifacts, `gh release create` publishes them, and *then* `verify-github-release.sh`
  checks what came out. On that run the artifact directory held only `dist-manifest.json`; the
  create step **succeeded**, the verify step failed, and the Release was already public with
  `/releases/latest` pointing at it. The later "already exists; refreshing assets with `--clobber`"
  log is attempt 2, a manual re-run, not the publisher.
- **The fix is therefore at that step, not at the job boundary.**
  `scripts/verify-github-release.sh` gains a `--staged <dir>` mode that applies the same asset-set
  rules to the local directory about to be uploaded, and `host` runs it ahead of both the
  attestation and the create step. Same script, same rules, moved to where it can prevent rather
  than report. Proven both ways against real data: a staged directory holding v0.46.0's real 28
  filenames is *publishable: 28 file(s), 14 executable*; one holding only `dist-manifest.json` fails
  with *missing required asset(s): flux-cli-installer.sh flux-cli-installer.ps1 sha256.sum*.
  The core-asset list is now one function called by both modes, so the pre- and post-publication
  checks cannot drift apart.
- `dist plan --tag=` is kept in the planning job, but for the smaller reasons only, now stated as
  such: a planning job should not hold a verb asking to create a public object, and `create` is
  inert by an upstream implementation detail a future dist may fill in. `--steps=create` was
  **reverted** out of the `host` invocation — adding an ignored flag bought nothing and asserted a
  false mechanism.
- The `plan` job's `val` output now carries only `ci.github.artifacts_matrix` and
  `ci.github.pr_run_mode` — the only two fields any consumer reads — instead of the whole manifest.
  20884 bytes of manifest including the release prose becomes 1826 bytes with none, so it cannot
  collide with a secret mask again. The full manifest is still uploaded as the
  `artifacts-plan-dist-manifest` artifact, which is how the build jobs actually consume it.
- The same hazard had a **second instance**: `host.outputs.val` was fed the unfiltered manifest and
  was voided by the same rule on the 0.47.0 run. Nothing consumes it today — the announcement steps
  read the *step* output, which job-output masking does not touch — so it is filtered to
  `{announcement_tag, announcement_is_prerelease}` rather than left as a trap for a first consumer.
- `scripts/check-release-integrity.sh` locks the shape structurally, because release.yml is
  cargo-dist-generated and `dist generate` rewrites it. The invariant it now asserts is the true
  one: **the pre-publication asset check exists, is unconditional, and sits above both the
  attestation and the create step** — plus the post-publication verifier still sits below it. The
  earlier version asserted `--steps=create/upload/release` on the host invocation, i.e. flags dist
  0.32.0 ignores entirely; that assertion is gone. The plan-job rule is narrowed from "no
  `dist host` at all" to "no `--steps=create`", so a legitimate `--steps=check` preflight stays
  possible, and its abort text says outright that this is forward-defence and not the mechanism
  that published v0.47.0.
- Rule 3 has a version floor, `INSTALLABLE_SINCE=v0.3.0`, not an allowlist: eight pre-0.3 dev-era
  Release objects (v0.1.0, v0.1.1, v0.2.0, v0.2.5–v0.2.8, v0.2.16) carry zero assets and predate
  cargo-dist hosting. Every `vX.Y.Z` at or after v0.3.0 is held to the full rule with no exceptions
  and nowhere to add one; the live audit passes over all 107 released version tags.
- Rule 3 counts platform archives rather than requiring "at least one of each kind". The looser rule
  is right for `verify-github-release.sh`, which guards one release against a closed set and cannot
  know how many targets a future config declares — but fleet-wide it would call a release that lost
  four of its five platforms installable. `REQUIRED_PLATFORM_ARCHIVES=5` is measured from the fleet,
  not chosen, and the self-test pins that `flux-lsp` archives do not count towards it.
