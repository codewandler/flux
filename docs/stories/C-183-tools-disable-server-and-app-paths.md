---
id: C-183
title: Wire [tools] disable into the server and app-run executors
pillar: Core
status: done
epic:
design:
note: "C-162 shipped the blocklist on the primary interactive path only — flux-server's HTTP/A2A surface and `flux app run`'s multi-agent host (app_cmd.rs) build their own executors and never read cfg.tools.disable, so the same config silently means less there"
---

# Wire `[tools] disable` into the server and app-run executors

## Goal
Make `[tools] disable` mean the same thing on every execution path. C-162 resolves the blocklist
once at startup and installs it on the executor — but only in `build_agent_with` (the `flux run` /
REPL / TUI path). `flux serve` and `flux app run` assemble their own executors and never consult
`cfg.tools.disable`, so an operator who disabled `browser.*` project-wide still ships those ops to
every served agent. The surface promise should not depend on which front door was used.

## Acceptance
- [ ] `flux serve` (HTTP + A2A) executors resolve and install the same `[tools] disable` set as the
      interactive path — failing-first test asserting a disabled op is absent from a served agent's
      surfaced tool set and refused at dispatch.
- [ ] `flux app run` agents get the same treatment, including per-agent executors — failing-first
      test at the app-run assembly seam.
- [ ] Resolution stays snapshot-at-startup (the A-95 stability rule) and reuses C-162's
      `ToolRegistry::resolve_disabled` — no second matching implementation.
- [ ] Unmatched-pattern warnings surface somewhere visible on each path (server log line, app-run
      startup output), consistent with the interactive startup warning.
- [ ] Standard gate: build, test, clippy `-D warnings`, fmt — both workspaces — plus
      `flux-codegate`.

## Progress
- 2026-07-28: Implemented both wiring gaps and proved each with a failing-first test (temporarily
  reverted the new code locally, observed the test fail for the intended reason, restored it —
  none of these reverts were committed).
  - **`flux serve` (no-program `flux app run --serve <addr>`)**: traced the call graph and found
    this path already shares `build_agent`/`build_agent_with` with the interactive `flux run` path
    — C-162 resolves and installs `[tools] disable` there already, and `flux_server::serve` derives
    no second executor from the returned `FlowEngine`. No new code needed for this branch; added a
    comment at the call site (`crates/flux-cli/src/app_cmd.rs`) naming the C-162 tests that cover
    it (`resolve_disabled_*` in flux-runtime, the CLI-binary
    `tools_disable_unmatched_entry_warns_at_startup` in `mock_smoke.rs`) so a future reader doesn't
    assume this branch is unwired.
  - **`flux app run <program.flux>`** (the actual gap): `crates/flux-app/src/app.rs`'s
    `Engine::new` now takes the raw `disabled: Vec<String>` patterns, resolves them once against
    the fully-assembled registry (built-ins + cognition + orchestration + the surface's
    contributed catalog + `task`) via `ToolRegistry::resolve_disabled` (C-162's one
    implementation — no second matching logic), prints the same unmatched-pattern startup warning
    as the interactive path, and installs the resolved set on the shared `execution`
    `ExecutionEnvironment` template that both plain journeys (`build_executor`) and lazily-built
    per-agent engines (`agent_engine`/`build_agent_engine`) clone from — one resolution, every
    derived executor. `App::try_with_execution_environment` threads the new parameter through;
    `crates/flux-cli/src/app_cmd.rs`'s `run_app` now passes `cfg.tools.disable.clone()` at its one
    call site for the program-driven path.
  - Added `ExecutionEnvironment::into_executor` unit coverage in
    `crates/flux-runtime/src/lib.rs` (`execution_environment_carries_disabled_ops_into_executor`)
    proving the environment-level disabled set survives into the built `Executor` — the seam both
    of flux-app's derived-executor kinds rely on.
  - Added `crates/flux-app/src/app.rs`'s
    `app_run_agent_target_executor_installs_tools_disable` (per-agent executor: surfacing +
    dispatch refusal) and `crates/flux-cli/tests/mock_smoke.rs`'s
    `app_run_tools_disable_unmatched_entry_warns_at_startup` (real-binary `flux app run`, unmatched
    pattern warns at startup and exits 0) as the two failing-first tests for the previously-unwired
    seam.
  - Resolution is snapshot-at-startup only (resolved once in `Engine::new` before any journey or
    agent-target engine is constructed; the A-95 stability rule) and reuses C-162's
    `ToolRegistry::resolve_disabled` exclusively — no second matcher was written.
  - Gate (scoped to the touched crates — `flux-app`, `flux-cli`, `flux-runtime`): `cargo test -p
    flux-app -p flux-cli -p codewandler-flux-runtime` all green (see report), `cargo clippy -p
    flux-app -p flux-cli -p codewandler-flux-runtime --all-targets -- -D warnings` clean, `cargo
    fmt -p flux-app -p flux-cli -p codewandler-flux-runtime -- --check` clean. Did not run the
    full-workspace gate or `flux-codegate` (other sessions have uncommitted, unrelated changes
    live across `flux-tui`, `flux-config`, `flux-core`, CI config, and several other story docs in
    this same tree; a workspace-wide gate run would mix in their in-flight work). Left for
    whichever pass closes this story out.
- Checkboxes and `status` intentionally left as-is this round per explicit instruction from the
  orchestrating session — this Progress note records completed implementation + scoped gate
  evidence, not final story closure.

## Notes
- C-162 record: [C-162-tool-disable-list.md](C-162-tool-disable-list.md) — the resolve/install
  pattern to copy lives in `crates/flux-cli/src/execution.rs`; the refusal gate is already global
  (`Executor::gate`), so only the *resolution + install* step is missing on these paths.
- The dispatch-refusal half is executor-level and therefore already active anywhere an executor is
  built with `with_disabled_ops` — the gap is that these paths never call it, so neither surfacing
  nor refusal applies there today.
