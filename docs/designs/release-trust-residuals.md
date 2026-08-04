# Release trust residuals — 2026-08-04

## Context

The validation pass over
[`docs/reviews/aggregate/2026-08-01-aggregate-complaint-triage.md`](../reviews/aggregate/2026-08-01-aggregate-complaint-triage.md)
found `REL-01`'s bootstrap allegation **closed** — `scripts/install-release-tooling.sh` verifies
cargo-dist against a committed SHA-256 and executes no downloaded script — and `REL-02` **closed
from `v0.38.0` forward**: `actions/attest` runs over every artifact, the attestations were verified
live for `v0.44.0`, and both README and getting-started document a verifier bound to the signer
workflow, tag ref and source digest.

The authority and promotion residuals were re-derived read-only on 2026-08-04 after Flux PR #29
merged. Canonical `origin/main` is
`9e3108b1b6856e30fa2e0baa2475d75d21fbc19f`:

- `branches/main/protection` returns **404** and `rulesets` returns `[]`. The only environment is
  `github-pages`; neither `release-control` nor `release` exists.
- The repository has **26 merged pull requests, zero recorded reviews and one administrator**.
- Actions defaults are `default_workflow_permissions=write` and
  `can_approve_pull_request_reviews=true`.
- Repository secret **names** are `RELEASE_TOKEN`, `MINISIGN_SECRET_KEY`, `ANTHROPIC_API_KEY` and
  `OPENROUTER_API_KEY`. The organization secret named `CARGO_REGISTRY_TOKEN` selects Flux (and four
  other repositories). No secret value was read or exposed.

GitHub secret values are write-only. Moving the three existing publication credentials cannot copy
or recover their values: a secure maintainer who already possesses each original value, or who can
revoke it and mint a replacement, must enter `RELEASE_TOKEN`, `MINISIGN_SECRET_KEY` and
`CARGO_REGISTRY_TOKEN` as `release` environment secrets. Creating the dedicated promotion App also
requires a maintainer to place its newly generated private key in `release-control`. These external
actions record names and metadata only; automation never reads, extracts or prints credential
values.

## Finding-to-story traceability

| Residual (revalidated 2026-08-04) | Story | v0.56.0 role |
| --- | --- | --- |
| `main` and release refs are unprotected; promotion has no dedicated identity; control/publication secrets have no separate environments | C-353 | publication blocker, including external configuration and secret-entry evidence |
| Release workflows mix model/build work, ref promotion, signing and publication authority; plugin publication remains branch-dispatched | C-354 | publication blocker |
| Candidate receipt v2 binds version, commit and run ID but not the seven artifact-archive identities and digests | C-355 | publication blocker |
| The cut bypasses a normal `main` PR; preview misses commit bodies/footers; staged/live shape and post-tag latest-state ordering are not one gate | C-516 | publication blocker |
| Attestation verification is documented but not yet the primary consumer install path | C-356 | visible residual, not a v0.56.0 publication blocker |
| One administrator and zero recorded reviews leave independent-review governance unproved | C-357 | visible residual, not a v0.56.0 publication blocker |

## Decisions

- **Tag creation and tag immutability are cumulative, not one rule.** Two active tag rulesets target
  exactly `refs/tags/v*` and `refs/tags/plugins-v*`. The creation ruleset has only the `creation`
  restriction and exactly one `Integration` bypass: the `flux-release-promoter` GitHub App. The
  immutability ruleset has `update` and `deletion` restrictions and an empty bypass list. Therefore
  the App can create a matching tag once, but neither it nor an administrator can move, force-update
  or delete that tag. Exact SemVer checks in the workflows narrow GitHub's glob patterns; malformed
  names never become releasable merely because they match `v*` or `plugins-v*`.
- **Protected `main` makes the release cut a pull request.** The release controller creates a cut
  branch, opens a normal pull request to `main`, waits for the required `ci` aggregate on that exact
  head, and merges through branch protection. Only the resulting merged canonical `main` SHA may
  become `release-candidates/<tag>` and then the annotated tag target. Direct or force push to
  `main`, including `HEAD:main`, is not a promotion or recovery path.
- **Control credentials and publication credentials live in different environments.**
  `release-control` admits only protected pre-tag controller refs and holds only
  `PROMOTION_APP_PRIVATE_KEY`. `release` admits only tag-triggered `v*` and `plugins-v*` jobs and
  holds `RELEASE_TOKEN`, `MINISIGN_SECRET_KEY` and `CARGO_REGISTRY_TOKEN`. Manual plugin dispatch is
  secret-free build/validation only. After the required `ci` succeeds on protected `main`, a separate
  automatic controller may ask the App to create the absent exact plugin tag at the still-current
  canonical-main SHA; only that tag event can sign, create a Release or publish a crate.
- **Promotion has one dedicated installation identity.** `flux-release-promoter` is installed only
  on `codewandler/flux`, requests Metadata read, Contents read/write, Actions read/write and Pull
  requests read/write, and has no user authorization, device flow or webhooks. The non-secret App ID
  is repository Actions configuration `PROMOTION_APP_ID`; its private key is the
  `release-control` environment secret `PROMOTION_APP_PRIVATE_KEY`. An installation token is minted
  only inside the narrow promotion job and is unavailable to model, build, signing and publication
  jobs.
- **`RELEASE_TOKEN` publishes GitHub Releases and nothing else.** It exists only in the tag-only
  `release` environment and only on the GitHub Release create/upload step. It is never a promotion
  identity and never moves `main`, a candidate ref or a tag.
- **Publication authority is scoped to the step that publishes.** Workflow and job environments do
  not carry publication secrets. Model and build jobs never receive a write token. Signing, GitHub
  Release publication and Cargo publication are distinct jobs with distinct authority.
- **Provenance certifies what arrived, not what was built.** Candidate receipt v3 binds the exact
  seven non-expired `artifacts-*` uploads by immutable ID, size and GitHub-reported lowercase
  SHA-256. The consumer hashes each raw ZIP before safe, namespaced extraction and before hosting,
  attestation or publication.
- **The promotion gate has one exact release shape.** A core release contains 28 named assets: ten
  application archives, their ten sidecars, four installers, `dist-manifest.json`, `sha256.sum`,
  `source.tar.gz`, and `source.tar.gz.sha256`. Compatibility is not permission to accept a weaker
  set for new releases.
- **Candidate cleanup is the last success step.** Promotion waits for newly created exact-tag/SHA
  binary and crates.io workflow runs, verifies the live Release, then runs the whole fleet/latest
  audit. An audit exit `2` is failure, never success or skip; any failure retains the candidate ref.
- **Consumer UX and governance remain honest residuals.** C-356 and C-357 stay visible and open, but
  neither changes whether a correctly gated v0.56.0 can be published.

## Dependency order

The contract order is C-353 (App, cumulative rulesets and separate environments) → C-354 (workflow
authority and tag-only publication) → C-355 (receipt-v3 candidate bytes) → C-516 (PR merge,
candidate, tag and public/latest closure). Implementations may be prepared together, but promotion
cannot be enabled until all four are implemented and the external C-353 configuration is verified.
Activation fails closed: an incomplete earlier boundary may stop releases, never authorize a direct
push, PAT promotion, branch publication or immutable-tag update.

## Release boundary

v0.56.0 is a core checkpoint release, not the Milestone-1 product release. Publication remains
blocked on implementation of C-353, C-354, C-355 and C-516 plus secure external App configuration,
publication-secret re-entry and value-free verification. The managed-local Exchange clean-machine
journey remains governed separately by C-509/C-510 and must pass its own acceptance before anyone
claims the Milestone-1 product release.

## Closure proof

Re-run the value-free platform queries in C-353 and prove both cumulative tag rulesets, both
environment policies, the exact App installation/permissions and secret-name placement. Inspect
parsed workflow structure under C-354; run the receipt-v3 adversarial fixture suite under C-355; and
run C-516's version, PR/merge-SHA, 28-asset, exact-run, live-release and promotion-order fixtures.
Then prove one release whose cut reaches `main` only through a green `ci` pull request, whose
candidate and immutable tag target that merged SHA, and whose publication completes through the
tag-only `release` environment. C-356 and C-357 keep this epic open after that publication proof.
