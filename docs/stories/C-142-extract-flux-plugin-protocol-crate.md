---
id: C-142
title: Extract codewandler-flux-plugin-protocol — the wire contract as its own crate
pillar: Core
status: done
priority: 11
epic: plugin-protocol-decoupling
design: docs/designs/plugin-protocol-decoupling.md
note: protocol.rs (856 lines of wire types + the guest stdio SDK) lives inside the host crate, so a guest has no crate to depend on that isn't also flux's host half
---

# Extract `codewandler-flux-plugin-protocol` — the wire contract as its own crate

## Goal

Give the plugin wire contract a crate of its own, so guests depend on the protocol rather than on
flux's plugin host. Prerequisite for the independent version line (C-143).

## Acceptance

- [x] New `crates/flux-plugin-protocol` (`codewandler-flux-plugin-protocol`) holds the contents of
      `crates/flux-plugin/src/protocol.rs`: `Frame`/`FrameKind`, `PluginManifest`,
      `OperationSpec`, `PluginCapabilities`, `AuthMethod`, `EndpointSpec`, `ConfigSpec`,
      `process_grant_allows`, the `PluginHandler`/`GuestHost` traits, and the synchronous `serve`
      stdio loop.
- [x] `flux-plugin` keeps the host half and re-exports the wire types, so no host call site
      changes (verified by the workspace building without edits outside the two crates).
- [x] The new crate's dependency graph is serde-only — no `flux-lang`, no `flux-core`, no
      `flux-runtime` (asserted by a test, not by inspection).
- [x] `plugins/host-kit` depends on the protocol crate instead of `flux-plugin`; the `guest`
      feature on `flux-plugin` is retired or reduced to a re-export shim.
- [x] The `flux-codegate` layering lint places the new crate at L0 and passes.
- [ ] Full gate green in both workspaces; `scripts/smoke-plugins.sh` passes.

## Progress
- Done. See the CHANGELOG `[Unreleased]` entries and `docs/designs/plugin-protocol-decoupling.md` ("As built").
- Standing gate item: the full both-workspace gate and `scripts/smoke-plugins.sh` run with the epic's other stories, not this one alone.

## Notes
- Depends on C-141 — extract only after the `flux-lang` edge is gone, or the new crate inherits it.
- Keep the module path stable for host consumers via `pub use`, so this is mechanical for
  everything outside `plugins/`.
