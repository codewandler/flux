# Design: event-trigger channels (`flux-channels`)

**Status:** implemented (story [D-04](../stories/D-04-event-trigger-channels.md)) · **Layer:** L6 (new
crate `flux-channels`) · **Owner:** Timo

> The originally-spec'd agentic **`EngineTarget`** (route an event to an `AgentSpec` `run_turn` so the model
> drives RAG + tools, with per-conversation memory) was deferred in favour of the journey route below; it is
> now tracked as **[D-09](../stories/D-09-agentic-channel-target.md)** (agentic channel target — a new
> `Deliverer` alongside the journey route), driven by the downstream Slack-channel assistant.

## Why

flux agents are reached request/response — the CLI/REPL, the HTTP `webhook`/A2A endpoints, voice
downstream. There was no way to run an agent that is **idle and woken by an external event**: a nightly
cron summary, an inbound webhook, a Slack mention. This is the channel breadth behind the downstream
managed-agents product (which has only RTVBP + A2A channels) and the "background agents" use case directly.

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
adapters: schedule (cron+chrono) · webhook (axum) · slack (slack-morphism, feature `slack`)
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
true` replies `202 Accepted` and runs fire-and-forget. Optional bearer `token` (literal or
`secret:env/KEY`), compared in constant time. A **non-loopback `addr` requires a `token`** (the host
auto-approves tools, so an open listener is a remote-trigger surface — mirrors flux-server). HMAC and a
shared multi-channel server are follow-ups.

### slack (`kind = "slack"`, feature `slack`)
`slack-morphism` socket mode, behind `--features slack` so its dep tree stays out of the default build.
Subscribes to app-mentions and human messages (bot/subtype messages are skipped to avoid reply loops);
delivers `{ text, user, channel, thread, conversation }` under the channel name and posts the journeys'
joined result back to the thread. `allow_users` / `allow_channels` settings gate access; bot/app tokens
come via `secret:env/...`. Live validation needs a real Slack app — the hermetic tests cover the
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
- slack (feature-gated, in-module) — event→payload mapping (thread as conversation) + allow-list.

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
