---
id: D-19
title: Complete the `flux plugin` lifecycle surface (uninstall + status)
pillar: Core
status: backlog
epic: plugin-platform-hardening
note: add `uninstall` + a richer `status`/`info` (version, pin, liveness, declared surface); small, no design doc
---

# Complete the `flux plugin` lifecycle surface (uninstall + status)

## Goal
Round out the `flux plugin` subcommand group so plugin management is fully first-class from the CLI. Today
it has `ls / add / install / call / pin / rollback / skill` but **no `uninstall`** (removal is a manual
`rm ~/.flux/plugins/<name>.toml`) and **no `status`/`info`** (`ls` only prints name → resolved binary path,
with no version, pin state, liveness, or declared surface). Serves the Agent pillar's "every integration is
legible and managed through one envelope" by making the lifecycle complete and inspectable.

## Acceptance
- [ ] `flux plugin uninstall <name>` removes the descriptor at `~/.flux/plugins/<name>.toml` and reports what
      was removed; a missing name is a clean error (no panic, non-zero exit). Failing-first test
      `plugin_uninstall_removes_descriptor` (add then uninstall then `ls` no longer lists it).
- [ ] `flux plugin status [<name>]` prints, per plugin: name, resolved binary path, **binary-exists +
      spawn/manifest-load check** (ok / missing / unloadable), version, pin state (from `pin`/`rollback`), and
      the declared surface from the manifest (ops count, requested capabilities, auth purposes, endpoints,
      datasources). With no argument it summarizes every installed plugin. Failing-first test
      `plugin_status_reports_manifest_and_liveness` (a registered-but-missing binary shows `missing`, not a
      crash).
- [ ] `ls` remains the default subcommand and its output is unchanged (status is the richer, opt-in view).
- [ ] Gate green: `cargo test -p flux-cli`, clippy `-D warnings`, fmt, `flux-codegate` layering lint. No new
      crate; no new cross-layer edge.

## Progress
- (not started)

## Notes
- Touch points: `crates/flux-cli/src/main.rs` (the `plugin` subcommand group — alongside `ls`/`add`/`install`/
  `pin`/`rollback`/`call`/`skill`); the descriptor registry under `~/.flux/plugins/<name>.toml`; manifest
  introspection via `flux_plugin` (`PluginManifest`, `PluginCapabilities`).
- Reuse the manifest read the existing `skill` subcommand already does (it renders installed-plugin manifests),
  rather than adding a second introspection path.
- Small, self-contained, no design doc. Surfaced by the D-08/C-02 plugin pack work; see the plugin README's
  "Installing + invoking plugins" section, which currently has no documented uninstall path.
