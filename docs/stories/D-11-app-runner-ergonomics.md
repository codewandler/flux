---
id: D-11
title: App-runner ergonomics for declarative bots (knowledge-ingest config, OpenAPI, persona/event-context from file)
pillar: Agent
status: done
priority: 1
theme: downstream-managed-services
note: "the ready pick: configurable `flux app run` knowledge ingest + OpenAPI + persona/event-context-from-file; makes it a viable host for a declarative bot, unblocking Slack-channel assistant flows"
---

# App-runner ergonomics for declarative bots

## Goal
Make `flux app run <program.flux>` a viable host for a real declarative bot/app, not just a demo. Three small,
additive app-runner gaps surfaced by a downstream Slack-channel assistant:
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
- [x] **Configurable knowledge ingest** for `flux app run`: a knowledge dir/globs (+ adequate depth) from the
      program or App options, ingesting markdown **and** OpenAPI JSON (call the existing `ingest_openapi`) as
      typed records. Failing-first test: ingest a fixtures dir incl. an OpenAPI spec → `search` returns a
      `file.document` and an `openapi.operation` record. **Done** — the `flux app run` path already routes
      through `build_datasources` (`crates/flux-cli/src/main.rs`), which ingests each program-declared
      `datasource` by kind (`markdown` walks a dir at depth 4000 / 1000-doc / 200k-byte caps; `openapi` reads a
      JSON spec via `ingest_openapi`); locked by `build_datasources_ingests_markdown_and_openapi_searchable`.
- [x] **System-prompt-from-file**: `agent_spec_from_decl` accepts `settings.system_prompt_files` (paths),
      concatenated into the prompt. Test: an agent decl with a prompt file → `spec.system_prompt` contains it.
      **Done** — `agent_spec_from_decl` (`crates/flux-app/src/app.rs`) is now async, reads each
      `settings.system_prompt_files` path through the guarded, workspace-confined `System`, and concatenates
      them after the base persona (a non-string entry or unreadable path is a clean, attributed error). Test
      `agent_spec_appends_system_prompt_files`.
- [x] **Event context to agent turns**: `run_agent` forwards the trigger label + payload (e.g. schedule `at`)
      into the turn so a scheduled agent can branch startup-vs-tick and read the time. Test: a scheduled
      trigger's agent turn sees the `at`/label. **Done** — `run_agent` synthesizes `event_context(label,
      payload)` as the turn input when an event carries no user `text` (`crates/flux-app/src/app.rs`); locked
      end-to-end by `scheduled_agent_turn_receives_event_context` (an echo provider surfaces the exact turn
      input the engine fed the model).
- [x] Additive — existing programs/agents unchanged; full gate green.

## Progress
- **Done.** Two of the three gaps had landed incrementally without the story being closed: the
  markdown+OpenAPI ingest came with **L-03**'s `build_datasources` (`585acea`) and the event-context synthesis
  with `443d4cb` — both were missing their acceptance tests, now added (locking the behavior). The genuine
  implementation gap, **system-prompt-from-file**, is the new work: `agent_spec_from_decl` reads
  `settings.system_prompt_files` through the guarded `System`, which made the spec/engine-build chain async
  (`agent_spec_from_decl` → `build_agent_engine` → `Engine`/`App::agent_engine` → the `a2a` channel's
  `from_decl_and_app` + its `host::serve` caller); the agent cache is built off-lock so no `MutexGuard` is held
  across the file-read await. Sibling: **L-03** (native-text program grammar).

## Notes
- Relevant: `crates/flux-cli/src/main.rs` (`run_app`, `build_doc_index`), `crates/flux-app/src/app.rs`
  (`agent_spec_from_decl`, `run_agent`), `crates/flux-capabilities/src/datasource/ingest.rs`
  (`ingest_openapi`). Sliceable into 3 small PRs (ingest-config, persona-file, event-context).
