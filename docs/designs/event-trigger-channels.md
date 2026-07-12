# Design: event-trigger channels (consolidated)

**Status: shipped.** This is the consolidated design record for the channels epic — it merges the two
channel design docs (stories D-04, D-09). The shipped behavior is documented in the living
[usage guide](../usage.md) (multi-agent programs) and on the [story board](../stories/README.md).

Consolidated docs, in narrative order:
1. **Event-trigger channels** (D-04) — cron / webhook / Slack triggers waking a journey via `flux app run`.
2. **Agentic channel target** (D-09) — waking an `AgentSpec` (not just a journey) on an event via `trigger.agent`.

---

# Design: event-trigger channels (`flux-channels`)

**Status:** implemented (story [D-04](../stories/D-04-event-trigger-channels.md)) · **Layer:** L6 (new
crate `flux-channels`) · **Owner:** Timo

> The originally-spec'd agentic **`EngineTarget`** (route an event to an `AgentSpec` `run_turn` so the model
> drives RAG + tools, with per-conversation memory) was deferred in favour of the journey route below; it is
> now tracked as **[D-09](../stories/D-09-agentic-channel-target.md)** (agentic channel target — a new
> `Deliverer` alongside the journey route), driven by downstream Slack-channel assistant use cases.

## Why

flux agents are reached request/response — the CLI/REPL, the HTTP `webhook`/A2A endpoints, voice
downstream. There was no way to run an agent that is **idle and woken by an external event**: a nightly
cron summary, an inbound webhook, a Slack mention. This is the channel breadth behind downstream managed
services and the "background agents" use case directly.

## Approach — channels are Program-declared and run by the app runner

The flux-app **Program** model already had the right shape, so channels reuse it rather than introducing
a parallel host:

- A channel is an ordinary [`ChannelDecl`](../../crates/flux-lang/src/program.rs) on the Program:
  `{ name, kind, settings }`, where `kind` is free-form (`schedule`/`webhook`/`slack`) and `settings` is
  an opaque JSON bag the host interprets.
- A channel **fires a bus event under its own name**; a `TriggerDecl { on: "<channel name>", run:
  "<journey>" }` routes it (exact label match) to a journey, which runs via
  `flux_app::App::deliver(label, payload) → run_triggers → run_journey → flux_flow::runtime::execute_flow`.
  The event payload is seeded into the journey's flow store, so the flow reads it with `{field}`.
- The **app runner** (`flux app run <program.flux>`) builds the `App`, builds the channels from
  `program.channels`, and starts them. No separate channels config, CLI verb, or single-agent target.

So `flux-channels` is a thin L6 crate carrying only the external-I/O adapters (the heavy deps: `axum`,
`cron`/`chrono`, a feature-gated Slack SDK) plus a small host. It **depends on flux-app**; flux-app is
unchanged and stays free of those deps (no dependency cycle).

> This supersedes an earlier draft of this design (a standalone host with a `flux channels run` CLI, a
> TOML config, and a single-agent `EngineTarget`). That cut against the grain — the Program's
> bus/triggers/journeys already do the routing — so the app-runner model replaced it.

## Shape — the `flux-channels` crate

```
Channel (trait)   name() + start(deliverer, cancel) — a long-running event source
Deliverer (trait) deliver(label, payload) -> Vec<JourneyRun> — the seam a channel calls to wake the app
AppDeliverer      the production Deliverer: gate-serialized App::deliver
serve(app, channels, run_stdin, cancel)  the host: fire startup, spawn channels, await Ctrl-C/cancel
build_channels(&[ChannelDecl]) -> Vec<Box<dyn Channel>>   kind → adapter; skips `cli`; unknown = error
adapters: schedule (cron+chrono) · webhook (axum) · slack (slack-morphism, default feature `slack`)
```

- **`Channel::start(d, cancel)`** runs the adapter's protocol loop until `cancel`; per external event it
  calls `d.deliver(self.name(), payload)` and uses the returned `Vec<JourneyRun>` for a reply (webhook
  response / Slack thread post) or ignores it (cron).
- **`Deliverer`** is a seam so adapters are testable without a real `App` (a recording double in tests).

### Concurrency — why deliveries are serialized

`AppDeliverer` holds a `tokio::sync::Mutex<()>` gate and serializes `App::deliver`. `App::deliver`
subscribes to the broadcast `Bus` and drains the cascade events its journeys emit; two concurrent
deliveries would each *also* receive the other's cascade events (broadcast fan-out) and double-process
them. One in-flight delivery at a time avoids that. Journeys themselves run on independent per-run
stores (`execute_flow`, session `{name}#{n}`), so this gate is the **only** serialization point — and
note this is unrelated to the `FlowEngine`/loop_host single-turn constraint, which the Program path
does not use. Cross-channel concurrent delivery (per-delivery bus isolation / correlation) is a
follow-up.

## Adapters

### schedule (`kind = "schedule" | "cron"`)
`cron` + `chrono`. `settings { schedule: "0 9 * * *" }` (a cron timer) or `{ on: "startup" }` (one-shot).
The loop is `sleep_until(schedule.after(now).next())` → `deliver(name, { at, name })`. **Cron format:**
both a familiar 5-field crontab (`"0 9 * * *"`) and the `cron` crate's native 6/7-field seconds-first
(`"* * * * * *"`) are accepted — a 5-field string is normalized by prepending `"0 "` for the seconds
slot. UTC only (per-entry timezone is a follow-up). Fire-and-forget; results are logged.

### webhook (`kind = "webhook" | "http"`)
`axum`. Each webhook channel runs its **own** server on `settings.addr`; `POST settings.path` delivers
the JSON body under the channel name and replies with the journeys' results as JSON. `settings.async =
true` replies `202 Accepted` and runs fire-and-forget. Optional bearer `token` (`token secret "KEY"`,
host-resolved before the adapter reads settings), compared in constant time. A **non-loopback `addr`
requires a `token`** (the host
auto-approves tools, so an open listener is a remote-trigger surface — mirrors flux-server). HMAC and a
shared multi-channel server are follow-ups.

### slack (`kind = "slack"`, default feature `slack`)
`slack-morphism` socket mode. Compiled in by default so `channel slack` runs on the stock binary; the
`slack` feature can still be dropped with `--no-default-features` to keep the dep tree out of a build.
Subscribes to app-mentions and human messages (bot/subtype messages are skipped to avoid reply loops);
delivers `{ text, user, channel, thread, conversation }` under the channel name and posts the journeys'
joined result back to the thread. `allow_users` / `allow_channels` settings gate access; bot/app tokens
come via `secret "ENV_NAME"` references (host-resolved at load). Live validation needs a real Slack app
— the hermetic tests cover the
event→payload mapping and the allow-list only.

> **slack-morphism version:** capped at `>=2.10, <2.18`. 2.18+ require `signal-hook-tokio ^0.4`, which
> does not exist on crates.io (max 0.3.1), so their socket-mode feature is unbuildable; 2.17 is the
> newest resolvable release.

## CLI

`flux app run <program.flux>` (a new explicit subcommand) builds the `App`, builds the channels, and
calls `flux_channels::serve`. The existing `flux run <app.flux>` auto-detect routes through the **same**
code path, so it now starts channels too. `serve` reads the interactive `cli` stdin loop when the program
declares a `cli` channel — or declares no channels at all (preserving the plain read-eval-print default);
a program with only background channels runs as a daemon until Ctrl-C. Destructive ops are denied without
`--yes` (the headless default).

## Testing (hermetic — no provider, no network)
- `routing.rs` — a delivered event runs the matching journey (pure-op flow returns a literal); an
  unmatched label runs nothing; concurrent deliveries are serialized without corruption.
- `schedule.rs` — a fast cron (`"* * * * * *"`) delivers one event per tick with `{ at, name }`; an
  `on:"startup"` channel fires once; a 5-field crontab parses.
- `webhook.rs` — a `POST` becomes a delivery and returns the journeys' results (sync) / `202` (async);
  a non-loopback bind without a token is rejected.
- `e2e.rs` — a fast cron channel wakes a **real** `App` whose journey formats the seeded payload field;
  asserts timer → deliver → trigger → journey → result, with no provider.
- slack (default feature, in-module) — event→payload mapping (thread as conversation) + allow-list.

`examples/channels-app.flux` (a cron heartbeat + a webhook) demonstrates `flux app run`.

## Reuse, don't reimplement
- flux-app's `App::deliver` + bus + triggers + journeys (`execute_flow`) — the routing and run path.
- flux-server's axum patterns / constant-time token compare — the webhook.
- The `ChannelDecl` / `TriggerDecl` Program model — no new language node kinds.

## Non-goals (v1) / named follow-ups
- Cross-channel **concurrent delivery** (per-delivery bus isolation / correlation; today serialized).
- Durable scheduling / missed-tick replay; per-entry timezone.
- Slack multi-turn thread → a persistent journey session (reply-parking / `ask`); per-event trust/policy.
- A shared webhook server across channels; webhook SSE/streaming; HMAC.
- Live Slack app validation (manual, needs real credentials).
- Multi-tenant event tagging — that's [D-02](../stories/D-02-tenant-event-substrate.md); this composes
  with it for per-account triggered-run history.

## Implementation references (the seams built on)

| Seam | Symbol | Location |
|------|--------|----------|
| Route an event → journeys | `App::deliver(label, payload) -> Vec<JourneyRun>` | `crates/flux-app/src/app.rs:104` |
| Trigger match (exact `on == label`) | `Engine::run_triggers` | `crates/flux-app/src/app.rs:181` |
| Journey run (fresh store, seeds payload) | `run_journey` → `execute_flow` | `crates/flux-app/src/app.rs:224` |
| Channel declaration (free-form kind/settings) | `ChannelDecl` / `TriggerDecl` | `crates/flux-lang/src/program.rs:42`,`:53` |
| Program parse | `Module::parse_str` | `crates/flux-lang/src/program.rs:137` |
| App construction (Arc-able, `&self` deliver) | `App::with_options` | `crates/flux-app/src/app.rs` |
| CLI app runner | `flux app run` → `run_app` | `crates/flux-cli/src/main.rs` |
| Layer map (`flux-channels` = L6) | `layer()` | `crates/flux-codegate/src/lib.rs:37` |


---

# Design: agentic channel target

**Status:** **mechanism implemented** (story [D-09](../stories/D-09-agentic-channel-target.md), commit
`0d8ac58`) · **Layer:** L6 (`flux-app`) · **Owner:** Timo

> **Implemented as `trigger.agent` in flux-app — not the `EngineDeliverer`-in-flux-channels shape this doc
> first proposed.** An `agent`-bound trigger (the existing `TriggerDecl.agent` field) runs a `FlowEngine`
> agent turn instead of a journey, with an `(agent, conversation) → EventStore` session map and grants from
> the `AgentDecl`'s `tools` under a headless `DenyApprover`. This reuses an existing Program field and needs
> no adapter change, so it was preferred over adding a parallel `Deliverer`. The seams:
> `crates/flux-app/src/app.rs` — `run_agent` / `agent_engine` / `session_for` / `agent_spec_from_decl` /
> `build_agent_engine`. The `EngineDeliverer`-in-flux-channels write-up below is retained as the
> **considered alternative**. **Remaining D-09 work:** register datasource (D-07) + plugin (D-08) tools
> into the agent's registry (today it sees only the App's builtins/cognition/orchestration).

## Why

[D-04](event-trigger-channels.md) shipped event-trigger channels routing each event to a **journey** (a
Flux-Lang DAG) via `flux_app::App::deliver` — deliberately the App-runner route, superseding D-04's
originally-spec'd `EngineTarget`. That is the right fit for a **scheduled/declarative** background agent
(cron → summary journey). It is the *wrong* fit for an **open-ended conversational assistant** — a
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

### Registry wiring — the app path must load plugins + datasource tools
The agent target is only useful with tools to drive. Today **only the CLI agent path** (`build_agent`,
`crates/flux-cli/src/main.rs:742`) loads subprocess plugins (`load_plugin_tools` / `discover`) and registers
the datasource `search` tool (`build_doc_index`); the **app/journey path does not** (`Engine::new` registers
only builtins + orchestration + cognition, `crates/flux-app/src/app.rs:151`). So D-09 also **factors that
plugin + datasource-index assembly into a shared helper** and has the `EngineDeliverer`'s registry include:
builtins + orchestration + the **D-07 retrieval ops** + the **D-08 plugin tools** (the program's
`allow_plugin_access`/declared plugins), authorized by the program's **op-grants**. This is the seam that
lets a Slack mention drive RAG `search` + `gitlab.*`/`slack.*` ops in one turn.

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
| Plugin load + datasource index (today CLI-only; share it) | `load_plugin_tools`/`discover`, `build_doc_index` | `crates/flux-cli/src/main.rs:742` |
| App registry (add plugin + datasource tools) | `Engine::new` | `crates/flux-app/src/app.rs:151` |

## Non-goals (v1) / named follow-ups
- Durable `conversation → session` index (in-memory v1; durable pairs with D-02).
- Per-event trust/policy variation (D-04's named follow-up) — every event runs under the agent's fixed
  grants + headless approver.
- Multiple agent targets per program; per-channel target selection (v1 is one target per program).
- Streaming partial replies to Slack (post-once at turn end, as the shipped adapter does).
