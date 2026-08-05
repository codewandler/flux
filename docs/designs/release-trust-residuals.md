# Release trust residuals — 2026-08-04

> **Authority supersession (C-559, 2026-08-05).** The original C-353 proposal for a dedicated
> `flux-release-promoter` App, release environments, tag rulesets and branch protection is not the
> v0.56.0 release design. The repository has none of those settings, and the user directed that they
> not be provisioned. This document records the executable replacement below; the original proposal
> remains in C-353 as historical, unchecked hardening work.

## Context

The validation pass over
[`docs/reviews/aggregate/2026-08-01-aggregate-complaint-triage.md`](../reviews/aggregate/2026-08-01-aggregate-complaint-triage.md)
found the download-bootstrap and artifact-attestation allegations already closed, but release
authority and promotion ordering still needed work. Read-only GitHub evidence on 2026-08-05 showed:

- no `main` protection, tag ruleset, `release-control` environment, `release` environment,
  `flux-release-promoter` installation or `PROMOTION_APP_ID` variable;
- repository secret names including `RELEASE_TOKEN`, `MINISIGN_SECRET_KEY` and provider keys, plus
  the organization `CARGO_REGISTRY_TOKEN`; and
- an explicit user decision to use those existing Actions secrets rather than add unavailable App
  or Environment configuration.

Secret values remain write-only and were neither read nor copied. C-559 changes workflow placement,
not credential values or ownership.

## Active authority design

- **The deterministic cut and all build work are credential-free for release mutation.** The
  release-flow `cut` job receives no model provider or repository-write credential. Candidate
  planning, builds, receipts, byte verification and assembly receive no release secret.
- **`RELEASE_TOKEN` is the host mutation identity.** Only the core promotion step, plugin tag-control
  step and two GitHub Release create/upload steps name it. The core helper preflights the PAT before
  mutation, then uses it for the exact cut ref, the constructed fast-forward two-parent main merge,
  merged-main candidate ref, annotated-tag push and exact cleanup. The plugin helper uses it only to
  push the one absent exact `plugins-vX.Y.Z` annotated tag at current canonical `main`.
- **The ambient `GITHUB_TOKEN` cannot move repository state.** In core promotion it has
  `contents: read` and `actions: write`; it dispatches the complete `ci.yml` on the exact staged cut,
  then dispatches the exact candidate workflow and observes those Actions runs. A PAT-authenticated
  git push creates each tag so the tag-triggered workflows run. The only main update is an ordinary
  fast-forward of a locally constructed commit whose first parent is the just-read live main SHA,
  whose second parent is the exact green cut, and whose tree is the isolated-index three-way result.
  A concurrent move is rejected; arbitrary/force main pushes, force tags, tag updates and tag
  recreation do not exist.
- **Every secret remains step-scoped.** `MINISIGN_SECRET_KEY` appears only on plugin index signing;
  `CARGO_REGISTRY_TOKEN` only on the applicable Cargo publish step; provider keys only on their
  model/smoke consumers; and `RELEASE_TOKEN` only on the four host-owned consumers above. No
  workflow or job environment holds a long-lived credential, and no GitHub Environment is used.
- **Publication authorities do not compose.** Attestation, GitHub Release publication, plugin
  signing and Cargo publication remain separate jobs. Manual plugin dispatch is secret-free
  build/validation only. Core candidate preparation remains exact-ref/exact-SHA and secret-free.
- **Candidate receipt v3 authenticates the handoff.** It binds the exact seven non-expired
  `artifacts-*` uploads by immutable ID, size and GitHub-reported SHA-256. Raw ZIP bytes are checked
  before safe namespaced extraction, hosting, attestation or publication.
- **The promotion gate has one exact release shape.** A core release contains the 28 named assets,
  exact checksums and attestations defined by C-516. Promotion waits for newly created exact-tag/SHA
  binary and crates.io runs, verifies the live Release, runs the fleet/latest audit, and performs
  candidate cleanup last.
- **Strict release-branch protection is compatible, not bypassed.** An up-to-date frozen source may
  contain one wrapper merge. Promotion unwraps it only when one parent is the exact release-trigger
  base, the other is an ancestor of live canonical `main`, and the wrapper/outer-trigger trees are
  byte-identical to that main parent. No administrator merge path exists.

## Story traceability

| Story | Disposition for v0.56.0 |
| --- | --- |
| C-353 | Superseded by C-559. Its App, environments, rulesets, branch protection and external secret migration are not required or claimed. |
| C-354 | Done. Its job/step isolation and tag-only publication remain; its App/Environment placement clauses are historical and superseded. |
| C-355 | Done. Candidate receipt v3 and raw-byte verification are unchanged. |
| C-516 | Done with C-559's settings-free exact-cut CI and fast-forward merged-main SHA; exact runs, 28 assets and cleanup-last ordering are unchanged. |
| C-559 | Implements the active no-App/no-Environment authority contract and release-policy fixtures. |
| C-356 / C-357 | Visible consumer-verification and governance residuals; neither blocks v0.56.0 publication. |

## Failure behavior

Missing, invalid or read-only `RELEASE_TOKEN` fails before the first remote promotion mutation. A
failed candidate or post-tag verifier preserves the exact candidate and prints the same-SHA resume
command. Before tag creation a retained candidate may resume the one absent tag creation; after the
tag exists it is reused and never moved, deleted or recreated. GitHub Release and Cargo publication
remain idempotent on rerun.

## Closure proof

`scripts/check-release-authority.sh` parses all four release workflows and rejects workflow/job
secret scope, release authority beside model/build work, `GITHUB_TOKEN` tag creation, a missing PAT
tag trigger, combined signing/GitHub/Cargo authority, or any restored App/Environment dependency.
`scripts/test-promote-release-flow.sh` pins PAT preflight, exact cut-CI/main-merge/candidate/tag
ordering, ambient token limits, exact tag-run waits and cleanup-last behavior. Running
`scripts/plugin-tag-control.sh --self-test` proves only a green current-main `ci` result plus usable PAT can push the one absent
plugin tag. C-355 and C-516 retain their receipt, asset, live-release and fleet/latest suites.

v0.56.0 is a core checkpoint release, not the Milestone-1 product release. C-509/C-510 still own the
separate managed-local Exchange clean-machine journey.
