# Design: agentic channel target (`EngineDeliverer`)

**Status:** proposed (story [D-09](../stories/D-09-agentic-channel-target.md)) · **Layer:** L6
(`flux-channels`, + a small `flux-app` seam) · **Owner:** Timo

## Why

[D-04](event-trigger-channels.md) shipped event-trigger channels routing each event to a **journey** (a
Flux-Lang DAG) via `flux_app::App::deliver` — deliberately the App-runner route, superseding D-04's
originally-spec'd `EngineTarget`. That is the right fit for a **scheduled/declarative** background agent
(cron → summary journey). It is the *wrong* fit for an **open-ended conversational assistant** — the
downstream Slack-channel assistant, which on a Slack mention must let the **model drive**: pick among ~8 integration
tools, call them, iterate, and answer. That is an agent loop (`FlowEngine::run_turn`), not a DAG.

This design adds an **agentic target** alongside the journey route: a channel can wake an `AgentSpec` turn,
with **per-conversation session memory** and **declared op grants**. The journey route stays the default and
is unchanged.

## The seam already exists

The Slack adapter does not know what runs an event — it calls the **`Deliverer`** trait and posts the
joined result back to the thread:

```rust
// crates/flux-channels/src/deliver.rs (shipped)
pub trait Deliverer: Send + Sync {
    async fn deliver(&self, label: &str, payload: Value) -> Result<Vec<JourneyRun>>;
}
```

`AppDeliverer` routes to `App::deliver` → triggers → journeys. We add a second impl; **no adapter change**.

## Shape — three pieces

### 1. `EngineDeliverer` (the agentic target)
```rust
pub struct EngineDeliverer {
    engine: Arc<FlowEngine>,        // assembled once from an AgentSpec (AgentSpec::into_engine)
    events: Arc<EventStore>,        // the persistent session store
    sessions: Mutex<HashMap<String, String>>, // conversation id → session id (in-memory v1)
}
```
`deliver(label, payload)`:
1. `conv = payload["conversation"]` (Slack thread ts; falls back to the channel id — the adapter already
   computes this, `adapters/slack.rs:165`). For a label with no conversation (cron), one session per run.
2. `sid = sessions.entry(conv).or_insert_with(|| events.create_session(model))` — **bind the thread to a
   persistent session** so repeated mentions append to one conversation log (multi-turn).
3. `text = payload["text"]`; run `engine.run_turn(&sid, &text, &mut sink).await`.
4. Return `vec![JourneyRun { journey: "<agent>", result: sink_final_text, steps }]` — the Slack adapter
   joins `.result` and posts it. One agent turn → one reply.

Per-conversation serialization is the `Deliverer`'s `gate` (same as `AppDeliverer`): one in-flight turn per
process today; per-conversation locking is a cheap follow-up if needed.

### 2. Per-conversation session memory
The `conversation → session` map is the crux: a stable id (Slack thread ts) maps to a persistent
`EventStore` session so a thread accumulates history and `await`/resume flows continue; a fresh thread gets
a fresh session. **In-memory map for v1** (a restart starts threads fresh — flagged, matches D-04's
in-memory-only caveat); a durable `conversation → session` index pairs naturally with **D-02**.

### 3. Declared op grants (headless authorization)
`flux-app`'s `build_executor` hardcodes the allow-list + a binary approver
(`crates/flux-app/src/app.rs:280`). The agentic target needs to authorize the bot's **specific** integration
ops (e.g. `gitlab.*`, `slack.post`) under the headless approver **without** blanket `--yes`. Add a small
seam: the assembly takes a **grant list** (op-name globs) that pre-allow those ops; everything else still
falls to `DenyApprover`. The bot declares its grants in the program (top-level `grants = [...]` or per the
`allow_plugin_access` config the bot already carries). This keeps "trusted, pre-authored program" from
meaning "allow everything."

## Wiring (`flux app run`)
`flux app run <program.flux>` builds the `App`, then `build_channels` + `serve`
(`crates/flux-cli/src/main.rs:3176`). Add: if the `Program` declares a top-level **agent target** (an
`AgentSpec` + grants), `serve` is handed an `EngineDeliverer` for that agent instead of (or alongside) the
`AppDeliverer`; channels whose trigger names the agent route to it, journeys route as before. v1 keeps it
simple: **one target per program** (agent *or* journeys), selected by whether the program declares an agent.

## Testing (hermetic — no provider, no network)
- **Agent turn:** a `MockProvider` (the pattern in `flux-flow`/`flux-sdk` tests) behind a real
  `EngineDeliverer`; a synthetic Slack payload drives one `run_turn` and the reply equals the mock's text —
  proves the agentic path with no journey.
- **Session binding:** two deliveries with the same `payload.conversation` resolve to one session id;
  distinct conversations get distinct ids (assert against the `EventStore`).
- **Op grants:** with `grants = ["gitlab.*"]`, a `gitlab.list_mrs` dispatch is allowed; an ungranted op
  (`bash`) is denied by the headless approver.

## Implementation references (seams to build on)

| Seam | Symbol | Location |
|------|--------|----------|
| The deliverer seam | `Deliverer` / `AppDeliverer` | `crates/flux-channels/src/deliver.rs` |
| Run one agent turn | `FlowEngine::run_turn(session_id, input, sink)` | `crates/flux-flow/src/engine.rs:132` |
| Engine assembly from a spec | `AgentSpec::into_engine` | `crates/flux-agent/src/lib.rs:117` |
| Session create/reuse | `EventStore::create_session(model)` | `crates/flux-events/src/store.rs:117` |
| Headless executor (extend with grants) | `build_executor` | `crates/flux-app/src/app.rs:280` |
| App-runner wiring | `flux app run` → `build_channels`/`serve` | `crates/flux-cli/src/main.rs:3176` |

## Non-goals (v1) / named follow-ups
- Durable `conversation → session` index (in-memory v1; durable pairs with D-02).
- Per-event trust/policy variation (D-04's named follow-up) — every event runs under the agent's fixed
  grants + headless approver.
- Multiple agent targets per program; per-channel target selection (v1 is one target per program).
- Streaming partial replies to Slack (post-once at turn end, as the shipped adapter does).
