# Release trust residuals — 2026-08-01

## Context

The validation pass over [`docs/reviews/aggregate-complaint-triage.md`](../reviews/aggregate-complaint-triage.md)
found `REL-01`'s bootstrap allegation **closed** — `scripts/install-release-tooling.sh` verifies
cargo-dist against a committed SHA-256 and executes no downloaded script — and `REL-02`
**closed from `v0.38.0` forward**: `actions/attest` runs over every artifact, the attestations were
verified live for `v0.44.0` against `/repos/codewandler/flux/attestations/`, and both README and
getting-started document `gh attestation verify` bound to signer-workflow, tag ref and source digest.

What survived is the **authority** half of REL-01, which no code change addressed and which the
platform now confirms rather than leaves unknown. `gh api` answered every query:

- `branches/main/protection` → **404, not protected**; `rulesets` → `[]`.
- `collaborators` → one admin; `gh pr list --state merged` → **empty**. Every commit lands by direct push.
- `environments` → only `github-pages`. `RELEASE_TOKEN`, `CARGO_REGISTRY_TOKEN` and
  `MINISIGN_SECRET_KEY` carry no reviewer, no branch policy, no wait timer.
- `ci.yml` has been red on `main` for six consecutive pushes and blocked nothing.

`ASSURE-04`'s "external-unknown" half is therefore no longer unknown; it is verified absent.

## Finding-to-story traceability

| Residual (validated 2026-08-01) | Story |
| --- | --- |
| `main` has no protection and no rulesets; release secrets have no environment gate | C-353 |
| `release-plugins.yml` grants `contents: write` workflow-wide including the vendor-dep build matrix; `crates-io.yml` holds the registry token at job level across `cargo publish`, which runs every dependency `build.rs` | C-354 |
| The candidate receipt binds version + commit + run-id but **no artifact digests**, so `host` promotes fetched bytes unverified and attests whatever arrived | C-355 |
| Installers are attested but the documented primary path runs `sh flux-installer.sh` without a verify step; no machine-readable statement of the first attested tag | C-356 |
| One author, one admin, zero merged PRs, no succession or incident-exercise evidence | C-357 |

## Decisions

- **A gate that blocks nothing is documentation.** Every assurance lane in this repo is advisory
  while `main` accepts unreviewed force-pushable direct pushes. Protection is the precondition that
  makes the rest of the assurance work load-bearing, so it ranks first.
- **Publication authority is scoped to the step that publishes.** A token live in a job that
  compiles third-party build scripts is a token that third-party build scripts can reach.
- **Provenance certifies what arrived, not what was built.** Attestation applied after an
  unverified artifact fetch proves the workflow's identity, not the bytes' integrity. The receipt
  must carry digests for the handoff to be authenticated.
- **Checksums from the producing workflow are integrity metadata, never a trust root.** This holds
  for the `.sha256` files, `sha256.sum`, and the checksums baked into the installer alike.
- **Bus factor is a risk to own, not a story to fake.** It is recorded with an owner and a review
  date; no code change can close it.

## Closure proof

Re-run the platform queries that produced this design (`branches/main/protection`, `rulesets`,
`environments`, `collaborators`, merged-PR count) and require each to return the intended state.
Verify one full release under the new receipt binding.
