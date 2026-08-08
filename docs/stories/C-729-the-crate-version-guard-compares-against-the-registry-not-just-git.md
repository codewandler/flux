---
id: C-729
title: "The crate-version guard compares against the registry, not just git"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-release]
note: "codewandler-flux-secret 1.3.0 was published during a failed release run, then C-709 added a pub host field to that same version. check-crate-versions.sh compares a crate's version against git history, so the bump from 1.2.0 to 1.3.0 satisfied it and the later content change did not. The result was a green CI and a crates.io publish that stopped at flux-capabilities with 'no field host on type &EndpointRef', because the published 1.3.0 and the local 1.3.0 are different crates. The guard must ask crates.io whether this exact version is already published and whether its content still matches"
---

# The crate-version guard compares against the registry, not just git

## Goal


## Acceptance

- [x] The guard asks crates.io whether a crate's current version is already published, not only
      whether git moved it. A version already on the registry cannot be published again — `cargo
      publish` skips it — so a content change under it ships nothing.
- [x] The check needs no build, no credential and no `cargo` invocation, so it stays a first-step
      check: one HTTP GET against the sparse index.
- [x] A transport failure warns rather than fails, so an offline working tree stays buildable, and
      `FLUX_SKIP_REGISTRY_CHECK=1` opts out explicitly.
- [x] Proven against the real failure, not asserted: with `flux-secret` at 1.3.0 and `BASE=v0.58.0`
      it fails naming both `flux-secret` and `flux-evidence`; at 1.4.0 it passes. The existing
      `--self-test` still passes.

## Progress

- Implemented in `scripts/check-crate-versions.sh` as `registry_has_version`, wired into the
  existing per-crate loop that CI already runs early (`.github/workflows/ci.yml`,
  "independently-versioned crates moved their version").
- The defect it closes: `codewandler-flux-secret` 1.3.0 was published by the failed v0.59.0 run,
  and C-709 then added `pub host` to `EndpointRef` under that same version. The workspace build
  could never see it — every first-party crate resolves through its `path` dependency, so local
  content always wins and CI is green by construction, while `cargo publish` resolves from the
  registry. v0.59.1's closure therefore stopped at `flux-capabilities` with `no field `host` on
  type `&EndpointRef``.
- v0.59.2 shipped with the guard and flux-secret 1.4.0, and all 34 crates in the publish closure
  are on crates.io.
