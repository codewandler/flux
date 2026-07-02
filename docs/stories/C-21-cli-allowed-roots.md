---
id: C-21
title: CLI allowed roots — read outside the workspace via --add-dir (+ full-disable hatch)
pillar: Core
status: done
note: read-only extra roots (--add-dir / [workspace] add_dirs); --allow-all-paths lifts the sandbox entirely
---

# CLI allowed roots — read outside the workspace via --add-dir

## Goal
Let the flux CLI **read/glob/grep outside the single workspace directory** by configuring additional
**read-only allowed roots** — without loosening writes (which stay confined to the cwd) — plus an explicit,
warned **`--allow-all-paths`** escape hatch that lifts confinement entirely. The whole boundary is one
function (`Workspace::resolve`), so this is a focused core change + CLI/config plumbing.

## Acceptance
- [ ] `Workspace` gains **read-only extra roots** + an `unconfined` flag. A new `resolve_read` accepts a
      path under the primary root, any `@named` root, **or any extra read root**; the existing `resolve`
      (writes) stays confined to the primary root (+ named). `unconfined` lifts both. **Failing-first**
      (`flux-system` tests): an absolute path under an added read-root **reads** OK but **write** is still
      rejected; a path outside all roots is still rejected; `unconfined` allows anything.
- [ ] The `read`/`edit`(read part)/`glob`/`grep`/stat `System` methods route through `resolve_read`;
      `write`/`append` stay on `resolve`. `walk_files` also traverses the extra read roots (returning
      absolute paths for them) so `glob`/`grep` see outside-cwd files.
- [ ] CLI: a repeatable global **`--add-dir <DIR>`** (read-only roots) + **`--allow-all-paths`** (lifts the
      sandbox, prints a stderr warning). Config: a `[workspace]` section (`add_dirs = [...]`,
      `allow_all = true`) merged user-then-project like `[skills] dirs`. Precedence flag > env > config.
      `FLUX_ADD_DIRS` (`:`-list) + `FLUX_ALLOW_ALL` are the env channel the flag exports through so
      `app run` / subprocess paths inherit it.
- [ ] Default behaviour unchanged: with no `--add-dir`/config, the CLI stays confined to the cwd (all
      existing confinement tests green).

## Progress
- **Done (tested, gate-green).**
  - `flux-system`: `Workspace` gained `read_roots` + `unconfined`; `resolve` split into a shared `resolve_in`
    with `resolve` (write — root + `@named`) and `resolve_read` (read — + read roots); the read-side `System`
    methods route through `resolve_read`, writes stay on `resolve`; `walk_files` also traverses the read
    roots (absolute paths). `Workspace::from_env(cwd)` layers `FLUX_ADD_DIRS`/`FLUX_ALLOW_ALL`. Tests:
    read-root reads but not writes, outside-all still rejected, `walk` surfaces read-root files as absolute,
    `unconfined` lifts both (32 flux-system tests green).
  - `flux-config`: `[workspace] add_dirs`/`allow_all` section + `workspace_add_dirs()`/`workspace_allow_all()`
    accessors + user/project merge (project-first, `~/` expand), with a merge test.
  - `flux-cli`: global `--add-dir <DIR>` (repeatable) + `--allow-all-paths`; `apply_workspace_access_env`
    merges flags + config + pre-set env → exports `FLUX_ADD_DIRS`/`FLUX_ALLOW_ALL` (so `app run`/subprocess
    inherit) and prints a stderr warning when the sandbox is lifted. Production `Workspace::new` sites in
    `flux-cli` + `flux-app` swapped to `Workspace::from_env`; test sites stay confined.
  - Verified: flags appear in help (global + per-subcommand); `--allow-all-paths` prints the warning on a
    real run; clippy + codegate layering clean; existing confinement tests still green (default unchanged).
- **Residual:** an SDK builder surface (`ClientBuilder::add_read_root`) for library consumers, if wanted —
  the CLI/app ask is fully covered.

## Notes
- Choke-point: `crates/flux-system/src/lib.rs:67` (`resolve`) → `Error::Config` on escape; `Workspace`
  already holds `root` + `named: HashMap` (the multi-root precedent, wired for `@global_ops` at
  `flux-cli/src/main.rs:1400`). Read-tool path: every builtin file tool routes through `System` →
  `resolve` (`flux-tools/src/lib.rs` read/write/glob/grep → `flux-system` guarded ops).
- Patterns to mirror: `[skills] dirs` (`Vec<String>` + `~` expand + user/project concat merge,
  `flux-config/src/lib.rs`), the `--skill-dir` repeatable `PathBuf` flag, and the `FLUX_ENABLE_BASH`
  env-export from config (`main.rs:1362`). Confinement tests to extend live in the `flux-system` test
  module (`rejects_absolute_outside`, `rejects_parent_escape`, …).
- Read-only roots (writes confined) is deliberate — matches "read outside a dir" and keeps the safe
  default; `--allow-all-paths` is the opt-in full escape hatch.
