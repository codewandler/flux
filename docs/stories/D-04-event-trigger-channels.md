---
id: D-04
title: Event-trigger channels — cron/timed, webhook, Slack (background agents)
pillar: Agent
status: done
theme: downstream-managed-services
design: docs/designs/event-trigger-channels.md
note: "new `flux-channels` L6 crate; channels declared in the Program + run by `flux app run` (each fires a bus event → trigger → journey via `App::deliver`); schedule/webhook/Slack adapters (see [CHANGELOG](../../CHANGELOG.md))"
---

# Event-trigger channels — cron/timed, webhook, Slack (background agents)

## Goal
Give flux real **event-trigger channels** — a *channel* is a long-running event source that **wakes an
agent on an external event** (a cron schedule, an inbound webhook, a Slack message). **Shipped** as a
dedicated **`flux-channels` (L6) crate** (keeps axum/cron/Slack deps out of flux-app's tree) whose
channels are **declared inside the `.flux` program** and run by the **app runner**: each channel fires a
bus event under its own name; a `trigger { on: "<channel name>", run: "<journey>" }` routes it to a
journey via `App::deliver`. See the [design](../designs/event-trigger-channels.md).

**Decisions:** channels are Program-declared (`ChannelDecl` + `TriggerDecl`), not a separate config/CLI;
the entry point is **`flux app run <program.flux>`** (with `flux run <app.flux>` as an alias); **full cron
expressions** (the `cron` crate). This replaced an earlier standalone-host / `flux channels run` / TOML /
single-agent-`EngineTarget` draft — the Program's bus/triggers/journeys already do the routing.

## Why (downstream managed services)
Today downstream agents commonly respond over voice (RTVBP) or A2A request/response. The product direction wants
**background agents that wake on events** — scheduled checks, inbound webhooks, Slack mentions. This is
the channel breadth behind "agents that aren't just request/response".

## flux gap
`flux-app` (L6) has only in-process channels (cli stdin/stdout) + `{on: startup/user_input}` triggers +
an internal event bus. There is no external-event channel abstraction and no daemon host that wakes
agents on cron/webhook/Slack.

## Prior art (copy the shape, not the code)
`~/projects/fluxplane` (Go) is a proven implementation:
- a `Channel` interface = `Name()` + `Start(ctx, client)` — a long-running task pumping normalized events
  to a client;
- a normalized `Inbound`/`Outbound` envelope (caller / trust / conversation / correlation id);
- a **daemon host** running channels concurrently and binding events to sessions by conversation id;
- adapters: schedule (`time.Ticker`, `every: 1m`, `startup`), webhook (HTTP/SSE), Slack (socket mode).
- Honest caveat to carry over: pending/scheduled work is **in-memory only** — no durable queue (fine for
  v1; flag it).

## Acceptance
- [x] **Core:** the `flux-channels` crate with a `Channel` trait, a `Deliverer` seam, the gate-serialized
      `AppDeliverer` over `App::deliver`, `serve(app, channels, run_stdin, cancel)`, and `build_channels`
      (kind → adapter; `cli` skipped; unknown = error).
- [x] **schedule** adapter: **full cron** (5-field crontab + 6/7-field seconds-first) + `on:"startup"`.
- [x] **webhook** adapter: axum `POST <path>` per channel → delivery; sync JSON reply / `async=true` →
      `202`; optional bearer token; non-loopback bind requires a token.
- [x] **slack** adapter (feature `slack`): socket-mode mentions/messages; thread as conversation; posts
      results back; `allow_users`/`allow_channels` policy.
- [x] **`flux app run <program.flux>`** subcommand (+ `flux run <app.flux>` routes through it).
- [x] `flux-channels` in the `flux-codegate` `layer()` map (L6) + root `Cargo.toml` members; full gate
      green (`cargo build/test/clippy/fmt`, `cargo test -p flux-codegate`, `--features slack`).
- [x] Hermetic tests: routing/serialization, fast-cron + startup, webhook sync/async/token, an e2e
      cron→real-App→journey run, and the Slack mapping/allow-list. Example `examples/channels-app.flux`.
- [x] The serialization rationale + the slack-morphism `<2.18` cap + follow-ups documented in the design.

## Progress
- **Done.** Implemented as the `flux-channels` L6 crate; design rewritten to the Program/app-runner model;
  10 hermetic tests + 3 feature-gated Slack unit tests pass; smoke-tested live via `flux app run`
  (startup + cron heartbeat to stdout, webhook POST returns the journey result).

## Notes
- Routing reuses flux-app: `App::deliver` (`crates/flux-app/src/app.rs:104`) → `run_triggers` (exact
  `on == label`) → `run_journey` → `execute_flow` (fresh per-run store). flux-app is unchanged.
- Deliveries are **serialized** (a gate Mutex) because `App::deliver` drains the broadcast bus's cascade
  events — concurrent deliveries would double-process via fan-out. Journeys are independent; cross-channel
  concurrency is a follow-up.
- Pairs with **D-01** (a journey can run a parameterized flow) and **D-02** (triggered runs persist to the
  tagged event log). Serves downstream channel-breadth needs and the fluxplane use case directly.
