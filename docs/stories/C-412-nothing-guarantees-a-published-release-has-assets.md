---
id: C-412
title: "Nothing mechanically guarantees a published Release has assets, and v0.47.0 proved it"
pillar: Core
status: ready
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

- [ ] **Failing-first**: a check that fails against v0.47.0's shape — a Release whose asset set is
      empty or short of the expected count — and passes against v0.46.0's 28 assets.
- [ ] `check-release-tags.sh` compares **asset counts**, not just tag existence, so an already-published
      assetless Release is detected fleet-wide.
- [ ] A candidate run that builds nothing cannot report a usable `success`: either the workflow fails
      when its build jobs skip while `preparing`, or the receipt is the only signal anything trusts.
- [ ] ⚠ Decide whether the Release object should be created **after** artifacts exist rather than in
      the first job. That is the root cause; the checks above are detection.
- [ ] `v0.47.0`'s allowlist entry is revisited — kept with a stated reason, or removed once the guard
      makes it unnecessary.
- [ ] The check has a `--self-test`, like its siblings, and is verified to fire.

## Notes

- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F6.
- The 0.47.1 CHANGELOG entry states the limitation honestly and is the narrative record of what
  happened; this story is the mechanical fix.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
