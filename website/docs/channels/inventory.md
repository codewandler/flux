---
title: Channel inventory and capabilities
description: "Every channel kind flux ships — cli, schedule, webhook, a2a, slack — with its settings, payload, reply path, auth model, and limits."
---

# Channel inventory and capabilities

Five channel kinds ship in the stock `flux` binary. This page is the reference for what each one can
actually do. For the model behind them — how a channel wakes a program and why deliveries are ordered
— read [what a channel is](./overview.md) first.

An unknown `kind` is a **load error**, not a warning: a program naming a channel flux does not
recognize refuses to start.

## At a glance

| kind | aliases | initiated by | reply path | needs a public URL | auth | build |
|---|---|---|---|---|---|---|
| [`cli`](#cli) | — | the operator, at a prompt | stdout | no | local process | always |
| [`schedule`](#schedule) | `cron` | the clock | none — result discarded | no | n/a | always |
| [`webhook`](#webhook) | `http` | any HTTP caller | the HTTP response | yes, if remote | optional bearer (**required** off-loopback) | always |
| [`a2a`](#a2a) | — | an agent or API client | response + SSE stream | yes, if remote | bearer, or per-request principal (RFC 7662) | always |
| [`slack`](#slack) | — | a Slack user | posted into the thread | **no** — outbound socket | Slack bot + app tokens | default feature |

Every kind's `settings` may carry `secret "ENV_VAR"` references instead of literals; the host resolves
them once at load, before any adapter reads its settings.

## `cli`

The interactive stdin loop, served by the host itself rather than as a background task — declaring it
tells the host to read prompts on standard input.

```flux
channel cli
```

It is also the default target of outbound `send`: `send({ "channel": "cli", "message": … })`.

## `schedule`

Self-clocked. Fires under its own name on a cron expression, or once at boot.

```flux
channel nightly
  schedule "0 9 * * *"          # 5-field crontab
```

- `schedule` — a **5-field** crontab (`"0 9 * * *"`), or a **6/7-field seconds-first** expression
  (`"* * * * * *"`).
- `on` — a lifecycle hook; only `"startup"` is supported, firing once at boot under the channel's name.
- Exactly one of `schedule` or `on` must be set.

**Payload:** `{ "at": "<RFC 3339 timestamp>", "name": "<channel name>" }`.

**Reply:** none. Nobody is waiting, so the run's result is discarded and a delivery error is logged
rather than fatal — a failed tick does not take the app down.

## `webhook`

An HTTP listener that turns a POST into a turn. This is the generic inbound door.

```flux
channel hook
  addr "127.0.0.1:8790"
  path "/hook"
  token secret "HOOK_TOKEN"
```

- `addr` — the address to bind (required).
- `path` — the POST path; defaults to `/`.
- `async` — when true, reply `202 Accepted` immediately and run the delivery fire-and-forget.
- `token` — optional bearer token. **Required for a non-loopback `addr`**, because a headless listener
  has no interactive approver; an open public listener would be an unauthenticated way to spend your
  model budget and touch your tools.

**Payload:** the POST body, verbatim.

**Reply:** synchronously, the journey results as JSON —
`{ "runs": [ { "journey": …, "result": …, "steps": … } ] }`. With `async: true` you get `202` and no
result.

## `a2a`

Exposes a declared agent over the HTTP/A2A API — sessions, SSE streaming, the A2A protocol, and the
agent card. This is the surface the standalone `flux serve` command used to provide.

```flux
channel api
  addr "127.0.0.1:8787"
  agent assistant
  token secret "API_TOKEN"
```

- `addr` — the address to bind (required).
- `agent` — which declared agent to serve; optional when the program declares exactly one.
- `token` — bearer token, **required for a non-loopback `addr`** unless `introspect_url` is set.

For multi-tenant deployments it also supports **per-request principal auth**: setting
`introspect_url` (RFC 7662 token introspection) resolves every request's bearer to a principal and
scopes sessions per realm. That mode requires `external_url` — the public agent card must advertise a
configured URL rather than trusting the request's `Host` header — and accepts
`introspect_client_id` / `introspect_secret`, `introspect_account_claim`,
`introspect_roles_claim`, `introspect_require_account`, and `introspect_allow_http`.

Unlike the other kinds, an `a2a` channel is constructed by the host rather than from its declaration
alone, because it needs the live agent's engine.

See [HTTP API](../agent/http-api.md), [A2A](../agent/a2a.md), and
[A2A conformance](../agent/a2a-conformance.md).

## `slack`

A Slack bot over **Socket Mode** — an outbound WebSocket, so there is **no public URL and no inbound
webhook to host**. It listens for mentions and posts each run's answer back into the originating
thread.

```flux
channel slack
  bot_token secret "SLACK_BOT_TOKEN"
  app_token secret "SLACK_APP_TOKEN"
  allow_users ["U123ABC"]
  allow_channels ["C456DEF"]
```

- `bot_token` — bot OAuth token (`xoxb-…`).
- `app_token` — app-level token for Socket Mode (`xapp-…`).
- `allow_users` / `allow_channels` — allow-lists. **Empty means everyone**; non-empty restricts to
  those ids. Both gates must pass.

**Payload:** `{ "text", "user", "channel", "thread", "conversation" }` — so a trigger's agent sees
`$text`, and `conversation` gives per-thread memory continuity.

**Reply:** non-empty run results are posted into the thread.

Compiled into the stock binary; only a `--no-default-features` build omits it, and such a build fails
loudly on a `slack` channel rather than silently ignoring it. Setup walkthrough:
[Slack channel setup](../agent/slack-channel.md).

## Adjacent surfaces

These are channels in spirit but are not `channel` kinds, and are documented separately:

- **[Realtime voice](../agent/realtime.md)** — a full-duplex voice session (experimental, SDK-level):
  a voice model is the acoustic front end while flux owns each turn.
- **[Endpoints](../agent/endpoints.md)** and **[Fleet](../agent/fleet.md)** — how agents find and
  address each other, including outbound A2A.

## Known limits

Stated plainly, because each one changes how you would deploy a channel:

1. **No webhook signature verification.** The `webhook` kind checks an optional *static bearer token*.
   It does **not** verify vendor webhook signatures — the HMAC-over-raw-body schemes that GitHub,
   Stripe, Slack's Events API and Zendesk each implement differently. So a vendor that signs but
   cannot send a custom `Authorization` header has no authenticated path in today's `webhook` channel.
   Typed, verified inbound events are being designed in
   [flux-connectors](https://github.com/codewandler/flux-connectors) as the reverse direction of the
   connector spec.
2. **One delivery at a time.** `AppDeliverer` serializes deliveries to keep concurrent bus cascades
   from double-processing each other. A busy channel is an ordered queue of one until per-delivery bus
   isolation lands.
3. **Trigger matching is exact.** `on "<name>"` is an exact label match against the channel's name —
   no globbing, no prefixes.
4. **Headless means no approver.** Anything that would prompt for approval cannot be confirmed in a
   channel-driven run. Set an explicit `permissions` ceiling; do not rely on `--yes`, which can only
   auto-approve *within* the ceiling and never widens it.
5. **Multi-party rooms are not a channel yet.** Meeting rooms — many humans and agents co-present,
   with presence, text, audio and screenshare — are designed but unimplemented (epic D-203 on the
   internal board). Nothing in the shipped kinds carries multi-participant presence.
