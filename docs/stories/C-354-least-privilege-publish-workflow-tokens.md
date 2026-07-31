---
id: C-354
title: Scope publication tokens to the steps that publish
pillar: Core
status: backlog
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "release-plugins.yml grants contents:write WORKFLOW-wide including the 5-target matrix that compiles third-party vendor deps; crates-io.yml holds CARGO_REGISTRY_TOKEN at job level across cargo publish, which runs every dependency build.rs"
---

# Scope publication tokens to the steps that publish

## Goal

Stop a compromised third-party build script from finding a write-capable token in its own process
environment.

## Acceptance

- [ ] `.github/workflows/release-plugins.yml` declares `permissions: contents: read` at workflow
      level and grants `contents: write` only on the `assemble` job that needs it.
- [ ] `crates-io.yml` moves `CARGO_REGISTRY_TOKEN` from job-level `env:` to the single publish
      step's `env:`.
- [ ] The `assemble` job's concentration of `MINISIGN_SECRET_KEY` + `CARGO_REGISTRY_TOKEN` +
      `contents: write` is either split or recorded as an accepted concentration with reasoning.
- [ ] The existing release-policy check is extended to assert workflow-level `permissions` are
      read-only in every release workflow, so a future job inherits nothing by default.

## Progress

- 2026-08-01 — filed from the job-by-job trust graph built during validation.

## Notes

- `release.yml` is already correct on this axis: workflow default is `contents: read, actions: read`
  and only `host` widens. It is the model for the other two.
