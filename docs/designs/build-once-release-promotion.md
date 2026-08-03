# Build-once release promotion

## Problem

The version tag currently starts five platform builds. The local cut is quick, but users cannot
install the release until those builds finish. Re-running a failed publication also rebuilds outputs
that were already successfully produced.

The optimization must not weaken provenance. A tag may publish only bytes built by this repository's
release workflow for the tag's exact source commit and workspace version.

## Lifecycle

### 1. Cut the release commit

`scripts/cut-release.sh` continues to create the version commit and a local annotated tag. Human
invocations run `scripts/release-full-gate.sh` before that commit. The unattended release-branch
flow is the sole exception: it creates the transactionally exact cut with `--no-gate`, stages it on
`release-candidates/vX.Y.Z`, and delegates the one mandatory gate to that candidate.

### 2. Prepare the exact commit

Dispatch `release.yml` on `release-candidates/vX.Y.Z` with the manifest version as its `version`
input. GitHub freezes the run's `head_sha`; the workflow validates both the version-derived ref and
`[workspace.package].version`, then runs `scripts/release-full-gate.sh "$GITHUB_SHA"` before planning
or building the unchanged cargo-dist local/global artifacts. It does not create or modify a GitHub
Release.

After the full gate and every build succeed, the run uploads a small `release-candidate-receipt`
containing a schema marker, version, tag, full commit SHA, `mandatory-full-v1` gate marker, gated SHA,
and workflow run ID. GitHub Actions v4+ artifacts are immutable within a run. The receipt therefore
names both the exact gated source and the immutable run that owns the cargo-dist outputs.

### 3. Promote with the tag

Pushing `v<version>` starts the same workflow at the tag commit. It queries successful manual runs
of this workflow at that exact `github.sha`, downloads the newest candidate receipt, and verifies
all receipt fields. Only then does the host job download that run's `artifacts-*` set, assemble the
cargo-dist announcement, create or refresh the GitHub Release, and run
`scripts/verify-github-release.sh`.

The workflow summary links the candidate run, leaving an auditable source-build → promoted-tag trail.
The tag run still plans from its own checkout, so cargo-dist independently rejects a tag/version
mismatch.

## Failure behavior

- A malformed candidate input, workspace-version mismatch, checked-out SHA mismatch, or red full
  gate fails before any artifact build or receipt.
- Receipt tampering, a SHA/version/run mismatch, or a missing candidate artifact fails before host or
  release creation.
- If no successful candidate exists at the tag SHA, the workflow logs a prominent warning and uses
  the existing five-platform build path. This compatibility fallback keeps an accidentally pushed tag
  releasable while making the slower behavior explicit.
- GitHub Release creation remains idempotent: an existing release receives the verified assets with
  `--clobber`; a missing release uses the existing bounded retry.
- Candidate workflow artifacts use a finite retention window. After expiry, a tag uses the explicit
  rebuild fallback; it never searches by version alone or promotes another commit's output.

## Trust and provenance

Candidate lookup is repository-local, restricted to successful `workflow_dispatch` runs of
`release.yml`, and filtered by the tag's full SHA. The downloaded receipt repeats and verifies the
version, tag, SHA, mandatory-gate identity, gated SHA, and run ID. Both preparation and promotion check out the workflow-owned source
revision with persisted Git credentials disabled.

No third-party upload path or mutable release cache is introduced. Cargo-dist still produces the
archives, installers, and checksum manifest, and the existing post-publication verifier still checks
the public asset shape.

## Operator flow

For `vX.Y.Z`, while its annotated tag remains local:

```sh
git push origin HEAD:refs/heads/release-candidates/vX.Y.Z
gh workflow run release.yml --ref release-candidates/vX.Y.Z -f version=X.Y.Z
sha=$(git rev-list -n1 'vX.Y.Z^{}')
run_id=$(gh run list --workflow release.yml --event workflow_dispatch --commit "$sha" \
  --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$run_id" --exit-status
git push origin vX.Y.Z
```

If `main` advances before dispatch, do not prepare the new head for the old tag. The candidate run
summary prints its exact SHA; compare it with `git rev-list -n1 vX.Y.Z`. A mismatch is safe—the tag
run will ignore that candidate—but it costs a fallback rebuild unless the exact tag commit is again
available as the dispatch ref.

## Non-goals

- Reusing ordinary CI binaries, whose profiles and target closure differ from cargo-dist.
- Moving crates.io publication into the binary promotion workflow.
- Skipping the pre-release live-provider/plugin smokes or the exact release commit's mandatory gate.
- Replacing cargo-dist's generated artifact or announcement formats.
