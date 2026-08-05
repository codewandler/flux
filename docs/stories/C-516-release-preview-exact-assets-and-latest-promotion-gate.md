---
id: C-516
title: Make release preview, exact asset inventory, and latest-state audit one promotion gate
pillar: Core
status: ready
priority: 4
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "v0.56.0 blocker — cut PR through ci, merged-main candidate/tag binding, full-message versioning, exact 28 assets/runs and fleet/latest audit before cleanup"
---

# Make release preview, exact asset inventory, and latest-state audit one promotion gate

## Goal

Make a promotion predict the irreversible version correctly, merge the cut through protected
`main`, publish one closed asset set from that merged SHA, and return success only after the exact
new tag's workflows, live Release and whole fleet/latest state are verified. Candidate deletion is
the final step, never early cleanup.

## Acceptance

- [ ] `release_plan` reads every commit in `v0.55.0..HEAD` with unambiguous record framing and the
      complete subject, body and footer. Breaking detection recognizes conventional `type!:` and
      `type(scope)!:` subjects plus footer tokens `BREAKING CHANGE:`, `BREAKING-CHANGE:` and
      `BREAKING:` only in their valid locations; it does not search incidental prose or concatenate
      records into a false marker.
- [ ] A non-empty `### Action needed` in `WHATS-NEW.md`'s `[Unreleased]` section is a breaking signal
      for the pre-1.0 minor bump: either a valid commit marker or this section selects `minor`, so the
      host cannot proceed with a patch that contradicts the user-facing migration notice. A fixture
      pinned to the current `v0.55.0` baseline proves the preview changes from the presently wrong
      `0.55.1` to `0.56.0`; no subject-only parser, manual override or weaker compatibility rule
      satisfies this contract.
- [ ] The host stages the deterministic cut on a fresh promotion branch, opens a normal pull request
      to protected `main`, and waits for the exact head's required `ci` aggregate before merging.
      It verifies the merge result is a new canonical `main` commit containing the exact cut diff,
      resolves that full SHA from GitHub after the merge, and only then creates
      `release-candidates/v0.56.0` at that SHA. The PR head/local cut/release-branch SHA cannot be a
      candidate or tag target. Direct `HEAD:main`, another direct/force push and an administrator
      bypass are absent from promotion and recovery fixtures.
- [ ] Staged and live verification require exactly these 28 distinct regular-file assets, with no
      omissions, extras or duplicate names:
  - ten application archives: each of `flux-cli` and `codewandler-flux-lsp` for
    `aarch64-apple-darwin.tar.xz`, `aarch64-unknown-linux-gnu.tar.xz`,
    `x86_64-apple-darwin.tar.xz`, `x86_64-unknown-linux-gnu.tar.xz` and
    `x86_64-pc-windows-msvc.zip`;
  - the ten matching `<archive>.sha256` sidecars;
  - `flux-cli-installer.sh`, `flux-cli-installer.ps1`,
    `codewandler-flux-lsp-installer.sh` and `codewandler-flux-lsp-installer.ps1`;
  - `dist-manifest.json`, `sha256.sum`, `source.tar.gz` and `source.tar.gz.sha256`.
- [ ] Every individual sidecar is exactly one newline-terminated
      `<64 lowercase hex> *<archive basename>` record for its sibling archive. `sha256.sum` contains
      exactly eleven unique, lexically ordered records in that same syntax: the ten application
      archives plus `source.tar.gz`; it contains no sidecar, installer, manifest or self entry. Both
      mechanisms are recomputed against downloaded bytes and must agree; path-bearing, uppercase,
      duplicate, orphaned, missing or extra records fail closed.
- [ ] Live verification resolves the annotated `v0.56.0` tag and its peeled target to the expected
      merged canonical-main SHA; the GitHub Release has that exact tag/target, is neither draft nor prerelease,
      and exposes the exact 28-name set with unique positive IDs/sizes. All 28 downloaded assets
      byte-match GitHub metadata and carry a valid `release.yml` GitHub attestation bound to
      `refs/tags/v0.56.0`, the expected source digest and no self-hosted runner. The verifier rejects
      historical optional assets or allowlists as substitutes for the new-release contract.
- [ ] After the candidate succeeds and before creating the tag, promotion snapshots the highest run
      database ID for `release.yml` and `crates-io.yml`. Only the `flux-release-promoter`
      installation token creates the annotated tag, once, at the verified candidate/merged-main SHA;
      `RELEASE_TOKEN`, `GITHUB_TOKEN`, a PAT or an administrator is not a tag-creation identity.
      After the tag exists, promotion waits for one newly created run of each workflow with
      `databaseId` above its snapshot, `event=push`, exact tag ref and exact head SHA; older runs,
      branch/manual runs, wrong tags/SHAs, duplicate ambiguous matches, skipped/cancelled jobs or a
      merely existing Release cannot satisfy the wait.
- [ ] Ordering is fixed and tested: both exact new runs finish successfully; then
      `scripts/verify-github-release.sh --repo codewandler/flux v0.56.0` verifies the live Release;
      then `scripts/check-release-tags.sh --repo codewandler/flux` verifies the entire tag/Release
      fleet and `/releases/latest`; only then may `release-candidates/v0.56.0` be deleted. Exit `2`
      from either verifier is failure, not success/skip. Every timeout, API failure or verification
      failure preserves the candidate ref and prints byte-for-byte resume commands for the same
      tag/SHA without rebuilding or deleting evidence. Before tag creation, resume may create the
      absent tag once; after creation, resume must verify and reuse the immutable tag and can never
      move, delete or recreate it. Only the narrow promotion job's `flux-release-promoter`
      installation token may perform the final candidate deletion; `RELEASE_TOKEN`, `GITHUB_TOKEN`, a
      PAT and an administrator cannot clean up or otherwise move that ref.
- [ ] Failing-first fixtures exhaust version parsing (body/footer/bang/action-needed mismatch and
      record-boundary traps); asset shape (each missing/extra/duplicate app, target and metadata
      class); sidecar/sum membership and digest corruption; tag peel, Release target/draft/prerelease,
      asset IDs/sizes/digests and attestation binding; PR check/merge/result-SHA mismatch;
      direct-main/PAT-tag paths; old/wrong/ambiguous workflow runs; verifier exit `1` and `2`; latest
      drift; and cleanup-before-audit. Success fixtures prove the exact `0.55.0 -> 0.56.0`, normal
      PR → merged-main SHA → candidate → one-time App tag, 28-asset and post-tag sequence. No weaker
      compatibility fixture passes.
- [ ] `scripts/release-candidate.sh`, `scripts/promote-release-flow.sh`,
      `scripts/verify-github-release.sh`, `scripts/check-release-tags.sh`, both tag workflows and the
      release policy test agree on these identities and ordering. The focused self-tests plus full
      release policy/documentation gate run before implementation is declared complete.

## Progress

- 2026-08-04 — filed `ready` from the release-trust audit at canonical
  `9e3108b1b6856e30fa2e0baa2475d75d21fbc19f` after PR #29. The current plan reads `%s` only,
  promotion directly advances `main`, the live v0.55.0 asset set demonstrates the exact 28-name
  shape, and promotion does not make the fleet/latest audit a precondition of candidate deletion.
  These are observed gaps; every Acceptance box is open.

## Notes

- v0.56.0 is a checkpoint core release, not the Milestone-1 product release. C-509/C-510 own the
  separate managed-local Exchange clean-machine acceptance.
- C-355 authenticates the seven Actions artifact ZIPs entering the host. This story authenticates
  the 28 files assembled from them and the final public/latest state; neither contract subsumes the
  other.
- C-353 owns the promotion App, cumulative tag rules and two environments. C-354 owns token
  placement and the plugin tag trigger. This story owns the core cut PR and exact merged-SHA
  sequence; it does not claim the current direct-push implementation is acceptable.
