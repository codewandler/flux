---
id: D-19
title: Complete the `flux plugin` lifecycle surface (uninstall + status)
pillar: Core
status: done
priority:
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
- [x] `flux plugin uninstall <name>` removes the descriptor at `~/.flux/plugins/<name>.toml` and reports what
      was removed; a missing name is a clean error (no panic, non-zero exit). Failing-first test
      `plugin_uninstall_removes_descriptor` (add then uninstall then `ls` no longer lists it).
- [x] `flux plugin status [<name>]` prints, per plugin: name, resolved binary path, **binary-exists +
      spawn/manifest-load check** (ok / missing / unloadable), version, pin state (from `pin`/`rollback`), and
      the declared surface from the manifest (ops count, requested capabilities, auth purposes, endpoints,
      datasources). With no argument it summarizes every installed plugin. Failing-first test
      `plugin_status_reports_manifest_and_liveness` (a registered-but-missing binary shows `missing`, not a
      crash).
- [x] `ls` remains the default subcommand and its output is unchanged (status is the richer, opt-in view).
- [x] Gate green: `cargo test -p flux-cli`, clippy `-D warnings`, fmt, `flux-codegate` layering lint. No new
      crate; no new cross-layer edge.

## Progress
- **`flux_plugin::remove_descriptor`** — removes `<dir>/<name>.toml`, returns whether one existed
  (`Ok(false)` for a missing name — a clean "nothing to uninstall", not an error); other IO failures
  propagate. Failing-first test `remove_descriptor_deletes_file_and_reports_missing_as_false`.
- **`flux plugin uninstall <name>`** — new `PluginAction::Uninstall` variant; prints `uninstalled plugin
  \`<name>\`` or bails `no such plugin \`<name>\` — nothing to uninstall` for a missing name.
- **`flux plugin status [<name>]`** — new `PluginAction::Status` variant. A `PluginStatusReport` carries
  the name, program/args, pin, `Liveness` (`Live`/`Missing`/`Unloadable(msg)`), and the loaded manifest.
  A missing binary is detected **without spawning** (`program_resolves` checks the path or `PATH`); a
  present binary is spawned and its manifest loaded via the same guarded `PluginHost::spawn` path `call`
  uses, so the declared surface (version, op count, auth purposes, endpoints, datasources, `discovers`,
  requested capabilities) is summarized. A bad-but-present binary (spawn/manifest failure) is
  `Unloadable`, never a crash.
- **Refactor for hermetic tests** — `run_plugin` split into a thin wrapper (resolves `plugins_dir()`)
  and `run_plugin_in(dir, action)` (the body); tests pass a temp dir, so they don't touch `HOME`.
- **Failing-first tests** — `plugin_uninstall_removes_descriptor` and
  `plugin_status_reports_manifest_and_liveness` (flux-cli); `remove_descriptor_deletes_file_and_reports_missing_as_false`
  (flux-plugin). Each verified to fail before the impl (missing symbol → compile error) and pass after.
- **Gate** — `cargo build/test --workspace` green (D-19 crates `flux-cli` 39 + `flux-plugin` stable across
  runs); `clippy -D warnings` clean (workspace + plugins); `fmt` clean; `flux-codegate` green; `plugins/`
  gate green. No new crate, no new cross-layer edge.
- **Note (out of scope):** `flux-config::tests::loads_project_config` is a pre-existing intermittent
  workspace-parallelism flake (latent temp-dir race in its test helper) — passes consistently in isolation;
  `flux-config` is untouched by D-19. Surfaced for visibility, not addressed here.

## Notes
- Touch points: `crates/flux-cli/src/main.rs` (the `plugin` subcommand group — alongside `ls`/`add`/`install`/
  `pin`/`rollback`/`call`/`skill`); the descriptor registry under `~/.flux/plugins/<name>.toml`; manifest
  introspection via `flux_plugin` (`PluginManifest`, `PluginCapabilities`).
- Reuse the manifest read the existing `skill` subcommand already does (it renders installed-plugin manifests),
  rather than adding a second introspection path.
- Small, self-contained, no design doc. Surfaced by the D-08/C-02 plugin pack work; see the plugin README's
  "Installing + invoking plugins" section, which currently has no documented uninstall path.
