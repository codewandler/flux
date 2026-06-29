---
id: D-11
title: App-runner ergonomics for declarative bots (knowledge-ingest config, OpenAPI, persona/event-context from file)
pillar: Agent
status: backlog
priority:
theme: downstream-managed-agents
design:
---

# App-runner ergonomics for declarative bots

## Goal
Make `flux app run <program.flux>` a viable host for a real declarative bot/app, not just a demo. Three small,
additive app-runner gaps surfaced by the downstream Slack-channel assistant (`bot.flux`, the second downstream consumer):
boot knowledge ingest is hardcoded, an agent's system prompt can't come from a file, and an event-woken agent
gets no event context.

## Why (motivating consumer: Slack-channel assistant)
- **S-03** can't faithfully index its knowledge: `build_doc_index` (`crates/flux-cli/src/main.rs`) is
  hardcoded — markdown/text only, depth-4, 200-doc cap, source `"local"`, **no OpenAPI** — so the bot's deep
  help-center articles + `service-manager.openapi.json` aren't indexed (D-07 already ships `ingest_openapi`,
  just uncalled on this path).
- **S-01 / S-06** must inline `bot/PERSONA.md` / `bot/MONITOR.md` into `bot.flux` because
  `agent_spec_from_decl` (`crates/flux-app/src/app.rs`) builds the system prompt from
  `description` / `settings.system_prompt` only.
- **S-06** monitor can't tell startup from a tick or read the tick time: `run_agent` forwards only
  `payload.text` to the turn, dropping the event label + the schedule `at`.

## Acceptance
- [ ] **Configurable knowledge ingest** for `flux app run`: a knowledge dir/globs (+ adequate depth) from the
      program or App options, ingesting markdown **and** OpenAPI JSON (call the existing `ingest_openapi`) as
      typed records. Failing-first test: ingest a fixtures dir incl. an OpenAPI spec → `search` returns a
      `file.document` and an `openapi.operation` record.
- [ ] **System-prompt-from-file**: `agent_spec_from_decl` accepts `settings.system_prompt_files` (paths),
      concatenated into the prompt. Test: an agent decl with a prompt file → `spec.system_prompt` contains it.
- [ ] **Event context to agent turns**: `run_agent` forwards the trigger label + payload (e.g. schedule `at`)
      into the turn so a scheduled agent can branch startup-vs-tick and read the time. Test: a scheduled
      trigger's agent turn sees the `at`/label.
- [ ] Additive — existing programs/agents unchanged; full gate green.

## Progress
- Backlog. Surfaced by Slack-channel assistant S-01/S-03/S-06 (`bot.flux` interim workarounds: inline personas, generic
  markdown walk, best-effort monitor). Sibling: **L-03** (native-text program grammar).

## Notes
- Relevant: `crates/flux-cli/src/main.rs` (`run_app`, `build_doc_index`), `crates/flux-app/src/app.rs`
  (`agent_spec_from_decl`, `run_agent`), `crates/flux-capabilities/src/datasource/ingest.rs`
  (`ingest_openapi`). Sliceable into 3 small PRs (ingest-config, persona-file, event-context).
