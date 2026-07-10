---
id: D-134
title: Sandbox abstraction, config, and threading (no OS backend yet)
pillar: Core
status: done
priority: 1
epic: process-sandboxing
design: docs/designs/process-sandboxing.md
note: "keystone — Sandbox/SpawnPolicy/Confinement types, [sandbox] config, FLUX_SANDBOX env channel, build_command seam; this story alone IS the Windows v1 behavior"
---

# Sandbox abstraction, config, and threading (no OS backend yet)

## Goal
Land the OS-sandbox abstraction end to end — config → CLI → env → `System` → `build_command`
seam — with only `Backend::Unsupported`, so every surface gains the settings plumbing, warnings,
and fail-closed `require` semantics before any real backend exists. On platforms without a
backend (Windows v1) this story's behavior is the shipped behavior.

## Acceptance
- [x] `crates/flux-system/src/sandbox.rs` exists with `SandboxSettings` (+ `from_env`), `SandboxMode`
      (Off/On/Require), `Backend` (Unsupported-only for now), `Sandbox` (`disabled`, `resolve`,
      `wrap_argv` identity passthrough, `configure` no-op, `ensure_available`, `preflight`),
      `SpawnPolicy::for_workspace`, and `Confinement`.
- [x] `build_command` takes an explicit `Confinement`; all five spawn modes pass it
      (`spawn_debug_pipe` = `Exempt`, rest = `Sandboxed`); failing-first test: with a `Require`
      sandbox and `Unsupported` backend, `run` returns a config error naming the reason
      (fail-closed backstop).
- [x] Sandboxed spawns inject `FLUX_SANDBOXED=1`; `FLUX_SANDBOXED` added to `SAFE_ENV`; test:
      `Sandbox::resolve` under the marker yields `Unsupported("already inside a flux sandbox")`.
- [x] `flux-config`: `SandboxConfig { enabled, require, network, writable }` on `Config` +
      security-directional merge (enabled/require OR, network strictest-wins, writable concat);
      parse + merge tests.
- [x] `flux-cli`: `--sandbox` / `--no-sandbox` conflicting globals; `apply_sandbox_env` resolves
      flag > env > config and exports `FLUX_SANDBOX` / `FLUX_SANDBOX_NET` / `FLUX_SANDBOX_WRITABLE`;
      startup preflight — `require`+unavailable errors out, otherwise one styled warning naming
      the reason.
- [x] `System::from_env(cwd)` (Workspace::from_env + Sandbox::resolve) replaces the production
      `System::new(Workspace::from_env(..))` sites; `with_sandbox` builder for
      custom-workspace sites; `System::new` stays env-free and infallible.
- [x] Gate green (both workspaces + codegate); CHANGELOG entry.

## Progress
- Implemented `crates/flux-system/src/sandbox.rs`: `SandboxMode`/`SandboxSettings::from_env`
  (`FLUX_SANDBOX`/`FLUX_SANDBOX_NET`/`FLUX_SANDBOX_WRITABLE`), `Backend` (all three variants
  declared; `resolve()` always yields `Unsupported` with a per-platform or nested-marker reason),
  `Sandbox` (`disabled`/`resolve`/`is_active`/`reason`/`describe`/`ensure_available`/`preflight`/
  `configure`/`wrap_argv`), `SpawnPolicy::for_workspace`, `Confinement`, and the
  `bubblewrap_argv`/`seatbelt_argv` stubs (`unimplemented!()`, unreachable in this story since no
  backend ever resolves active).
- `build_command` (flux-system/src/lib.rs) gains the `Confinement` param, wraps at the top
  (`ensure_available` first, then `wrap_argv` before `argv.split_first()`), and injects
  `FLUX_SANDBOXED=1` only when genuinely active (`sandbox::sandbox_marker`, unit-tested
  independent of the backend stubs). All five spawn modes updated; `spawn_debug_pipe` is
  `Confinement::Exempt` with the Chrome-sandbox rationale inlined. `FLUX_SANDBOXED` added to
  `SAFE_ENV` so the marker survives descendants' env-clear.
- `System` gained `sandbox: Sandbox`, `System::from_env(cwd)`, and `with_sandbox`. Migrated every
  production `System::new(Workspace::from_env(..))` site in `flux-cli/src/main.rs` (5 sites) to
  `System::from_env`; the 3 `workspace_with_flow_roots` custom-workspace sites get
  `.with_sandbox(resolved_sandbox())`. `flux-eval/src/runner.rs`'s eval-task spawn keeps its
  isolated `Workspace::new(&workdir)` (switching to `Workspace::from_env` would leak the host's
  `FLUX_ADD_DIRS`/`FLUX_ALLOW_ALL` into the isolated eval workspace) and instead gets
  `.with_sandbox(...)` — a deliberate deviation from the plan's literal "flux-eval → System::from_env"
  wording; see the Notes section below.
- `flux-config`: `SandboxConfig` added to `Config` with a security-directional merge
  (`merge_sandbox`) + accessor methods (`sandbox_enabled`/`sandbox_require`/`sandbox_network`/
  `sandbox_writable`), following the `WorkspaceConfig` pattern.
- `flux-cli`: global `--sandbox`/`--no-sandbox` (`conflicts_with`), `apply_sandbox_env(&cli, &cfg)`
  called right after `apply_workspace_access_env` in `main()`, resolving flag > pre-set env >
  config, exporting `FLUX_SANDBOX`/`FLUX_SANDBOX_NET`/`FLUX_SANDBOX_WRITABLE`, and running the
  startup preflight (hard error under `require`+unavailable via `?`; one styled warning otherwise).
- Tests added: `crates/flux-system/src/sandbox.rs` (12 unit tests: `from_env` truthiness/parsing,
  nested-marker resolve, `ensure_available` fail-closed/soft, `wrap_argv` identity,
  `sandbox_marker` decision, `SpawnPolicy::for_workspace` contents); `crates/flux-system/src/lib.rs`
  (`require_sandbox_with_unsupported_backend_fails_closed_on_run`,
  `flux_sandboxed_marker_survives_env_clear_like_other_safe_env_entries`); `crates/flux-config/src/lib.rs`
  (`sandbox_config_parses_and_merges_security_directional`, `sandbox_network_merge_is_strictest_wins`);
  `crates/flux-cli/src/main.rs` (`apply_sandbox_env_resolves_flag_over_env_over_config_and_fails_closed_under_require`,
  plus `--sandbox`/`--no-sandbox` cases added to the existing parse-error/valid-parse tests).
- Gate: `cargo build --workspace`, `cargo test -p codewandler-flux-system -p flux-config -p flux-cli
  -p flux-codegate` (+ `flux-eval` spot-checked), `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo fmt --all` (`--check` clean) — all green. `cargo test --workspace` also run as
  an extra check: 0 failures.

## Notes
- Design: [process-sandboxing](../designs/process-sandboxing.md). Invariants 1/4/5 land here.
- The seam wraps at the top of `build_command` so `current_dir`/`kill_on_drop`/`process_group`/
  `apply_safe_env` apply to the (future) wrapper unchanged.
- **Deviation from the plan doc**: `bubblewrap_argv`/`seatbelt_argv` are infallible
  (`-> Vec<String>`, matching the epic instructions' exact signature) rather than
  `Result<Vec<String>>` as sketched in `docs/designs/process-sandboxing.md`/the impl plan.
  `Sandbox::wrap_argv` still returns `Result<Vec<String>>` (wrapping `Ok(..)` today) so the seam
  stays fallible at that layer for D-135/D-136 if they need it, without forcing the two argv
  builders themselves to be fallible — argv construction from a `SpawnPolicy` is pure data
  transformation with no IO. Both are `unimplemented!()` stubs, unreachable in this story because
  `Sandbox::resolve` never activates a real backend.
- **Deviation from the plan doc, refined after review**: `flux-eval/src/runner.rs` keeps
  `Workspace::new(&workdir)` rather than `System::from_env`, because inheriting the caller's
  `FLUX_ADD_DIRS`/`FLUX_ALLOW_ALL` would leak host filesystem access into an isolated eval. The
  attached sandbox still governs grading and descendant policy, but the harness-selected child
  `flux` *host* now launches through the explicit trusted-host exemption: putting that host itself
  in `network = false` would block its Anthropic/OpenAI/Ollama request. The harness forwards
  `FLUX_SANDBOX`/`FLUX_SANDBOX_NET`/`FLUX_SANDBOX_WRITABLE` and backend overrides into the child,
  after benchmark-controlled env, so its own shell/plugin descendants are confined at their spawn
  choke point. It deliberately does not forward `FLUX_SANDBOXED`, because the host was not wrapped.
- For D-135/D-136: `Sandbox::wrap_argv` delegates to `bubblewrap_argv(bwrap: &Path, argv: &[String],
  policy: &SpawnPolicy) -> Vec<String>` / `seatbelt_argv(sandbox_exec: &Path, argv: &[String],
  policy: &SpawnPolicy) -> Vec<String>` in `crates/flux-system/src/sandbox.rs` — replace the
  `unimplemented!()` bodies. `Sandbox::resolve` needs its `platform_unsupported_reason()`-driven
  fallback replaced with real discovery (`FLUX_BWRAP_BIN`/PATH probe on Linux,
  `/usr/bin/sandbox-exec` existence on macOS) gated by `cfg!(target_os = ..)`, storing the
  **absolute** path in `Backend::Bubblewrap`/`Backend::Seatbelt`. `SpawnPolicy::for_workspace`
  already derives `writable`/`network`/`cwd`; `Sandbox::preflight` currently just calls
  `ensure_available()` and should become the real cached version probe.
