---
id: D-165
title: Make the Slack support-bot example genuinely runnable (Slack default-on, agent-driven, program-relative docs)
pillar: Agent
status: done
priority:
theme: downstream-managed-services
note: "D-11 follow-on: the support-bot.flux example is advertised as `flux app run …` but couldn't run — Slack was feature-gated out of the binary, the example was internally contradictory (agent-bound trigger + dead journey), and its ./docs resolved against cwd. Now real end-to-end."
---

# Make the Slack support-bot example genuinely runnable

## Goal
`crates/flux-app/examples/support-bot.flux` is the headline "whole app in native flux-lang" demo, but a
`flux app run …support-bot.flux` on a stock binary failed. Make it real end-to-end: ship Slack in the
default binary, make the example internally coherent (agent-driven), and resolve its `./docs` corpus
beside the program file so it works from any directory. A D-11 (app-runner ergonomics) follow-on.

## Acceptance
- [x] Slack channel adapter compiled in by default — `default = ["slack"]` in `flux-cli` + `flux-channels`
      `Cargo.toml`; the `--no-default-features` bail message + the "feature-gated" docs updated.
- [x] Example rewritten to the agent-driven shape: `tools [search]` (dropped the inert `send`), the
      trigger is agent-bound with no `run`, and the dead `journey answer` removed. Explicit current model
      id `claude-sonnet-5` (the app path does not resolve per-agent model aliases). Failing-first test:
      `support_bot_example_covers_the_module_surface` (`crates/flux-app/tests/integration.rs`).
- [x] Parser: an agent-bound `trigger` no longer requires a `run` journey name (the runtime only reads
      `run` when there is no agent); a trigger with neither is a clear error. Tests:
      `agent_bound_trigger_needs_no_run_journey`, `trigger_with_neither_run_nor_agent_is_an_error`
      (`crates/flux-lang/src/parse.rs`).
- [x] Datasource relative `path` resolves against the **program file's directory**, not cwd; the program
      dir is added as a read-only root so out-of-cwd launches read the corpus. Test:
      `build_datasources_resolves_relative_path_against_program_dir` (`crates/flux-cli/src/main.rs`).
- [x] A real `crates/flux-app/examples/docs/*.md` corpus shipped beside the example.
- [x] Net-new Slack setup guide (`website/docs/agent/slack-channel.md`, in the sidebar); the verbatim
      example copy in `website/docs/agent/programs.md` updated to the new shape (still parses under the
      website-contract test); a program-relative-path note in `datasources.md`.
- [x] Full gate green in both feature configs (default + `--no-default-features`).

## Progress
- Done. Landed unreleased (pending the next MINOR — see Notes on SemVer).

## Notes
- **SemVer:** the datasource path-resolution change is a behavior change to `flux app run` (a program
  relying on cwd-relative datasource paths would resolve differently) → MINOR under the flux rule
  (breaking/behavior → MINOR in 0.y). Slack-default-on is purely additive.
- **Trade-off (accepted):** the default build now pulls `slack-morphism` + a second rustls crypto
  provider (aws-lc-rs alongside ring) → larger binary + longer CI, but the adapter finally gets CI
  compile coverage.
- **Latent gap (out of scope):** per-agent `model "sonnet"` aliases are NOT resolved in the app path
  (`build_agent_engine` passes `decl.model` straight through) — the example uses an explicit id. Worth a
  follow-up to run `decl.model` through the provider's `resolve_model`.
- Design trail: `docs/designs/event-trigger-channels.md` (channels), `docs/stories/D-11-app-runner-ergonomics.md` (theme parent).
