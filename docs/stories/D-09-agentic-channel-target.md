---
id: D-09
title: Agentic channel target — wake an AgentSpec (not just a journey) on an event
pillar: Agent
status: backlog
priority:
theme: downstream-managed-agents
design: docs/designs/agentic-channel-target.md
---

# Agentic channel target — wake an `AgentSpec` (not just a journey) on an event

## Goal
Let a channel wake an **agent turn** — an `AgentSpec` run through `FlowEngine::run_turn` where the model
drives RAG + tools — **as an alternative to** the shipped journey route, with **per-conversation session
memory** and **declared op grants**. This is what lets `flux-channels` host an open-ended *conversational
assistant* (a Slack DevOps bot that picks among many integrations and iterates), not only a fixed
Flux-Lang DAG.

## Why (downstream: Slack-channel assistant, managed-agents)
The downstream **Slack-channel assistant rewrite** (second downstream flux consumer; replacing the fluxplane Go bot) is a
tool-using assistant: on a Slack mention it must decide which of ~8 integrations to query, call them, maybe
iterate, and answer — an **agent loop**, not a pre-authored DAG. managed-agents wants the same "background agent
woken by events" breadth. The shipped journey route (D-04) is the right fit for a **scheduled monitor** (cron
→ summary journey) but cannot express open-ended model-driven tool use.

## flux gap (grounded in the shipped code)
D-04 shipped channels routing each event to a **journey** via `flux_app::App::deliver` — deliberately the
App-runner/`ProgramTarget` route, superseding the D-04 design's `EngineTarget`. Consequences for an agentic
bot, all verified in-tree:
- **No agent turn.** A journey is a DAG run by `execute_flow`; the App registry exposes builtins +
  orchestration (`emit`/`send`/`ask`/`spawn`) + cognition (`ai.*`) but **no model-drives-tools turn**
  (`crates/flux-app/src/app.rs:163`). `spawn` re-enters another *journey*, not an agent loop.
- **No thread memory.** Each delivery runs on a **fresh in-memory `FlowStore`** with a unique session id
  (`crates/flux-app/src/app.rs:245`); the Slack `conversation` id is seeded into the payload
  (`crates/flux-channels/src/adapters/slack.rs:170`) but nothing binds it to a persistent session.
- **Coarse authorization.** `build_executor` hardcodes an allow-list (`emit/send/ask/spawn/read/glob/grep/
  search`) + a binary `Allow`/`Deny` approver (`crates/flux-app/src/app.rs:280`); integration ops can only
  run under blanket `--yes`.

The clean injection point already exists: the **`Deliverer` trait**
(`crates/flux-channels/src/deliver.rs`) is what the Slack adapter calls, and the adapter just joins
`runs[].result` → posts to the thread — so an alternative `Deliverer` needs **no adapter change**.

## Acceptance
- [ ] A new `Deliverer` (e.g. `EngineDeliverer`) routes an event to `FlowEngine::run_turn` against a
      configured `AgentSpec`, returning the answer as a single `JourneyRun { result }`. Failing-first test:
      a synthetic Slack-shaped payload + a `MockProvider` drives one agent turn and returns its text (no
      journey, no network).
- [ ] A `conversation → EventStore session` map so repeat events with the same `payload.conversation`
      append to one session (multi-turn thread), a fresh `conversation` gets a fresh session. Failing-first
      test: two deliveries with the same `conversation` share a session; distinct ones stay isolated.
- [ ] A per-program op-grant seam so the program authorizes its integration ops under the headless approver
      **without** blanket `--yes`. Failing-first test: a program granting `gitlab.*` can dispatch it; an
      ungranted op is denied.
- [ ] Target selectable from the program/CLI (`flux app run` — e.g. a top-level `AgentSpec`/`target` knob
      in the `Program`); the journey route stays the default and is unchanged.
- [ ] Full gate green (`cargo build/test/clippy/fmt`, `cargo test -p flux-codegate`); `flux-channels`/
      `flux-app` layer placement unchanged.

## Progress
- Backlog. Builds the `EngineTarget` capability the D-04 design specified but the impl intentionally
  deferred (see [[d04-channels-via-app-runner]] / `docs/designs/event-trigger-channels.md`).

## Notes
- Reuse, don't reimplement: `FlowEngine::run_turn` + `EventStore` sessions (the channels design's
  *Implementation references* table lists every seam); `AgentSpec::into_engine` for assembly; the
  `Deliverer`/`AppDeliverer` pattern in `crates/flux-channels/src/deliver.rs`.
- The op-grant seam pairs with — but does not require — **D-02** (tenant-tagged events); single-tenant bot
  needs only the `conversation → session` binding, not account tags.
- Serves the Slack-channel assistant **S-02/S-04** stories and managed-agents' background-agent direction. Non-goal: per-event
  trust/policy variation (the D-04 design's named follow-up).
