---
id: C-515
title: "Make embedded documentation freshness a mandatory PR and release gate"
pillar: Core
status: done
priority: 0
areas: [ci, release, website, docs]
note: "PR website workflow currently admits stale public-docs.zip — regenerate, commit, then check the archive in every PR and publication path"
---

# Make embedded documentation freshness a mandatory PR and release gate

## Goal

Make `crates/flux-server/assets/public-docs.zip` a deterministic committed mirror of the public
website for every Flux pull request, release candidate and website publication. Contributors and
release automation regenerate it with `scripts/build-embedded-docs.sh`, include any changed archive
in the same commit, and prove the committed checkout fresh with
`scripts/build-embedded-docs.sh --check` before publication.

## Acceptance

- [x] A failing-first workflow contract proves the current always-on pull-request and exact-SHA
      release gates can accept a stale embedded-doc archive, and fails if any governed PR, release or
      website-publication path later loses its freshness check.
- [x] `AGENTS.md`, contributor guidance and publishing documentation require this exact ordered
      sequence: run `scripts/build-embedded-docs.sh`, commit
      `crates/flux-server/assets/public-docs.zip` when it changes, then run
      `scripts/build-embedded-docs.sh --check` against the committed checkout before opening a PR.
- [x] Every pull request runs an unfiltered embedded-doc freshness gate with the pinned Node/npm
      website dependencies installed; a change outside `website/**` cannot bypass it.
- [x] The exact-SHA release-candidate gate provisions the same website dependencies and runs
      `scripts/build-embedded-docs.sh --check` after the release archive commit and before writing a
      candidate receipt, building release artifacts or publishing anything.
- [x] The website workflow runs the freshness check before artifact upload or deployment for every
      applicable pull-request, main, release and manual-publication event; pull requests remain
      build-only and never deploy.
- [x] Release-cutter contract tests retain the regenerate-and-commit transaction, assert that
      `crates/flux-server/assets/public-docs.zip` belongs to the release commit, and prove the
      post-commit exact-SHA freshness check cannot be skipped.
- [x] The generated story board, engineering changelog and embedded archive are current. Relevant
      workflow tests and the full repository gate pass, including a final regenerate, archive commit
      when changed, and `scripts/build-embedded-docs.sh --check` before the implementation PR.

## Progress

- 2026-08-04: Added `scripts/test-embedded-docs-gates.sh` failing-first: it initially failed because
  the unfiltered CI workflow had no pinned Node/npm setup or archive check. The repaired contract
  covers CI, exact-SHA candidate, website upload/deploy, release-flow/cutter ordering and all four
  contributor/release guidance surfaces; the existing release-candidate test now pins archive
  membership in the release commit and post-commit gate ordering.
- 2026-08-04: The unfiltered PR job and exact-SHA candidate now install `website/package-lock.json`
  under Node 22 and verify `public-docs.zip` before the repository gate, dist planning, builds or
  receipt. The real pinned website build, archive check, YAML parse, shell syntax, action-pin and
  workflow contract checks pass; the generated archive was already byte-current.
- 2026-08-04: Implementation started from dispatched `origin/main` commit `7d2657d5`; auditing the
  always-on pull-request, exact-SHA release and website-publication gates with failing-first contract
  coverage before changing workflow behavior.
- 2026-08-04: Filed from the observed PR website failure and canonical audit at `df4672aa`: the
  website workflow checks freshness only behind path filters, while the universal pull-request and
  exact-SHA release gates do not provision website dependencies and check the committed archive.

## Notes

- `scripts/cut-release.sh` already owns regeneration and inclusion of `public-docs.zip` in the
  release commit. Preserve that transaction and add the post-commit proof at every publication gate.
- This story hardens the C-498 mechanism across workflow triggers; it does not reopen C-498's fixed
  GitHub Pages environment determinism defect.
