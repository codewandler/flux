---
id: C-310
title: "Catalog refresh — re-project a plugin's ops without restarting flux"
pillar: Core
status: done
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

- [x] **Failing-first test**: a fixture plugin whose `manifest` response changes between calls has
      its new op visible in the registry after a refresh, and its removed op gone. It fails today
      because nothing re-fetches.
      → `refresh_reprojects_a_changed_catalog_into_the_registry`
      (`crates/flux-plugin/tests/catalog_refresh.rs`), driving the new `drift_plugin` fixture
      (`crates/flux-plugin/src/bin/drift_plugin.rs`), whose manifest is read from a mode file
      passed as `argv[1]`.
- [x] Refresh is reachable from the CLI (`flux plugin refresh <name>`, or the shape that fits
      `plugin_cmd`'s existing subcommands — match what is there rather than inventing a verb).
      → `PluginAction::Refresh` (`crates/flux-cli/src/args.rs`) →
      `refresh_plugin_catalog` (`crates/flux-cli/src/plugin_cmd.rs`); reachability proven by
      `plugin_refresh_is_a_reachable_subcommand` (`crates/flux-cli/tests/plugin_refresh.rs`),
      the report wording by `plugin_cmd::tests::refresh_report_*`.
- [x] **A refresh re-runs every load-time check, and a manifest that now fails one is refused
      without disturbing the ops already registered.** A plugin does not get to widen its own
      authority by answering `manifest` differently the second time — re-validate
      `validate_manifest_operations`, the capability projection, and the authority contract exactly
      as the initial load does. Name the test that proves a refresh cannot escalate.
      → **`a_refresh_cannot_widen_the_granted_capabilities`** is the anti-escalation test
      (`catalog_refresh.rs`), with `every_capability_family_is_checked_for_widening` and
      `dropping_an_fs_scopes_secret_flag_is_a_widening` (`host.rs`) covering all **ten** families
      (`process`, `secrets`, `http`, `http_hosts`, `private_hosts`, `conn`, `blob`, `discover`,
      `credential`, `fs`) — `capability_widenings` destructures `PluginCapabilities` exhaustively,
      so an eleventh field reds the build rather than landing unchecked.
      **`a_surrendered_capability_declaration_cannot_strip_an_ops_authority`** is the other
      direction, and the more dangerous one: a refreshed manifest that *gives up* capabilities must
      not thereby strip its ops' `access` and authority requirements while the pinned host caps
      still grant them. `a_refresh_cannot_move_the_other_pinned_authority_fields` covers the same
      for `endpoints`/`auth`/`config`.
      `a_refresh_cannot_weaken_a_retained_ops_gating_scope` +
      `a_retained_op_may_not_weaken_its_gating_scope` cover the same-name re-scope;
      `a_refresh_re_runs_manifest_validation` covers `validate_manifest_operations`.
- [x] Removed ops are actually withdrawn from the catalog, not merely shadowed — an op the plugin no
      longer advertises must stop being callable.
      → `a_withdrawn_op_is_removed_while_an_in_flight_call_completes_under_its_old_spec`
      asserts `ToolRegistry::get` is `None` and the name is absent from `names()`. Decided and
      tested for the in-flight case: the running call keeps its own `Arc<dyn Tool>`, completes under
      the spec it was authorized with, and withdrawal governs only the next dispatch.
- [x] A refresh that fails (dead subprocess, protocol error, oversized frame) leaves the previously
      registered ops intact and reports the failure; it never half-applies a catalog.
      → `a_refresh_against_a_dead_subprocess_leaves_the_catalog_intact` and
      `a_refresh_with_an_oversized_manifest_frame_leaves_the_catalog_intact`. Both, plus the
      protocol-decode case, surface as an `Err` out of `PluginHost::manifest` before anything is
      mutated; `prepare_refresh` takes `&self` so no refusal can mutate, and
      `CatalogRefresh::apply` is clone-then-swap.
      `a_refused_registry_write_keeps_the_plugin_and_the_registry_in_step` covers the remaining
      ordering hazard: `refresh_into` writes the registry *before* committing the plugin, so a
      rejected `apply` cannot leave the plugin believing it published ops the registry never took
      (which would strand those names — the next refresh would diff against the newer manifest and
      never withdraw them).
- [x] Op coherence warnings (`op_coherence_warnings`, C-191) are emitted for the refreshed manifest
      the same way they are at load.
      → `a_refresh_reports_coherence_warnings_without_refusing_the_catalog` — warned, not fatal,
      exactly as at load; surfaced by the CLI via `refresh_report_surfaces_coherence_warnings`.
- [x] Full gate green in both workspaces.

## Progress
- Filed 2026-07-31 from the approved connectors-seam plan.
- 2026-07-31 — implemented. `LoadedPlugin::refresh()` + `CatalogRefresh::apply()` in the new
  `crates/flux-plugin/src/host/refresh.rs`; `flux plugin refresh <name>` on the CLI; operator docs
  in `website/docs/plugins/using-plugins.md`.
- The design decision worth carrying forward: **a refresh changes the operation set, never the
  grant.** Both halves of the grant are pinned to the load-time manifest — the enforced
  capabilities (`self.caps`, `make_caps` is never re-run) *and* the declared ones
  (`pin_granted_authority`, covering `capabilities`/`auth`/`endpoints`/`config`, exactly the fields
  `SystemHostCaps::with_manifest` reads). Capability containment is checked *literally* (a refreshed
  entry must appear verbatim in the granted list), deliberately stricter than the runtime grant
  matchers — a genuine narrowing such as `"kubectl get"` under a granted `"kubectl"` is also
  refused. A permissive error here is a privilege escalation, a strict one is a refusal the operator
  resolves with a restart, and "must already be in the list" cannot drift as grant grammars gain
  wildcards.
- **Rework round 1 fixed a blocking hole in the first cut, and it is the reason the declaration is
  pinned rather than merely checked.** The first cut pinned only enforcement and projected the specs
  from the *refreshed* declaration. A manifest that **surrendered** capabilities therefore produced
  ops with no `access` and zero `AuthorityRequirement`s — sailing past the authorization floor —
  while the pinned caps still handed them the secret, the host and the program. Overstating
  authority teaches an operator to over-grant; understating it removes the requirement to grant at
  all, so this direction was the worse one. Computing both halves from a single value makes the
  disagreement unrepresentable instead of checked.
- `CatalogRefresh::apply` withdraws only names its own `source` registered. `ToolRegistry::remove`
  is name-keyed and source-blind, so an unguarded withdrawal let a refresh silently evict another
  pack's identically named op — a privilege swap by collision. Found by the divergence test below.
- Not covered, deliberately: **the live in-session registry is still frozen.** `Executor` owns its
  `ToolRegistry` by value behind `Arc<Executor>` with no `registry_mut`, and
  `crates/flux-cli/src/execution.rs:1703-1708` cites A-95 prompt-cache stability as the reason the
  surfaced set must not churn mid-turn. This story delivers the mechanism and an operator surface
  for it; wiring it into a running REPL session needs the interior-mutability decision that A-95
  guards, and belongs to its own story.

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
