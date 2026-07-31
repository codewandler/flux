---
id: D-202
title: Zendesk tutorial, catalog integration, and release proof
pillar: Agent
status: done
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [docs, website, plugins]
note: "CLOSED (D-214) — the docs half was redone for the connector pack; the release-proof half is VOID (no binary to sign) and the live-credential run is D-214's open bullet, not tracked twice here"
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
- 2026-07-31 — **closed under D-214**, with its four bullets resolving three different ways rather
  than one:
  - The **tutorial** was redone for the connector pack, not merely unblocked. It had decayed into
    active misinformation: it instructed a reader to `flux auth set zendesk`, `flux plugin status
    zendesk` and `flux plugin call zendesk zendesk.test '{}'` against a binary that does not exist.
    It now documents how a host registers the pack, and names the two connector-side gaps that keep a
    live run impossible.
  - The **catalog/website/drift** bullet holds: `examples/README.md` (which still told a reader to
    `cargo build --release -p zendesk`), its index row, and the website examples page are corrected,
    and the drift tests that guard them are green.
  - ~~`scripts/smoke-plugins.sh` adds an env-gated `zendesk.test` leg~~ → **VOID.** There is no plugin
    to smoke, and a connector-pack smoke leg cannot live in this repository: `connector-pack` depends
    on `flux-spec`/`flux-runtime`, so flux cannot depend back on it (see D-214's Notes). Nor would a
    "skipped" leg be honest here — with no credential *address* declared, skipping would imply a
    credential is all that is missing.
  - ~~a separate plugin-pack release is recorded as owed~~ → **VOID for the reason this story
    recorded it:** the `flux-plugin-zendesk` binary that would have needed a signed pack was removed
    before it ever shipped, so *that* debt is discharged by deletion rather than by cutting anything.
    **A pack release is nonetheless owed for an unrelated reason**, and closing this story must not be
    read as saying otherwise: `check-host-kit-protocol-drift.sh` fails on the tree today because
    `codewandler-flux-plugin-protocol` 1.1.0 is live on crates.io while the published
    `codewandler-flux-host-kit` still requires `^1` (1.0.0). That is the protocol line's independent
    versioning (C-143), it predates this story, and it is tracked where the protocol line is tracked
    — not here.
  The one genuinely unfinished thing — a documented run against live credentials — is **D-214's open
  bullet** and is deliberately not tracked twice.
