---
id: D-202
title: Zendesk tutorial, catalog integration, and release proof
pillar: Agent
status: blocked
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [docs, website, plugins]
note: "WITHDRAWN before release — plugin docs/catalog/smoke entries removed with the plugin; redo for flux-connectors"
---

# Zendesk tutorial, catalog integration, and release proof

## Goal

Make the Zendesk workflow discoverable and reproducible for a user who has only a Flux installation,
a Zendesk URL/email, and one API token, and leave the release channel honest about what is not yet
published.

## Acceptance

- [x] Tutorial covers signed-pack and local install, non-secret endpoint/user configuration,
      `flux auth set zendesk`, every entrypoint, safe-write examples, model requirements, and the
      explicit exposure of internal notes to the configured model.
- [x] Example/plugin catalogs, website navigation, typed-migration inventory, and generated/customer
      changelog mirror remain complete under their drift tests.
- [x] `scripts/smoke-plugins.sh` adds an env-gated `zendesk.test` leg and skips honestly without
      credentials.
- [x] Root and nested plugin workspace build/test/clippy/fmt/codegate checks are green; a separate
      plugin-pack release is recorded as owed, not performed.

## Progress

- 2026-07-30 — tutorial, catalogs, sidebar, changelogs/mirror, typed inventory, and credential-gated
  smoke are complete. Nested workspace is fully green; root build/fmt/codegate and feature-focused
  tests are green. The root-wide gate remains red only in concurrent, pre-existing remediation work:
  two `flux-orchestrate` adaptive-loop tests fail and `flux-server::resource::record_provider_delta`
  is dead under clippy `-D warnings`. The signed plugin-pack release remains explicitly owed.
- 2026-07-30 — closed on integration. The concurrent remediation work landed, and the full gate is
  green in **both** workspaces: `cargo test --workspace`, `clippy --all-targets -D warnings`,
  `cargo fmt --check`, `cargo test -p flux-codegate`, and every `scripts/check-*.sh` policy gate.
  The signed plugin-pack release carrying the new `flux-plugin-zendesk` binary is still owed and is
  cut separately from the core release.
