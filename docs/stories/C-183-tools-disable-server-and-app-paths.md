---
id: C-183
title: Wire [tools] disable into the server and app-run executors
pillar: Core
status: backlog
priority:
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
- (not started — filed 2026-07-28 as the scope note from C-162's implementation)

## Notes
- C-162 record: [C-162-tool-disable-list.md](C-162-tool-disable-list.md) — the resolve/install
  pattern to copy lives in `crates/flux-cli/src/execution.rs`; the refusal gate is already global
  (`Executor::gate`), so only the *resolution + install* step is missing on these paths.
- The dispatch-refusal half is executor-level and therefore already active anywhere an executor is
  built with `with_disabled_ops` — the gap is that these paths never call it, so neither surfacing
  nor refusal applies there today.
