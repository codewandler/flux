---
id: C-354
title: Scope promotion and publication tokens to the jobs that consume them
pillar: Core
status: done
priority: 2
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "v0.56.0 blocker — App-only promotion in release-control; tag-only signing/GitHub Release/Cargo publication in release; plugin branch publication removed"
---

# Scope promotion and publication tokens to the jobs that consume them

## Goal

Make every release authority explicit and non-composable: model/build jobs cannot see a write token;
pre-tag promotion uses only the dedicated App inside `release-control`; signing, GitHub Release and
Cargo publication use distinct tag-triggered jobs inside `release`.

## Acceptance

- [x] All four release workflows declare workflow-level `contents: read`. Any other GitHub write
      permission is granted only on the distinct job that consumes it. No workflow-level or
      job-level `env` contains a provider key, `PROMOTION_APP_PRIVATE_KEY`, an App installation
      token, `RELEASE_TOKEN`, `MINISIGN_SECRET_KEY` or `CARGO_REGISTRY_TOKEN`; every long-lived
      secret appears only in the `env`/input of its single consuming step.
- [x] `RELEASE_TOKEN` exists only in a tag-triggered `release`-environment GitHub Release
      create/upload step. It is absent from `.github/workflows/release-flow.yml` and from every
      candidate, pull-request, merge and tag-creation step. Tests refuse any use of it to move
      `main`, create/update/delete `release-candidates/*`, create/update/delete a tag, dispatch a
      workflow or mint another credential.
- [x] `.github/workflows/release-flow.yml` separates these boundaries:
  - preview, smoke, scribe and local cut use only the selected model secret at step scope and have no
    GitHub write token;
  - one narrow job names `release-control`, passes `PROMOTION_APP_PRIVATE_KEY` only to its token-mint
    step with `PROMOTION_APP_ID`, and uses the resulting `flux-release-promoter` installation token
    only to push the cut branch, open the normal PR to `main`, observe/merge that PR after its exact
    head `ci`, create the candidate ref, dispatch/observe its workflow, create the final tag and,
    only after C-516's live/fleet gates pass, delete the candidate ref;
  - no model, build, attestation, signing or publication job can reference the key or token.

      The promotion job never pushes directly or force-pushes to `main`. It takes the candidate and
      eventual tag SHA only from the merged PR's resulting canonical `main` commit. The installation
      token is never persisted as an artifact, output beyond the job or reusable secret.
- [x] `.github/workflows/release.yml` leaves plan, candidate resolution, target/global builds,
      receipt recording, candidate byte verification and hosting read-only and secret-free. A
      separate tag-triggered `release`-environment attestation job has only `id-token: write` and
      `attestations: write`; a later `release`-environment GitHub Release job receives only
      `RELEASE_TOKEN` at the create/upload step. A manual candidate dispatch cannot enter either
      signing/publication job even if an input is forged.
- [x] `.github/workflows/release-plugins.yml` publishes only on a push of an exact
      `plugins-v[0-9]+.[0-9]+.[0-9]+` tag. A retained `workflow_dispatch` is structurally a
      build/validation path only: it has no `publish` input and cannot enter `release` or
      `release-control`, mint an App token, create a tag, sign, create/upload a GitHub Release or
      publish a crate. Dry run stops after its secret-free artifacts. Separately, a successful
      `workflow_run` of the exact required `ci` workflow on protected `main` may enter a narrow
      `release-control` job only when the run head SHA still equals canonical `main`, the lockstep
      plugin version is exact and the corresponding tag is absent; that job mints the App token and
      may create that plugin tag once at the validated canonical-main SHA. The tag event then
      starts distinct publication jobs after the five-target secret-free build: secret-free index assembly,
      `release`-environment minisign signing with `MINISIGN_SECRET_KEY` only on the sign step,
      GitHub Release publication with `RELEASE_TOKEN` only on the create/upload step, and host-kit
      Cargo publication with `CARGO_REGISTRY_TOKEN` only on the publish step. No job combines these
      authorities.
- [x] `.github/workflows/crates-io.yml` publishes only on a push of an exact
      `v[0-9]+.[0-9]+.[0-9]+` tag. Checkout, toolchain install, version validation and packaging are
      secret-free. Its isolated `release`-environment publish job exposes
      `CARGO_REGISTRY_TOKEN` only on the `scripts/publish-crates-io.sh` step; no branch/manual event,
      prior build/validation step, workflow/job environment or unrelated job can read it.
- [x] Failing-first policy tests parse Actions YAML into workflow/job/step structure (including
      aliases, expressions, inherited permissions, `on`, `if`, `environment`, `needs`, `uses`,
      action inputs and `env`) rather than grepping text. Fixtures fail for workflow/job secret
      scope, inherited write permission, model/build plus write-token co-residence, mixed
      `release-control`/`release` use, `RELEASE_TOKEN` promotion, App-token publication, missing
      environment, combined signing/GitHub/Cargo authority, or a secret referenced outside its one
      authorized step.
- [x] Trigger/flow fixtures fail for direct or force push to `main`, candidate/tag creation before
      the cut PR is merged, a candidate/tag SHA different from the returned merged `main` SHA, a
      branch/manual plugin signing/publication path, any manual plugin tag-creation path, a plugin
      controller run whose `ci` conclusion, head branch or head SHA does not match protected current
      `main`, a tag publication job reachable from `workflow_dispatch`, or a plugin/core tag accepted
      by the wrong workflow.
- [x] The parsed policy test covers the complete current inventory — `release.yml`,
      `release-flow.yml`, `release-plugins.yml` and `crates-io.yml` — and fails when another release
      workflow is added without an explicit disposition. Existing pinned-action and release-policy
      checks stay green; no text-order or naming-only substitute is accepted.

## Progress

- 2026-08-05 — implemented. `scripts/check-release-authority.sh` parses all four release workflows
  (aliases resolved) into workflow/job/step structure and binds every long-lived credential to one
  authorized step; run failing-first against the pre-change tree it reported 34 violations, including
  `release-flow.yml: job 'cut' runs a model credential beside release authority RELEASE_TOKEN` and
  `release.yml: job 'host' holds write permission attestations, contents, id-token`. Twenty-one
  structural fixtures now reject each named violation class. `release.yml` splits `host` into
  read-only assembly, a `release`-environment `attest` job holding only `id-token`/`attestations`
  write, and a `release`-environment `publish-github-release` job whose create/upload step is the
  only place `RELEASE_TOKEN` exists. `release-flow.yml` hands the cut to a narrow `release-control`
  job as a git bundle (`scripts/bundle-release-cut.sh`) and mints the App token in one step
  (`scripts/mint-promotion-token.sh`); `RELEASE_TOKEN` is gone from that workflow. `release-plugins.yml`
  publishes only on an exact `plugins-v` tag, whose creation is `scripts/plugin-tag-control.sh` under
  a `workflow_run` of `ci` on current canonical `main`; `crates-io.yml` lost `workflow_dispatch` and
  split into secret-free `validate` plus a `release`-environment `publish`.
- 2026-08-05 — scope note: the `release-control`/`release` environments, the `flux-release-promoter`
  installation and the `PROMOTION_APP_ID` variable are C-353's external configuration and do not yet
  exist on the repository. Every clause of this story is about workflow/job/step structure and is
  satisfied and enforced offline, but the pipeline cannot execute end to end until C-353 lands.
- 2026-08-01 — filed from the job-by-job trust graph built during validation.
- 2026-08-04 — contract raised to `ready` after re-reading all four live workflows. The current
  concentration includes `release.yml`'s host job, `release-flow.yml`'s PAT-bearing cut/promotion
  job, `release-plugins.yml`'s branch-dispatched assemble/publish job and `crates-io.yml`'s job
  environment. These are observed pre-implementation facts, not accepted future paths; every
  Acceptance item remains open.

## Notes

- GitHub permissions are job-scoped, while secrets can be step-scoped. Separate jobs are required
  wherever GitHub write authority must not coexist with model, build, signing or registry work.
- C-353 owns the App, cumulative rulesets, environments and external secret entry. C-355 owns the
  candidate receipt. C-516 owns exact PR/run/tag/public ordering. This story owns trigger and token
  placement.
- D-46's checked Acceptance records the historical branch-dispatched plugin pipeline that shipped.
  This open story supersedes only its publication trigger/authority contract; it does not claim that
  the tag-triggered replacement has been implemented.
