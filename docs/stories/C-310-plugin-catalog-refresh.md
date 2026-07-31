---
id: C-310
title: "Catalog refresh — re-project a plugin's ops without restarting flux"
pillar: Core
status: in-progress
priority: 7
epic: connector-platform
areas: [flux-plugin, flux-cli]
note: "the behavior the connectors seam is built on: an op set that changes when the operator authenticates a provider. PluginProcess::manifest() is already re-callable on an open process — nothing calls it twice and nothing re-projects the registry"
---

# Catalog refresh — re-project a plugin's ops without restarting flux

## Goal

Let a running flux re-fetch a loaded plugin's manifest and re-project its operations into the live
tool registry, so an op set that depends on remote state becomes usable without a restart.

This is the load-bearing behavior for the **connectors seam** (see Notes): the operator says
"there is a connectors deployment at `localhost:8000`", authenticates the Zendesk provider *inside
that deployment*, and `zendesk-ticket-create` must then become callable in the session that is
already open. A restart-to-see-new-ops loop makes the whole seam unusable interactively.

## Context — verified against this tree

- `PluginProcess::manifest()` (`crates/flux-plugin/src/host/loading.rs:187`) is a plain
  `self.request("manifest", Value::Null)` over the open NDJSON channel — **not** a file read. So a
  manifest that reflects live remote state is already expressible today; the plugin answers
  `manifest` by querying whatever it fronts.
- Nothing calls it a second time. The manifest is fetched once during load and the resulting
  `ToolSpec`s are projected once.
- `plugin_tool_spec` (same file) is a pure function of `(plugin, op, capabilities)`, so re-projection
  needs no new derivation logic — only a caller and a registry swap.
- ⚠ C-309 changed this function (`AccessKind::Process` is now unconditional). Rebase awareness: this
  story edits the same file.

## Acceptance

- [ ] **Failing-first test**: a fixture plugin whose `manifest` response changes between calls has
      its new op visible in the registry after a refresh, and its removed op gone. It fails today
      because nothing re-fetches.
- [ ] Refresh is reachable from the CLI (`flux plugin refresh <name>`, or the shape that fits
      `plugin_cmd`'s existing subcommands — match what is there rather than inventing a verb).
- [ ] **A refresh re-runs every load-time check, and a manifest that now fails one is refused
      without disturbing the ops already registered.** A plugin does not get to widen its own
      authority by answering `manifest` differently the second time — re-validate
      `validate_manifest_operations`, the capability projection, and the authority contract exactly
      as the initial load does. Name the test that proves a refresh cannot escalate.
- [ ] Removed ops are actually withdrawn from the catalog, not merely shadowed — an op the plugin no
      longer advertises must stop being callable.
- [ ] A refresh that fails (dead subprocess, protocol error, oversized frame) leaves the previously
      registered ops intact and reports the failure; it never half-applies a catalog.
- [ ] Op coherence warnings (`op_coherence_warnings`, C-191) are emitted for the refreshed manifest
      the same way they are at load.
- [ ] Full gate green in both workspaces.

## Progress
- Filed 2026-07-31 from the approved connectors-seam plan.

## Notes
- **The connectors seam.** The target experience is `flux auth login connectors` (which already works
  — `crates/flux-cli/src/auth_cmd.rs:112-121` falls through to `login_plugin` for any non-builtin
  name, running a real PKCE grant with a loopback listener), then a thin plugin answers `manifest` by
  querying the deployment's catalog and `operation.call` by posting to it. flux holds exactly one
  secret on that path — the deployment session bearer — and never a vendor credential.
- The host running that OAuth grant (`crates/flux-plugin/src/host.rs:371-420`) is **correct** here and
  is not a posture problem: the bearer it manages is the plugin's *own* credential, which in this case
  is the connectors session. The credential that must never enter flux is the **vendor's**.
- Sibling stories: [C-311](C-311-vendor-host-disclosure-at-approval.md),
  [C-312](C-312-connector-credential-boundary.md).
- The external half (a connectors API with sign-in, provider listing, activation and a catalog
  endpoint) does not exist yet in `../flux-connectors` — its `docs/designs/connectors-app.md` describes
  a loopback reference host that executes ops **in-process**. This story is independently testable
  against a fixture plugin and must not wait for it.
