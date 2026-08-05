---
id: C-353
title: Protect main and gate release secrets behind protected environments
pillar: Core
status: blocked
priority: 1
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "superseded for v0.56.0 by user-directed C-559 — do not provision the App, variables, environments or rulesets"
---

# Protect `main` and gate release secrets behind protected environments

## Goal

Make repository and publication gates load-bearing without an administrator escape hatch. Give
pre-tag promotion one repository-scoped GitHub App identity, keep it separate from tag-only
publication credentials, and keep reversible configuration separate from external secret entry.

## Acceptance

- [ ] A stable aggregate check with the exact required-check identity `ci` is the sole branch-policy
      interface to the changing workflow matrix. A failing, pending or missing aggregate blocks
      merge; parsed fixtures prove matrix/check-name changes cannot silently remove the aggregate.
- [ ] `main` protection requires a pull request and the `ci` aggregate and refuses direct push,
      force push and deletion. It applies to administrators and custom roles with branch-protection
      bypass authority; its bypass list is empty. The release cut can reach `main` only by opening a
      normal pull request, waiting for `ci` on that exact head and merging through this protection.
      `HEAD:main`, another direct ref update and a force push are never promotion or recovery paths.
      The existing `release` trigger branch is protected by the same PR/`ci`, no-force, no-delete and
      administrator-enforcement posture, so entering the controller is itself a reviewed merge.
- [ ] Two **cumulative active tag rulesets** both have `target=tag`, an empty exclusion list and the
      exact `ref_name.include` patterns `refs/tags/v*` and `refs/tags/plugins-v*`:
  - the creation ruleset contains the `creation` restriction and exactly one bypass actor —
    `actor_type=Integration`, the exact App/integration ID of `flux-release-promoter`, and
    `bypass_mode=always`; no user, team, repository role, organization administrator, deploy key or
    other integration bypass exists;
  - the immutability ruleset contains the `update` and `deletion` restrictions and has
    `bypass_actors=[]`.

      The cumulative result is mechanical: only `flux-release-promoter` can create a matching tag,
      once created neither the App nor any administrator can update, force-update or delete it.
      Exact workflow validators separately admit only `v[0-9]+.[0-9]+.[0-9]+` or
      `plugins-v[0-9]+.[0-9]+.[0-9]+`; GitHub's broader `v*`/`plugins-v*` ref globs never authorize a
      malformed release.
- [ ] The private GitHub App is named exactly `flux-release-promoter`, is installed on exactly
      `codewandler/flux`, and requests only Metadata read, Contents read/write, Actions read/write
      and Pull requests read/write. It requests no organization or account permissions, user
      authorization/OAuth-on-install, device flow or webhook. Value-free evidence records the App
      and installation IDs, selected repository and exact permission map.
- [ ] Repository Actions configuration contains non-secret `PROMOTION_APP_ID`. The separate
      `release-control` environment contains only the environment secret
      `PROMOTION_APP_PRIVATE_KEY`, disallows administrator protection-rule bypass, and has
      `deployment_branch_policy={protected_branches:true,custom_branch_policies:false}`. The narrow
      core promotion job additionally requires the automatic `push` event at exact
      `refs/heads/release`; the narrow plugin promotion job additionally requires a successful
      `workflow_run` of the exact required `ci` workflow whose head branch is `main`, whose head SHA
      equals the current canonical `main` SHA, and whose plugin version has no existing exact tag.
      Both branches are protected as specified above. No manual dispatch, tag, pull-request head,
      arbitrary branch or unprotected ref can enter the environment.
- [ ] The `release` environment disallows administrator protection-rule bypass and uses selected
      branches/tags (`protected_branches=false`, `custom_branch_policies=true`) with exactly two
      deployment policies: `{type: tag, name: v*}` and `{type: tag, name: plugins-v*}`. It has no
      branch policy. Every signing or publication job names `release` and rejects its input unless
      the tag is the exact core or plugin SemVer form assigned to that workflow. A branch or manual
      dispatch cannot access its secrets or publish.
- [ ] The promotion App installation token is minted only inside the narrow `release-control`
      promotion job for the applicable core or plugin invocation. `PROMOTION_APP_PRIVATE_KEY`
      appears only on token-mint steps, tokens are neither persisted nor uploaded, and job teardown
      revokes or lets them expire. Model, build, attestation, signing, GitHub Release and Cargo
      publication jobs cannot reference the key or installation token.
- [ ] Repository Actions defaults are `default_workflow_permissions=read` and
      `can_approve_pull_request_reviews=false`; parsed workflow tests prove a future workflow cannot
      inherit write authority or approve its own pull request.
- [ ] The reversible repository wave configures/tests the aggregate check, branch and cumulative tag
      protection, both environments, App metadata/installation and Actions defaults without reading,
      logging, exporting or deleting any credential value.
- [ ] A secure maintainer who already possesses each original publication credential, or first
      revokes it and mints a replacement, enters `RELEASE_TOKEN`, `MINISIGN_SECRET_KEY` and
      `CARGO_REGISTRY_TOKEN` as environment secrets on tag-only `release`. The App owner separately
      generates a private key and enters it as `PROMOTION_APP_PRIVATE_KEY` on `release-control`.
      Automation never claims GitHub can read, copy or recover an existing secret value.
- [ ] `RELEASE_TOKEN` is documented and tested as solely the GitHub Release create/upload
      credential. It is absent from `release-control`, never mints or substitutes for the App token,
      and never moves `main`, a candidate ref or a tag. `PROMOTION_APP_PRIVATE_KEY` and the
      installation token are absent from tag-only `release`.
- [ ] Value-free evidence records the exact environment secret names and updated metadata, absence
      of `RELEASE_TOKEN` and `MINISIGN_SECRET_KEY` from repository secrets after successful re-entry,
      and removal of Flux from the organization `CARGO_REGISTRY_TOKEN` selection. Environment-gated
      dry runs prove availability without printing values; a failure retains the old source secret
      and stops migration rather than guessing.
- [ ] The implementation records and runs these value-free queries (or equivalent paginated
      queries), expands each ruleset by ID, and asserts exact fields rather than names alone:

      ```bash
      gh api repos/codewandler/flux/branches/main/protection
      gh api repos/codewandler/flux/rulesets
      gh api repos/codewandler/flux/environments/release-control
      gh api repos/codewandler/flux/environments/release-control/deployment-branch-policies
      gh api repos/codewandler/flux/environments/release
      gh api repos/codewandler/flux/environments/release/deployment-branch-policies
      gh api repos/codewandler/flux/actions/permissions/workflow
      gh api repos/codewandler/flux/environments/release-control/secrets \
        --jq '[.secrets[].name] | sort'
      gh api repos/codewandler/flux/environments/release/secrets \
        --jq '[.secrets[].name] | sort'
      gh api repos/codewandler/flux/actions/variables/PROMOTION_APP_ID
      gh api repos/codewandler/flux/actions/secrets --jq '[.secrets[].name] | sort'
      gh api orgs/codewandler/actions/secrets/CARGO_REGISTRY_TOKEN/repositories \
        --jq '[.repositories[].full_name] | sort'
      ```

      Evidence also records the GitHub App installation and permissions without requesting a
      private key or installation token. No command requests or prints a secret value.

## Progress

- 2026-08-05 — superseded by C-559 at the user's direction. The repository has none of the App,
  variable, environment or ruleset configuration this contract requested; it must not be provisioned
  for v0.56.0. C-559 preserves step-isolated host mutation with the existing `RELEASE_TOKEN` and
  removes these external settings from the executable release path. The unchecked Acceptance below
  remains the historical hardening proposal, not authorized implementation work.
- 2026-08-01 — filed from validation of REL-01/ASSURE-04.
- 2026-08-04 — contract raised to `ready` against canonical
  `9e3108b1b6856e30fa2e0baa2475d75d21fbc19f` after PR #29. Live evidence still shows no branch
  protection, ruleset, `release-control` or `release` environment; Actions defaults remain write +
  workflow PR approval. The three existing publication credentials require later secure external
  re-entry, and the new App key requires external generation/entry. Every Acceptance item remains
  open.

## Notes

- GitHub rulesets apply cumulatively. A creation restriction admits only its bypass actors, while a
  separate update/deletion restriction with no bypass makes the created tag immutable even to the
  App that created it.
- GitHub deployment policies match branch and tag names separately. `release-control` is a pre-tag
  control boundary; `release` is a tag-only signing/publication boundary. Neither can substitute for
  the other.
- Repository configuration and workflow code are reversible. Credential disclosure, revocation,
  replacement and private-key entry are external maintainer actions; record names, timestamps and
  successful gated use, never credential bytes.
- C-354 defines the jobs attached to each environment. C-355 authenticates candidate bytes. C-516
  owns the PR-to-merged-main-SHA and post-tag ordering. All remain open.
