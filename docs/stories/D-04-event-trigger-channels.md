---
id: D-04
title: Event-trigger channels — cron/timed, webhook, Slack (background agents)
pillar: Agent
status: backlog
priority:
theme: downstream-managed-agents
design: docs/designs/event-trigger-channels.md
---

# Event-trigger channels — cron/timed, webhook, Slack (background agents)

## Goal
Give flux a real **event-trigger channel** abstraction in a dedicated **`flux-channels` (L6) crate**: a
long-running host where a *channel* is an entrypoint that **wakes an agent on an external event** — a cron
schedule, an inbound webhook, or a Slack message — and routes the event into a run through the safety
envelope. **Epic** — ship the abstraction (`Channel` trait + normalized envelope + `Target` seam +
`Host`) + the schedule adapter first; webhook and Slack follow as slices. See the
[design](../designs/event-trigger-channels.md).

**Decisions:** a **dedicated crate** (keeps axum/Slack/cron deps out of flux-app's always-loaded tree),
and **full cron expressions** in the first schedule slice (the `cron` crate, not just `every: "5m"`).

## Why (managed-agents)
Today an managed-agents agent only responds over voice (RTVBP) or A2A request/response. The product wants
**background agents that wake on events** — scheduled checks, inbound webhooks, Slack mentions. This is
the channel breadth behind managed-agents' M5 and the "agents that aren't just request/response" direction.

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
- [ ] **Slice 1 (crate + trait + Target + Host + schedule):** the `flux-channels` crate with a `Channel`
      trait, the normalized `Inbound`/`Outbound` envelope, a `Target` seam (`EngineTarget` over
      `FlowEngine::run_turn`, session-bound by `conversation`), a `Host` running channels concurrently,
      and a **schedule** adapter taking **full cron** (`"0 9 * * *"`) + `startup`. Failing-first tests:
      (a) a `MockTarget` proves per-`conversation` session reuse + serialization with no provider; (b) a
      fast cron (`* * * * * *`) submits one `Inbound` per tick; (c) a `MockProvider` `EngineTarget` proves
      `run_turn` is reached and a session created.
- [ ] **Slice 2 (webhook):** axum `POST /<channel>` → `Inbound`; sync reply with `Outbound`, optional
      `async = true` → `202`; optional bearer/HMAC.
- [ ] **Slice 3 (Slack, feature-gated):** socket-mode mentions/DMs/threads; `conversation = thread ts`;
      post `Outbound` back; access policy in per-channel settings.
- [ ] New CLI subcommand `flux channels run <config.toml>`.
- [ ] `flux-channels` added to the `flux-codegate` `layer()` map as L6 + root `Cargo.toml` members; full
      gate green (`cargo build/test/clippy/fmt`, `cargo test -p flux-codegate`).
- [ ] The in-memory-only caveats (scheduled work, `conversation → session` map) and the per-event-trust
      follow-up are documented (see the design's Non-goals).

## Progress
- Backlog (epic). Design written: [`docs/designs/event-trigger-channels.md`](../designs/event-trigger-channels.md).
  Decomposes into the three acceptance slices above.

## Notes
- The run seam is `FlowEngine::run_turn` (`crates/flux-flow/src/engine.rs:132`); the existing
  "event → run" reference is flux-server's `serve`/`webhook` (`crates/flux-server/src/lib.rs:39`,`:209`).
- `ProgramTarget` (reuse flux-app's `App::deliver` + bus for declarative multi-journey Programs) is
  specced but **deferred** — the `Target` trait leaves the door open without building it in slice 1.
- Pairs with **D-01** (the host runs a parameterized flow) and **D-02** (triggered runs persist to the
  tagged event log; also the home for the durable `conversation → session` index).
- Serves the managed-agents channel-breadth direction (alongside its RTVBP + A2A channels) and the fluxplane
  use case directly.
