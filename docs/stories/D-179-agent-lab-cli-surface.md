---
id: D-179
title: Agent Lab CLI — flux record / flux test / resurrect-on-open
pillar: Agent
status: backlog
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
note: "Phase 5 — CLI as the reference app on the SDK; depends on D-174/D-178"
---

# Agent Lab CLI — flux record / flux test / resurrect-on-open

## Goal
Expose the Deterministic Agent Lab through the CLI (the reference app built on the SDK): record
scenarios, run them offline as a test gate, and transparently resume interrupted sessions.

## Acceptance
- [ ] `flux record <name> "<prompt>"` writes a `tests/scenarios/<name>/` fixture (a `Storage::dir`
      directory) via `Scenario::record`.
- [ ] `flux test [<name>]` runs `Scenario::replay` + the fixture's assertions offline ($0, no key, no
      network); exit code reflects pass/fail; a regression prints the plan-source + world diff.
- [ ] Resurrect-on-open: when `auto_resurrect` is on, `flux sessions`/session open resumes an
      interrupted turn and reports it.
- [ ] Fixtures remain openable by the existing `flux replay`/`fork`/`diff` (same `Storage::dir`
      layout) — an integration test proves cross-tool compatibility.
- [ ] Follows CLI conventions: explicit subcommands, no implicit default-run.

## Progress
- (not started — epic deferred; docs-only for now)

## Notes
- Files: `crates/flux-cli/src/{args,dispatch,session}.rs`.
- Depends on D-174 (Test Kit) and D-178 (Resurrect). Website docs/ops-catalog mirrors as needed
  (website is contract-tested).
