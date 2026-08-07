---
id: C-717
title: "The release cut refuses unbumped independently versioned crates"
pillar: "Core"
status: backlog
areas: [flux-release]
note: "v0.58.0 publish evidence: flux-evidence 1.1.0 gained KIND_BUDGET_PROJECTION without a bump, the idempotent publisher skipped the already-published version, and flux-flow 0.58.0 can never verify; v0.58.1 with evidence 1.2.0 completes the line"
---

# The release cut refuses unbumped independently versioned crates

## Goal

An independently versioned crate whose content changed since its last published version must fail
the release cut until its own version moves. The v0.58.0 tag shipped `flux-evidence` source
containing `KIND_BUDGET_PROJECTION` under the already-published 1.1.0: the idempotent publish
closure correctly skipped the existing registry version, and `codewandler-flux-flow` 0.58.0 then
failed tarball verification with E0425 against the old registry content — an unpublishable release
that no local gate had refused. The same drift pattern is already queued again: unreleased
`flux-secret` gained `HostRef` while its manifest still reads 1.2.1.

## Acceptance

- [ ] Failing first: a fixture reproducing an independently versioned crate with content drift and
      no version bump makes the release-cut check fail naming the crate, its manifest version and
      the published version it collides with.
- [ ] The check compares each independently versioned workspace crate against its most recently
      published version (registry index or recorded release inventory) and refuses the cut when
      content changed under an already-published version; lockstep `0.x` workspace crates are out
      of scope, their version is the tag.
- [ ] A v0.58.1 patch cut bumps `flux-evidence` to 1.2.0, raises the workspace requirement, and
      the complete publish closure — including the crates the failed v0.58.0 runs never reached —
      verifies and publishes through the existing idempotent workflow.
- [ ] The unreleased `flux-secret` `HostRef` drift is caught by the new check on the next cut
      rather than by hand.
