---
id: C-719
title: "A tag build that publishes nothing must be red"
pillar: "Core"
status: ready
priority: 5
areas: [workflows, release]
note: "GitHub propagates skipped transitively through needs; the publish chain skipped and the run still reported success, so v0.59.0 has a tag and no Release"
---

# A tag build that publishes nothing must be red

## Goal

`v0.59.0` was tagged, pushed, and its `Release` workflow reported **success** — and no GitHub
Release exists. `gh release list` still shows `v0.58.0` as latest. No binaries, no attestation, no
container image, no announcement.

The run did this without a single failing job:

| job | conclusion |
|---|---|
| `plan` | success |
| `resolve-release-candidate` | success (found candidate run `31192876684`) |
| `build-local-artifacts` | **skipped** |
| `host` | success |
| `build-global-artifacts` | **skipped** |
| `attest` | **skipped** |
| `publish-github-release` | **skipped** |
| `publish-container-image` | **skipped** |
| `announce` | **skipped** |

## Cause

GitHub propagates `skipped` **transitively** down the `needs` graph: a job whose dependency chain
contains a skipped job is itself skipped unless it breaks the chain with `always()`. Breaking it at
one hop is not enough — it must be broken at *every* hop.

Skipping `build-local-artifacts` is correct here. It is precisely what promoting a prepared
candidate is supposed to do (`build-local-artifacts` at `release.yml:264` runs only when
`resolve-release-candidate` found no candidate). `host` breaks the chain correctly with `always()`
plus explicit `skipped || success` result checks. `attest`, `publish-github-release` and
`publish-container-image` each carried a bare `if: needs.plan.outputs.publishing == 'true'` with no
`always()`, so the skip flowed through a *successful* `host` and took the whole publish chain with
it.

A run where every publish job skipped is reported by GitHub as `success`. So "the release workflow
passed" carried no information about whether a release existed.

## Acceptance

- [ ] `attest`, `publish-github-release` and `publish-container-image` break the skip chain with
      `always()` and assert their real upstreams succeeded — admitting a transitively-skipped graph,
      never a failed or skipped dependency.
- [ ] A `verify-published` job fails the run when `plan.outputs.publishing == 'true'` and
      `publish-github-release` did not succeed, so a publish-nothing tag build can never be green
      again. This is the story's failing-first evidence: the job must fail on the v0.59.0 job
      results and pass on a run that actually published.
- [ ] Promoting a prepared candidate still skips the build jobs and still publishes — the fix must
      not force a rebuild on the promote path.
- [ ] `v0.59.0` ends with a real GitHub Release carrying binaries and attestation, or is superseded
      by a version that does.
- [ ] The gate is green in both workspaces.

## Notes

The `host` job's own comment already records the previous instance of this class: v0.47.0 shipped a
Release whose only asset was `dist-manifest.json`, because the builds skipped and `host` ran anyway.
That was fixed by hardening `host`. The same reasoning was never applied to the jobs below it.

Related: the `crates.io` job failed on this tag for an unrelated reason (`flux-flow` referencing
`flux_evidence::KIND_BUDGET_PROJECTION`, absent from the published `flux-evidence 1.1.0`); the
`flux-evidence` 1.2.0 bump landed in `aaa42b2e`.
