---
id: C-72
title: Publish flux-config in the SDK closure
pillar: Core
status: in-progress
note: "release blocker found by the v0.25.0 crates.io run: runtime's config dependency must be registry-resolvable"
---

# Publish flux-config in the SDK closure

## Goal

Make every dependency of the published runtime crate resolvable from crates.io while preserving the
existing `flux_config` Rust import path.

## Acceptance

- [x] `flux-config` uses the repository's `codewandler-flux-*` package namespace and retains the
  `flux_config` library name.
- [x] The root publish pin, ordered publish script, workflow documentation, and publishing runbook
  include config before runtime.
- [x] Both workspaces remain locked and the config package passes `cargo publish --dry-run`.
- [ ] The idempotent crates.io workflow resumes and publishes the remaining v0.25.0 closure.

## Progress

- The first v0.25.0 publish run stopped at `codewandler-flux-runtime`: its new `flux-config`
  dependency was path-only and therefore invalid in a published manifest.
- Added a codegate regression test that rejects path-only production dependencies in the published
  closure and requires the ordered publish script to list every vanity-prefixed workspace package.
- Kept that guard hermetic with `cargo metadata --no-deps`, so a root-only CI cache need not preload
  unrelated nested-plugin registry dependencies.
- The full root gate and the isolated config publish dry-run pass.

## Notes

- The first sixteen crates through `codewandler-flux-events` were already published successfully;
  the publish script deliberately skips them on resume.
