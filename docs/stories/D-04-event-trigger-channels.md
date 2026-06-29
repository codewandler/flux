---
id: D-04
title: Event-trigger channels — cron/timed, webhook, Slack (background agents)
pillar: Agent
status: backlog
priority:
theme: downstream-managed-agents
---

# Event-trigger channels — cron/timed, webhook, Slack (background agents)

## Goal
Give flux a real **event-trigger channel** abstraction: a long-running host where a *channel* is an
entrypoint that **wakes an agent on an external event** — a cron/timed schedule, an inbound webhook, or a
Slack message — and routes the event into a flow run through the safety envelope. Generalises flux-app's
nascent in-process trigger/channel concept into external event sources. **Epic** — ship the abstraction +
schedule adapter first; webhook and Slack follow as slices.

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
- [ ] First slice (trait + host + schedule): a `flux-channels` abstraction — a `Channel` trait, a
      normalized event envelope, and a daemon host running channels concurrently — plus a **schedule**
      adapter (tokio interval or a cron-expr crate). Failing-first test: a registered schedule channel
      fires on its interval and drives one flow run through the envelope.
- [ ] Webhook adapter (HTTP listener → normalized event) — subsequent slice.
- [ ] Slack adapter (socket mode) — subsequent slice.
- [ ] Each adapter delivers a normalized event into a flow run; the in-memory-only persistence limit is
      documented, durable scheduling deferred.
- [ ] Full gate green; layering lint placement confirmed (a daemon host with HTTP/Slack deps sits at the
      right layer, not pulled below its dependencies).

## Progress
- Backlog (epic). Decomposes: (1) trait + host + schedule, (2) webhook, (3) Slack.

## Notes
- Reuse flux-app's trigger/event-bus where it fits rather than forking a second one.
- Pairs with **D-01** (the host invokes a parameterized flow) and **D-02** (triggered runs persist to the
  tagged event log).
- Serves the managed-agents channel-breadth direction (alongside its RTVBP + A2A channels) and the fluxplane
  use case directly.
