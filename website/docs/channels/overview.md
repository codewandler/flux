---
title: What a channel is
description: "The conceptual and technical model behind flux channels: an event source that wakes a program, routed by triggers through the ordinary safety envelope."
---

# What a channel is

A **channel** is how a flux app is *reached*. Everything else in flux runs because you asked it to —
you type a prompt, you run a flow. A channel inverts that: it is a long-running event source that
**wakes** the app when something happens outside it, whether that is a cron tick, an HTTP request, a
Slack mention, or another agent calling in.

This is the difference between an agent you *use* and an agent that is simply *running*. Background
agents, scheduled workflows, chat assistants, and agent-to-agent services are all the same mechanism
with a different event source underneath.

## Conceptually: an event source plus a route

A channel does not know what it wakes. It only knows how to notice things and how to announce them.

A channel **fires a bus event under its own name**, and a [`trigger`](../agent/programs.md) routes
that name to something that does work:

```flux
channel slack  # the event source, named `slack`
  bot_token secret "SLACK_BOT_TOKEN"
  app_token secret "SLACK_APP_TOKEN"

trigger on_message  # the route
  on "slack"  # matches the channel's name, exactly
  agent assistant  # what runs: an agent (model-driven) or `run <journey>` (fixed flow)
```

That separation is the whole design. The channel is transport; the trigger is policy; the agent or
journey is behaviour. Swapping Slack for a cron schedule changes one declaration and nothing else —
and one program can carry several channels at once, each with its own route.

Because a channel is an ordinary declaration on a [program](../agent/programs.md), there is no
separate channels config file, no daemon to configure, and no CLI verb per channel. You declare the
channel in the `.flux` file and run it:

```bash
flux run app.flux        # or: flux app run app.flux
```

## Technically: `Channel`, `Deliverer`, and the returned runs

Two small traits carry the entire mechanism, in the `flux-channels` crate:

- **`Channel`** — `name()` plus `start(deliverer, cancel)`. The adapter runs its own protocol loop
  (a cron timer, an HTTP listener, a WebSocket) until cancelled, and calls the deliverer once per
  external event.
- **`Deliverer`** — `deliver(label, payload) -> Vec<JourneyRun>`. This is the seam into the app, which
  is also why adapters are testable against a recording double instead of a live app.

The interesting part is the **return value**. `deliver` hands back the runs it caused, and what a
channel does with them is exactly what distinguishes the kinds:

- a synchronous **webhook** writes them into the HTTP response; an asynchronous one acknowledges
  the request first,
- **Slack** posts them back into the originating thread,
- **cron** discards them — nobody is waiting,
- a **connector** acknowledges an accepted event with `202` and does not put journey results in the
  HTTP response.

So "does this channel have a reply path, and where does the reply go?" is a per-kind property, not a
framework-wide one. The [inventory](./inventory.md) states it for each.

The event payload is seeded into the run's store, so a flow reads it with ordinary interpolation
(`{field}`), and an agent-bound trigger receives it in scope (for example `$text` for an incoming
message).

## Deliveries are concurrent, isolated, and bounded

Deliveries from different channels can run at the same time. The app keeps each delivery's event
cascade and nesting budget isolated, so a slow scheduled sweep does not make an unrelated webhook or
Slack message wait behind it.

Concurrency is bounded. An app admits **64 deliveries at once by default**; set the positive-integer
environment variable `FLUX_MAX_INFLIGHT_DELIVERIES` to choose another process-wide default. SDK
callers can instead use `App::with_max_inflight_deliveries`, and can inspect `App::delivery_load()` to
distinguish admitted work from deliveries waiting for a slot.

At the bound, a delivery **waits**. flux neither drops it nor rejects it, and the wait propagates back
to the task calling `deliver`. A synchronous webhook therefore takes longer to answer under pressure.
Adapters that acknowledge first — asynchronous webhooks and connector channels — may already have
returned `202` while their delivery task waits.

Isolation does not make all app state private. Agent sessions, parked asks, and the recorded-send log
are shared, so deliveries addressing the same conversation can interleave. There is also no
unbounded mode: if your own flows hold deliveries open while waiting on other deliveries, set the
limit above that fan-out width or the mutually dependent work can deadlock.

## Channels grant no authority

A channel is a way in, never a way *up*. An event arriving on a channel dispatches through the same
envelope as a keystroke in the CLI: **authorization → approval → guarded IO**. A channel cannot widen
a program's `permissions` ceiling, and neither can `--yes`.

Two consequences worth internalizing before you expose a channel:

- **Channel input is untrusted input.** Whoever can reach the channel can put text in front of your
  model. That is prompt-injection surface, and the program's `allow`/`deny` ceiling — not the
  channel — is what bounds the damage. See [Safety](../agent/safety.md).
- **A headless channel has no human to ask.** With no interactive approver, operations that would
  prompt cannot be confirmed by anyone. That is why the network-facing kinds require a bearer token
  on any non-loopback address, and why the app-level `permissions` ceiling matters more here than
  anywhere else.

Secrets in channel settings are always **references**, resolved by the host at load:
`bot_token secret "SLACK_BOT_TOKEN"` reads the environment variable at startup. Tokens never live in
the file, so a program is safe to commit.

## Two directions, and why it matters

Most of what a channel does is **inbound**: something outside calls flux and a turn happens. But the
same program also speaks **outbound**, and the two are worth keeping distinct in your head:

| | inbound (they call us) | outbound (we call them) |
|---|---|---|
| **Mechanism** | a `channel` + a `trigger` | an `op` — `send`, `http.request`, a plugin, a connector |
| **Who initiates** | the outside world | the flow or the model |
| **Example** | a Slack mention wakes the agent | `send({ "channel": "cli", … })` posts a message |

A complete integration usually needs both halves — a vendor's API to call, *and* a way for that
vendor to notify you. flux covers outbound richly (plugins, `http.request`, and
[flux-connectors](https://github.com/codewandler/flux-connectors) generating typed ops from vendor
specs). For inbound traffic, a generic `webhook` passes the parsed JSON body through under one label.
A [`connector`](./connector.md) instead loads a named manifest binding that selects event labels and
maps vendor fields into payload symbols. Both can check a static bearer token; neither currently
serves a vendor HMAC-signed webhook.

## Next

- **[Channel inventory and capabilities](./inventory.md)** — every kind that exists, what it can do,
  and what it cannot.
- **[Connector channels](./connector.md)** — install a manifest, select a binding, and understand the
  current verification and reply limits.
- **[Multi-agent programs](../agent/programs.md)** — the file format channels are declared in.
- **[Slack channel setup](../agent/slack-channel.md)** — a full worked example, tokens included.
