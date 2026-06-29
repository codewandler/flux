---
id: D-09
title: Agentic channel target — wake an AgentSpec (not just a journey) on an event
pillar: Agent
status: in-progress
priority: 4
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
- [x] An `agent`-bound trigger routes an event to `FlowEngine::run_turn` against an `AgentSpec`, returning
      the answer as a single `JourneyRun { result }`. **Implemented as `trigger.agent` in flux-app** (reuses
      the existing `TriggerDecl.agent` field), *not* a flux-channels `EngineDeliverer` — the simpler fit,
      no adapter change. Test: `agent_trigger_runs_a_turn_and_returns_the_reply` (mock provider, no network).
      Committed `0d8ac58`.
- [x] A `(agent, conversation) → EventStore session` map so repeat events with the same
      `payload.conversation` append to one session (multi-turn thread), a fresh `conversation` gets a fresh
      session. Test: `same_conversation_reuses_one_session_distinct_ones_isolate`. (In-memory v1.)
- [x] Declared op grants: an `AgentDecl`'s `tools` become both the visible op subset **and** the pre-allow
      grants under a headless `DenyApprover` — granted ops run, everything else is denied, no blanket
      `--yes`. Test: `agent_spec_maps_tools_to_grants_and_persona`.
- [x] Target selection: an `agent`-bound trigger routes to the agent; a plain trigger runs its journey,
      unchanged (the journey route stays the default). Test: `trigger_without_agent_still_runs_its_journey`.
- [ ] **Registry wiring (remaining; depends on D-07/D-08):** the agent's registry is currently the App's
      builtins + cognition + orchestration — it must also get the **datasource retrieval tools (D-07)** and
      **plugin tools (D-08)** so the model has tools to drive. Factor the shared plugin/index assembly out
      of the CLI's `build_agent` so the `flux app run` path uses it too. Failing-first test: an agent target
      sees a registered plugin op + the `search` tool.
- [x] Full gate green for the mechanism (`cargo test -p flux-app`); `flux-app` layer placement unchanged.

## Progress
- **Mechanism landed & green** (`0d8ac58`): `trigger.agent` runs an agent turn with per-thread session
  memory + declared grants; 4 hermetic tests. Builds the `EngineTarget` the D-04 design deferred — via
  `trigger.agent` in flux-app rather than the originally-designed `EngineDeliverer` (the implemented shape
  reuses the existing Program field; see [[d04-channels-via-app-runner]]).
- **Remaining:** the registry wiring (datasource + plugin tools into the agent's registry) — blocked on
  **D-07** (datasource tools) and **D-08** (plugin tools). Land those, then close this.

## Notes
- **Implemented approach vs the design:** the committed mechanism is `trigger.agent`-in-flux-app
  (`crates/flux-app/src/app.rs` — `run_agent`/`agent_engine`/`session_for`/`agent_spec_from_decl`). The
  design doc's `EngineDeliverer`-in-flux-channels is recorded there as the *considered alternative*.
- Reuse, don't reimplement: `FlowEngine::run_turn` + `EventStore` sessions; `AgentSpec::assemble` for
  assembly. The op-grant seam pairs with — but does not require — **D-02** (tenant-tagged events).
- Serves the Slack-channel assistant **S-02/S-04** stories and managed-agents' background-agent direction. Non-goal: per-event
  trust/policy variation (the D-04 design's named follow-up).
