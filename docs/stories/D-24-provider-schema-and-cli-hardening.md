---
id: D-24
title: Harden provider schemas and CLI daemon ergonomics
pillar: Agent
status: done
note: "`emit_plan` now advertises the derived `DraftAst` schema, `flux plugin call` resolves short op names, served daemons honor SIGTERM, and `flux tui` fails clearly without a TTY"
---

# Harden provider schemas and CLI daemon ergonomics

## Goal
Make provider-facing planner/tool schemas derive from Rust types where the contract matters most, and
close the CLI/runtime rough edges surfaced by the parallel Claude sessions.

## Acceptance
- [x] `emit_plan` advertises the real `DraftAst` JSON Schema, not a placeholder object, with a regression
      test proving the schema carries `DraftAst`/`Node` definitions.
- [x] `ask_user` and the shared `ToolSpec` helper use schemars-derived input schemas with focused tests.
- [x] `flux plugin call <plugin> <short-op>` resolves to the plugin manifest's fully qualified op name
      while preserving explicit full-name calls.
- [x] Served daemons stop cleanly on Ctrl-C and SIGTERM.
- [x] `flux tui` fails clearly when invoked without a real terminal.
- [x] Focused crate tests pass.

## Progress
- **2026-06-30:** Added `flux_spec::tool_input_schema` and switched the planner's synthetic tools toward
  schemars-derived schemas. CLI hardening and final tests are still in progress.
- **Done (2026-06-30):** `emit_plan` now carries the derived `DraftAst` schema, `ask_user` uses a typed
  schema, `flux plugin call` resolves short op names against the manifest, `flux-server`/`flux-channels`
  honor SIGTERM, and `flux tui` fails early without a TTY. Focused gates: `cargo test -p flux-spec -p
  flux-flow`, `cargo test -p flux-cli -p flux-tui -p flux-server -p flux-channels`, non-TTY TUI smoke,
  and `app run --serve` SIGTERM smoke.

## Notes
- Follow-on: broader conversion of the built-in `flux-tools` hand-written schemas should be done in smaller
  op-family patches to keep behavior reviewable.
