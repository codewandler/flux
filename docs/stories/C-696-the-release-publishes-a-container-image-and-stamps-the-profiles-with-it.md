---
id: C-696
title: "The release publishes a container image and stamps the profiles with it"
pillar: "Core"
status: in-progress
epic: remote-agents
areas: [release]
design: docs/designs/operating-a-deployed-host.md
note: "release-blocking: the profiles pin newTag 0.58.0 and cut-release.sh does not restamp it, and newName is a bare local name no workflow publishes"
---

# The release publishes a container image and stamps the profiles with it

## Goal

Two release defects sit between the delivered deployment profiles and an operator who can use
them. First, `deploy/kubernetes/kustomization.yaml` and `deploy/agent/kustomization.yaml` pin
`newTag: 0.58.0` and `cut-release.sh` does not restamp it, so cutting a release leaves both
profiles advertising the previous version's image — the manifests and the binary disagree the
moment a release happens. Restamping is blocked today by two exact-string greps
(`scripts/test-embedded-docs-gates.sh:83`, `scripts/test-release-candidate.sh:334`) which must be
taught the same substitution rather than worked around. Second, `newName: flux-system` is a bare
local name and no workflow publishes an image anywhere, so both profiles are silently
build-it-yourself while C-480's own acceptance says released docs use shipped artifacts.

Publish the image from the release workflow and make the profiles reference what was published,
without weakening the release integrity rules: `check-release-integrity.sh` restricts
`attestations`/`id-token` permissions to the attesting job, and the release asset inventory is
closed and structurally enforced.

## Acceptance

- [ ] The release workflow builds the container image from the released binary artifact — never a
      fresh source build — and publishes it to the project registry tagged with the release
      version, with provenance attestation, and `check-release-integrity.sh` passes unchanged or
      is extended deliberately with the reason recorded.
- [ ] `cut-release.sh` restamps both kustomizations' image tag as part of the cut; the two
      exact-string gates learn the same substitution, and a test proves a cut leaves no manifest
      referencing the previous version.
- [ ] `newName` references the published registry path, and `deployment_artifacts` pins that the
      two profiles agree on both name and tag with each other and with the workspace version.
- [ ] The deployment docs state what is now true: pull the published image, or build locally with
      the documented script — with the verification command for the attestation.
- [ ] A dry run of the cut on a scratch branch demonstrates the restamp and leaves the working tree
      clean; no registry push happens outside a real release.


## Comments

- In progress: dispatched to an implementor in worktree flux-c696 off base 4af5e8cf. Release-blocking pair: cut-release.sh does not restamp the two kustomizations' image tag (blocked by two exact-string gates that must learn the same substitution), and newName is a bare local name no workflow publishes. Must not weaken check-release-integrity.sh's scoping of attestations/id-token to the attesting job — report BLOCKED instead.
