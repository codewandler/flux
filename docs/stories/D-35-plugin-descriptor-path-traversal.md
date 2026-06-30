---
id: D-35
title: Sanitize plugin descriptor names — block path traversal in `descriptor_path`
pillar: Core
status: done
priority:
epic: plugin-platform-hardening
note: `flux plugin uninstall ../../x` (or an absolute name) can delete files outside the plugins dir — centralize one guard in `descriptor_path`
---

# Sanitize plugin descriptor names — block path traversal in `descriptor_path`

## Goal
Close the path-traversal in `flux_plugin::descriptor_path` so a plugin name supplied to
`uninstall` / `install` / `pin` / `rollback` / `status` / `load` cannot escape the plugins directory
via `..`, a path separator, or an absolute component. `Path::join` treats those literally, and
`remove_descriptor` feeds the result straight into `std::fs::remove_file` — a destructive delete —
so `flux plugin uninstall ../../config` resolves to `<dir>/../../config.toml` and an absolute name
replaces the base entirely. Serves the Core safety invariant that one guarded path governs plugin
descriptor IO; the same `descriptor_path` backs `load_descriptor`, `add_descriptor`, `set_pinned`,
and `remove_descriptor`, so one guard covers all four.

## Acceptance
- [x] `descriptor_path` (or a guard it calls) rejects names containing a path separator (`/`), a `..`
      component, or a leading `/` (absolute), returning a clean `Err` **before any filesystem op** —
      covering `load_descriptor`, `add_descriptor`, `set_pinned`, and `remove_descriptor` from the
      single seam. Failing-first test `descriptor_path_rejects_traversal_names` (flux-plugin) verifying
      `remove_descriptor(dir, "../../x")`, `remove_descriptor(dir, "/etc/passwd")`,
      `add_descriptor(dir, "../x", …)`, and `load_descriptor(dir, "../x")` all error without touching
      the filesystem (a sentinel file left outside `dir` is still present afterwards).
- [x] `flux plugin uninstall ../../config` (and an absolute name) exits non-zero with a clear error
      and deletes nothing outside the plugins dir — CLI failing-first test, temp-dir-scoped
      (`run_plugin_in`) — `plugin_uninstall_rejects_traversal_names`.
- [x] Legitimate names (alphanumeric, `-`, `_`, `.`) still work end-to-end — the existing
      `plugin_uninstall_removes_descriptor` and
      `remove_descriptor_deletes_file_and_reports_missing_as_false` tests stay green unchanged.
- [x] Gate green: `cargo test -p flux-plugin -p flux-cli`, `clippy -D warnings`, `fmt`, and the
      `flux-codegate` layering lint. No new crate; no new cross-layer edge.

## Progress
- (done) Hardened `descriptor_path` to return `Result<PathBuf>` and reject a name that is empty,
  contains a path separator (`/` / `\`), or has a non-`Normal` path component (`..`, `.`, absolute,
  Windows prefix) — one guard (`invalid_plugin_name`) covering `add_descriptor` / `load_descriptor` /
  `set_pinned` (transitively, via `load`+`add`) / `remove_descriptor`. `set_pinned` needs no direct
  call: it delegates to `load_descriptor` + `add_descriptor`, both of which now validate.
- **Failing-first tests** — `descriptor_path_rejects_traversal_names` (flux-plugin): for each bad
  name (`../sentinel`, `../../…sentinel`, `/etc/passwd`, `a/b`, `..`, `.`, ``) it asserts all four
  entrypoints error, a sentinel file outside `dir` is untouched, nothing is written inside `dir`,
  and a legitimate name (`my-plugin_v2.0`) still round-trips. `plugin_uninstall_rejects_traversal_names`
  (flux-cli): `flux plugin uninstall ../../…sentinel` and an absolute name both exit non-zero and
  leave the outside sentinel intact. Each verified to fail before the guard (the unsanitized join
  would reach the sentinel) and pass after.
- **Gate** — `cargo build/test --workspace` green; `clippy -D warnings` clean; `fmt` clean;
  `flux-codegate` green. No new crate, no new cross-layer edge. (Pre-existing flux-config
  workspace-parallelism flake — `scoped_private_net_grants_parse_and_merge` / `loads_project_config` —
  passes in isolation and is unrelated to this change, which touches only flux-plugin L2 + flux-cli L6.)

## Notes
- Surfaced by an xhigh review of D-19 (commit `27b1c10`). D-19's acceptance as written (a clean
  error on a missing name, no panic) is met; the traversal is a distinct security hardening concern
  D-19 never enumerated. Filed as a separate story rather than reopening a committed / CHANGELOG'd /
  board-listed-as-done story — the repo's established pattern for hardening (cf. D-22, D-31, D-32
  under this same epic). See D-19's Notes for the backreference.
- Touch point: `crates/flux-plugin/src/lib.rs` — `descriptor_path` (~line 1746),
  `remove_descriptor` (~1812), `add_descriptor` (~1776), `load_descriptor` (~1789),
  `set_pinned` (~1802). CLI arm: `crates/flux-cli/src/main.rs` `PluginAction::Uninstall` (~4154)
  plus the `add` / `pin` / `rollback` / `status` callers (~4050, 4064, 4069, 4074, 4126).
- Prefer the guard **inside `descriptor_path`** so all four call sites are covered by one check
  rather than each repeating validation. Match the existing `Error::Other(format!(...))` rejection
  style. On Windows, also reject `\` and drive-absolute names; the existing tests are Unix-shaped,
  so gate any Windows-specific assertion behind `#[cfg(windows)]` if needed.
- Small, self-contained, no design doc.
