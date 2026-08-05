---
id: C-559
title: "Release Flux without dedicated App or environment settings"
pillar: Core
status: done
priority: 0
areas: [release, workflows, release-policy]
depends_on: [C-558]
note: "user-directed v0.56.0 unblock — use the existing repository RELEASE_TOKEN; remove PROMOTION_APP and release environment configuration"
---

# Release Flux without dedicated App or environment settings

## Goal

Keep release mutation outside model and build jobs while removing the unconfigured
`flux-release-promoter` App, `PROMOTION_APP_ID`, `PROMOTION_APP_PRIVATE_KEY`, `release-control`
environment and `release` environment from the Flux release path.

## Acceptance

- [x] Failing first, release-authority fixtures demonstrate that canonical `main` cannot currently
      complete a release without the absent App variable/key and environments. The final fixtures
      require all four release workflows to contain no `PROMOTION_APP_ID`,
      `PROMOTION_APP_PRIVATE_KEY`, App-token minting, `environment: release-control`, or
      `environment: release` dependency.
- [x] `.github/workflows/release-flow.yml` keeps model/smoke/scribe/cut work credential-free and
      passes the existing repository Actions secret `RELEASE_TOKEN` only to the separate host-owned
      promotion step. No workflow-level or job-level secret environment is introduced.
- [x] `scripts/promote-release-flow.sh` uses `RELEASE_TOKEN` for the exact cut ref, pull request,
      merged-main candidate ref, annotated tag and final cleanup mutations, while the ambient
      `GITHUB_TOKEN` remains limited to candidate dispatch and read-only Actions observation. A PAT
      push, not `GITHUB_TOKEN`, creates the tag so both tag workflows are triggered. Missing or
      unusable `RELEASE_TOKEN` fails before any promotion mutation; no direct main force-push or tag
      update/recreation path is added.
- [x] `.github/workflows/release-plugins.yml` passes `RELEASE_TOKEN` only to the narrow host-owned
      plugin tag-control step reached from a successful `ci` run for current canonical `main`.
      `scripts/plugin-tag-control.sh` uses that PAT to create the one absent exact plugin tag so the
      tag workflow is triggered; it never accepts `PROMOTION_TOKEN` or `GITHUB_TOKEN` as mutation
      authority.
- [x] App-token minting and revocation code is removed. `release.yml`, `release-plugins.yml` and
      `crates-io.yml` read their already-configured repository/organization secrets directly in the
      one consuming step, without GitHub Environment configuration. The same `RELEASE_TOKEN` may be
      named in the isolated core promotion, plugin tag-control and GitHub Release steps, but never at
      workflow/job scope or beside model, build, signing or Cargo publication work. Planning, builds,
      receipts, verification and attestation remain secret-free.
- [x] Release policy tests parse and enforce the revised step-level boundary, including fixtures for
      a token exposed to a model/build job, workflow/job scope, `GITHUB_TOKEN` tag creation, missing
      PAT tag triggering, mixed Cargo/signing/GitHub publication, or a reintroduced App/environment
      dependency. Existing candidate-v3, exact merged-SHA, 28-asset and latest-release gates remain
      unchanged.
- [x] The release trust design, C-353, C-354, C-516, publishing runbook, changelog and generated
      board explicitly record this user-directed supersession. No live App, environment, ruleset or
      branch-protection setup is required or claimed.
- [x] Focused release-policy checks and the full repository gate pass from the final committed tree;
      the story is done on canonical `main` before the v0.56.0 promotion begins.

## Progress

- Failing-first on dispatched `476fd255`: the new whole-inventory release-authority test failed on
  `release.yml`'s `environment: release` dependency before any workflow/helper implementation
  changed. The same baseline still contained both App settings, both App-token mint steps and the
  deleted mint/revoke helper.
- Implemented the user-directed boundary in one writer worktree. Core promotion preflights and uses
  the step-scoped `RELEASE_TOKEN` for cut/PR/merge/candidate/tag/cleanup mutation while the ambient
  token dispatches and observes Actions only. Plugin tag control uses the same PAT for its one exact
  annotated tag push. All other release secrets remain on their consuming steps, and all four
  workflows have no App or Environment dependency.
- Focused Rust and structural release-authority tests pass, including 23 parsed workflow mutants,
  plugin-control missing/read-only/wrong-token refusals, PAT tag-push proof and the complete C-516
  promotion-order policy.
- On 2026-08-05 live GitHub evidence showed no repository variables, no `release-control` or
  `release` environments, no `flux-release-promoter` installation and no tag rulesets. The user
  explicitly directed the coordinator to remove those settings instead of provisioning them.
- The repository already has `RELEASE_TOKEN`, `MINISIGN_SECRET_KEY`, provider credentials and the
  selected organization `CARGO_REGISTRY_TOKEN`; this story changes their workflow placement, not
  their values.
