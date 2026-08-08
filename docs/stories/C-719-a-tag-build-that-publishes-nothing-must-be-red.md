---
id: C-719
title: "A tag build that publishes nothing must be red"
pillar: "Core"
status: done
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

- [x] `attest`, `publish-github-release` and `publish-container-image` break the skip chain with
      `always()` and assert their real upstreams succeeded — admitting a transitively-skipped graph,
      never a failed or skipped dependency.
      → `release.yml:658`, `:686`, `:780` (landed in `e353d528`). Held structurally by
      `scripts/check-publish-chain.sh`, whose `attest-loses-always`,
      `publish-github-release-loses-always`, `publish-container-image-loses-always`,
      `announce-loses-always`, `attest-admits-a-failed-host`,
      `publish-github-release-admits-a-failed-attest` and
      `container-image-admits-an-unpublished-release` fixtures each restore one of these holes and
      are each rejected.
- [x] A `verify-published` job fails the run when `plan.outputs.publishing == 'true'` and
      `publish-github-release` did not succeed, so a publish-nothing tag build can never be green
      again. This is the story's failing-first evidence: the job must fail on the v0.59.0 job
      results and pass on a run that actually published.
      → `release.yml:908`. `scripts/check-publish-chain.sh` **executes that job's own step script**
      with `needs.publish-github-release.result` substituted: it must exit non-zero for `skipped`,
      `failure` and `cancelled`, and zero for `success`. The `verify-published-deleted`,
      `verify-published-gated-on-the-result-it-checks`, `verify-published-accepts-a-skipped-publish`,
      `verify-published-assertion-made-conditional` and `verify-published-needs-a-checkout` fixtures
      cover the ways it stops being a backstop.
- [x] Promoting a prepared candidate still skips the build jobs and still publishes — the fix must
      not force a rebuild on the promote path.
      → confirmed in production: runs `31246987406` (v0.59.1) and `31251445072` (v0.59.2) both
      skipped `build-local-artifacts` and `build-global-artifacts` and both published 28 assets.
      Held by the model's liveness assertion — on the promote path every authority job and the
      backstop must conclude `success` — and by `promote-path-forces-a-rebuild`, which rejects the
      "just rebuild on the tag" escape that would satisfy every skip rule while discarding
      build-once.
- [x] `v0.59.0` ends with a real GitHub Release carrying binaries and attestation, or is superseded
      by a version that does.
      → superseded. `v0.59.1` and `v0.59.2` each carry the exact 28-asset inventory, and
      `/releases/latest` is `v0.59.2`. `v0.59.0` remains a tag with no Release.
- [x] The gate is green in both workspaces.
      → `scripts/release-full-gate.sh` green on `impl/C-719`; see Progress for the exact line.

## Progress

- 2026-08-08 — the workflow half had already landed in `e353d528`; what was missing was the
  assertion that keeps it. Added `scripts/check-publish-chain.sh` and wired it into `ci.yml`'s
  `action-pins` job beside the other release guards.

  GitHub Actions YAML cannot be unit-tested by running it, so the script models the one scheduling
  rule this defect lives in — **transitive** skip propagation over the `needs` closure, with
  `always()` as the only thing that stops it — and schedules the *committed* `release.yml` through
  it. Every `if:` is read from the file, never restated, and the publish chain is derived (every job
  with `host` in its `needs` closure) rather than listed, so a publish job added later is covered
  the day it lands. A condition outside the tiny supported grammar aborts the check instead of being
  guessed at.

  Three kinds of assertion: structural (`always()` at every hop, real upstream success asserted,
  the backstop gated on nothing but `publishing`), executed (the backstop's real script, run), and
  simulated (216 publishing runs — every `host` × `build-local` × `build-global` result crossed
  with every subset of the authority jobs failing — must satisfy *run concluded success ⇒
  publish-github-release succeeded*, plus the liveness half so "fail every tag run" is not a pass).

  Fidelity, not just strictness — the model was checked against real runs of this workflow in both
  directions:

  | run | workflow | model predicts | GitHub did |
  |---|---|---|---|
  | `31196060862` (v0.59.0) | `release.yml` at `e353d528^` | attest / publish-github-release / publish-container-image / announce `skipped`, conclusion `success` | identical — and no Release |
  | `31246987406` (v0.59.1) | as committed | all five `success` | identical, 28 assets |
  | `31251445072` (v0.59.2) | as committed | all five `success` | identical, 28 assets |

  Both v0.59.1 and v0.59.2 are promote-path runs with `build-local-artifacts` and
  `build-global-artifacts` `skipped`, so acceptance criterion 3 is confirmed in production and not
  only in the model. `--replay` against the `e353d528^` file also makes the check exit 1, which is
  the failing-first evidence against real history. The self-test asserts the same table from a
  synthetic reconstruction, because CI checks out at depth 1 and cannot read a parent commit.

  What it does not catch: it is a model, not GitHub. It does not validate the Actions schema, does
  not evaluate `fromJson`/`startsWith`, and takes `plan`, `resolve-release-candidate`, the two build
  jobs and `host` as inputs rather than deciding them — which is the form the v0.59.0 evidence
  arrived in. Whether the published *bytes* are right remains `scripts/verify-github-release.sh`.

## Notes

The `host` job's own comment already records the previous instance of this class: v0.47.0 shipped a
Release whose only asset was `dist-manifest.json`, because the builds skipped and `host` ran anyway.
That was fixed by hardening `host`. The same reasoning was never applied to the jobs below it.

Related: the `crates.io` job failed on this tag for an unrelated reason (`flux-flow` referencing
`flux_evidence::KIND_BUDGET_PROJECTION`, absent from the published `flux-evidence 1.1.0`); the
`flux-evidence` 1.2.0 bump landed in `aaa42b2e`.
