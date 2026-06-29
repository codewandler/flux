# Design: event-trigger channels (`flux-channels`)

**Status:** proposed (story [D-04](../stories/D-04-event-trigger-channels.md)) · **Layer:** L6 (new
crate) · **Owner:** Timo

## Why

flux agents today are reached request/response — the CLI/REPL, the HTTP `webhook`/A2A endpoints, voice
downstream. There is no way to run an agent that is **idle and woken by an external event**: a nightly
cron summary, an inbound webhook, a Slack mention. This is the channel breadth behind the downstream
managed-agents product (which has only RTVBP + A2A channels) and the "background agents" use case directly.
fluxplane (Go) is the proven prior art for the shape; this lifts that shape into flux, reusing flux's
engine, event store, and (optionally) flux-app's Program/bus rather than forking them.

**Decisions:** a **dedicated `flux-channels` (L6) crate** (not folded into flux-app, to keep the heavy
adapter deps — axum, a Slack SDK, a cron crate — out of flux-app's always-loaded tree); and **full cron
expressions** in the first schedule slice.

## Shape — one crate, five pieces

A new **`flux-channels`** L6 crate. The core is small and dep-light; each protocol adapter is a
feature-gated module so the heavy deps stay optional.

1. **`Channel` trait** — a long-running event source.
   ```rust
   #[async_trait]
   pub trait Channel: Send + Sync {
       fn name(&self) -> &str;
       /// Run the protocol loop until the ctx's token cancels, calling `ctx.run(..)` per event.
       async fn start(&self, ctx: ChannelCtx) -> Result<()>;
   }
   ```
   `ChannelCtx` exposes **one** routing seam and a `CancellationToken`:
   `ctx.run(inbound, sink) -> Result<Outbound>` — the host does per-conversation serialization +
   `Target.run` and hands back the final `Outbound`. The adapter passes a sink it cares about
   (thread-poster for Slack, no-op for a webhook that just wants the returned `Outbound`, logger for
   cron) and `await`s. The adapter owns its protocol loop (cron timer, axum server, Slack socket); the
   host owns routing, session binding, and concurrency.

2. **Normalized event envelope** (fluxplane-style — decouples protocol from the agent).
   ```rust
   pub struct Inbound {
       pub channel: String,            // source channel name
       pub conversation: String,       // stable id → session binding (thread / webhook key / job name)
       pub text: String,               // what the agent receives as input (EngineTarget)
       pub label: Option<String>,      // trigger label for ProgramTarget (e.g. "cron:nightly"); None for EngineTarget
       pub payload: serde_json::Value, // structured extras (cron {at,name}, webhook body, slack meta)
       pub meta: serde_json::Value,    // adapter identity/context (slack user+channel, webhook headers) — see trust note
   }
   pub struct Outbound { pub text: String, pub payload: serde_json::Value }
   ```
   **Trust note (honest scoping):** flux's policy lives in the `Executor` baked into the `FlowEngine` at
   assemble time — it is **per-engine, not per-event**. So v1 runs every event under one fixed policy
   (the target agent's permissions + a headless approver); caller identity travels in `meta` as context
   only. Per-event trust (a Slack operator getting a looser policy than a public user) needs a per-run
   approver/policy seam on `run_turn` — called out as a follow-up below, not built in v1.

3. **`Target` seam** — what an inbound event runs against (keeps the host agnostic to single-agent vs
   program).
   ```rust
   #[async_trait]
   pub trait Target: Send + Sync {
       async fn run(&self, ev: &Inbound, sink: &mut dyn AgentSink) -> Result<Outbound>;
   }
   ```
   - **`EngineTarget(FlowEngine)`** — **the v1 target**, and the whole "a background agent woken by
     events" story. Maps `ev.conversation` → an `EventStore` session (create-once, reuse), calls
     `engine.run_turn(&session_id, &ev.text, sink)`. This is exactly the flux-server `webhook` pattern,
     session-bound so a multi-turn channel (Slack thread) appends to one conversation log. Runs a **D-01
     parameterized flow** when that lands (seed `ev.payload` as inputs).
   - **`ProgramTarget(flux_app::App)`** — **optional, deferred** (the reason `Target` is a trait, not a
     concrete type). For a declarative multi-journey `Program`, call `app.deliver(ev.label?, ev.payload)`
     so its existing triggers/journeys fire, folding the `Vec<JourneyRun>` results into one `Outbound`.
     This is the "reuse flux-app's bus/triggers where it fits" path; spec it but don't build it in the
     first slices. The trait also gives tests a `MockTarget`.

4. **`Host`** — the daemon. Owns the set of `Channel`s + one `Target`, a `CancellationToken`, and a
   per-conversation serialization map. `Host::run()` starts every channel as a tokio task; each
   `ctx.run(inbound, sink)` call routes to `target.run(ev, sink)`, serializing same-`conversation` runs
   and running different conversations concurrently. Graceful shutdown via the shared token (below).

5. **Per-run `AgentSink` bridge** — each adapter supplies a sink that forwards streamed output
   (`text_delta`/`tool_call`/`turn_end`, from `crates/flux-flow/src/agent_sink.rs`) back over its
   protocol: a webhook collects-then-replies, Slack posts to the thread, cron logs.

## The run seam (event → agent run), concretely

```
adapter event ──▶ Inbound ──▶ Host.route ──▶ Target.run(ev, sink)
                                              │  EngineTarget:
                                              │    sid = sessions.entry(ev.conversation)
                                              │             .or_insert_with(|| events.create_session(model))
                                              │    engine.run_turn(&sid, &ev.text, sink).await
                                              └─▶ Outbound ──▶ adapter sink forwards reply
```

Session binding is the crux: a stable `conversation` id (Slack thread ts, a webhook correlation
header, a cron job name) maps to a persistent `EventStore` session so repeated events on the same
conversation **append to one conversation log** (and resume `await` flows); a fresh conversation gets a
fresh session. The host **serializes runs per `conversation`** (one in-flight turn each — a second event
on the same thread queues behind the first) and runs different conversations concurrently; the shared
`EventStore`/`FlowStore` (SQLite) is the concurrency floor, so distinct sessions are independent. In v1
the `conversation → session` map is **in-memory** (a restart starts threads fresh); a durable
conversation→session index is a follow-up (and pairs naturally with D-02's tagged log).

## Adapters

### Schedule (first slice) — full cron
- **Dep:** the `cron` crate (`Schedule::from_str` + `.upcoming(Utc)`) + `chrono` for the clock; the
  adapter drives a `tokio::time::sleep_until(next)` loop and `ctx.run` on each tick. (Alternative
  considered: `tokio-cron-scheduler` — rejected to avoid a second scheduler competing with our `Host`.)
- **Kinds:** a `startup` one-shot + N cron entries (`schedule = "0 9 * * *"`). Timezone defaults to UTC;
  a per-entry `tz` is a follow-up.
- **Event:** `conversation = job name`, `text = the job's prompt`, `payload = {at, name}`. Fire-and-
  forget (no reply); the sink logs the run.
- **Caveat (flagged, matches fluxplane):** scheduled state is **in-memory only** — a missed tick during
  downtime is not replayed; durable scheduling is deferred.

### Webhook (slice 2)
- **Dep:** axum 0.7 (already in the workspace; reuse flux-server's patterns). A `POST /<channel>` →
  `Inbound`; the handler awaits `Target.run` and returns the `Outbound` as the HTTP response
  (request/response). Optional HMAC signature verification + bearer token (mirror flux-server's `token`).
- **Conversation:** from a configurable header/body field, else one session per request.
- **Long runs:** an agent turn can outlast a client's HTTP timeout. Default is the synchronous reply
  (fine for short tools); a per-channel `async = true` returns `202 Accepted` immediately and runs
  fire-and-forget (like cron), with results delivered out-of-band (log/sink). No streaming/SSE in v1.

### Slack (slice 3, feature-gated)
- **Dep:** a Slack SDK (e.g. `slack-morphism`) socket-mode, behind `--features slack` so it never bloats
  the default build. Subscribe to mentions/DMs/thread replies; `conversation = thread ts` (multi-turn);
  post the `Outbound` back to the thread; access policy (allow/deny users+channels, default trust) as
  per-channel settings (fluxplane's model).

## Host, lifecycle, concurrency
- `Host::run()` spawns each channel as a tokio task and awaits the first of: a `tokio::signal::ctrl_c`,
  an external shutdown, or a fatal channel error (the fluxplane daemon / flux-app `App::run` pattern).
- Per-`conversation` serialization + cross-conversation concurrency as described in the run seam above.
- Graceful shutdown cancels the shared `CancellationToken`; channels stop their loops and in-flight
  turns drain via `FlowEngine::run_turn_cancellable`, which flux already supports.

## Config + CLI
- A channels config file (TOML) declares the target agent (an `AgentSpec`/`.flux`, or a Program for
  `ProgramTarget`) and a `[[channel]]` array (`kind` = `schedule|webhook|slack` + kind-specific settings
  + the prompt/journey to run).
- New CLI subcommand **`flux channels run <config>`** (explicit-subcommand convention), reusing
  `build_agent`/serve wiring from `crates/flux-cli/src/main.rs`.

## Layering & deps (must-dos for the lint)
- Add `flux-channels` to `crates/flux-codegate/src/lib.rs` `layer()` as **L6**, and to root `Cargo.toml`
  `members` + `[workspace.dependencies]` (path-only, **non-published** — it's a surface, not in the
  16-crate publish closure).
- Deps: `flux-flow`, `flux-events`, `flux-agent`, `flux-runtime`, `flux-system`, optional `flux-app`
  (ProgramTarget); `tokio`, `async-trait`, `serde`; `axum` (webhook); `cron` + `chrono` (schedule);
  `slack-morphism` (slack, feature-gated). Confirm no inner→outer edge (all of these are ≤ L6).

## Testing (hermetic — no provider, no network)
- **Routing/serialization:** a `MockTarget` (implements `Target`, records each `Inbound`) + a test
  channel that submits synthetic events — assert session reuse per `conversation`, per-conversation
  serialization, and concurrent distinct conversations, with **no** provider.
- **Schedule adapter:** drive it with a fast cron (`* * * * * *`, every second) or an injected clock seam
  and assert it submits one `Inbound` per tick with `payload {at,name}`; assert `startup` fires once.
- **Webhook adapter:** `tower`/axum test client `POST`s a body, assert it becomes an `Inbound` and the
  `Outbound` is returned (sync) / `202` (async).
- **End-to-end:** a `MockProvider` (the pattern already in `flux-sdk`/`flux-flow` tests) behind a real
  `EngineTarget` to prove `run_turn` is reached and a session is created — one test, gates the first slice.

## Reuse, don't reimplement
- `FlowEngine::run_turn` + `EventStore` sessions (the run + persistence seam) — not a new loop or store.
- `AgentSink` (`flux-flow`) for output streaming; flux-server's axum/SSE patterns for the webhook.
- flux-app's `App::deliver` + bus for `ProgramTarget` (declarative multi-journey reuse).
- D-01's parameterized-flow seam: a channel can run a stored, settings-seeded flow once it lands.

## Non-goals (v1) / named follow-ups
- Durable scheduling / a persistent job queue / missed-tick replay (in-memory only for v1).
- Durable `conversation → session` index (in-memory v1; a durable index pairs with D-02).
- **Per-event trust/policy** — v1 runs every event under the target engine's fixed policy; varying the
  approver per caller needs a per-run policy seam on `run_turn` (follow-up).
- Per-channel targets — v1 is one target, many channels; per-channel `target` is a later config knob.
- Distributed coordination / exactly-once delivery; webhook streaming/SSE.
- Full `ask` reply-parking (a journey parked awaiting a human reply) — defer.
- Multi-tenant event tagging — that's [D-02](../stories/D-02-tenant-event-substrate.md); this composes
  with it for per-account triggered-run history.

## Implementation references (the seams to build on)

| Seam | Symbol | Location |
|------|--------|----------|
| Run one agent turn | `FlowEngine::run_turn(session_id, user_input, sink)` | `crates/flux-flow/src/engine.rs:132` |
| Cancellable turn (shutdown drain) | `FlowEngine::run_turn_cancellable(.., cancel)` | `crates/flux-flow/src/engine.rs:146` |
| Engine assembly from a spec | `AgentSpec::into_engine` / `assemble` | `crates/flux-agent/src/lib.rs:117` |
| Deterministic flow run | `flux_flow::runtime::execute_flow(..)` | `crates/flux-flow/src/runtime.rs` |
| Output streaming contract | `AgentSink` | `crates/flux-flow/src/agent_sink.rs:14` |
| Existing "event → run" reference | `serve(addr, agent, token)` + `webhook` handler | `crates/flux-server/src/lib.rs:39`, `:209` |
| Session create/reuse | `EventStore::create_session(model) -> "s_{n}"` | `crates/flux-events/src/store.rs:117` |
| Program reuse (ProgramTarget) | `App::deliver(label, payload)` → triggers/journeys | `crates/flux-app/src/app.rs:104` |
| Layer map (add `flux-channels` = L6) | `layer()` | `crates/flux-codegate/src/lib.rs` |
