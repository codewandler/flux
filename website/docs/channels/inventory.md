---
title: Channel inventory and capabilities
description: "Every shipped channel kind, with its settings, payload, reply path, auth model, and limits."
---

# Channel inventory and capabilities

The stock `flux` binary ships the channel kinds listed below. This page is the reference for what each
one can actually do. For the model behind them — how a channel wakes a program and how concurrent
deliveries are isolated and bounded — read [what a channel is](./overview.md) first.

An unknown `kind` is a **load error**, not a warning: a program naming a channel flux does not
recognize refuses to start.

## At a glance

| kind | aliases | initiated by | reply path | needs a public URL | auth | build |
|---|---|---|---|---|---|---|
| [`cli`](#cli) | — | the operator, at a prompt | stdout | no | local process | always |
| [`schedule`](#schedule) | `cron` | the clock | none — result discarded | no | n/a | always |
| [`webhook`](#webhook) | `http` | any HTTP caller | the HTTP response | yes, if remote | optional bearer (**required** off-loopback) | always |
| [`connector`](#connector) | — | a vendor webhook | `202`; no automatic vendor reply | yes, if remote | manifest verification posture + optional bearer (**required** off-loopback for the currently servable posture) | always |
| [`a2a`](#a2a) | — | an agent or API client | response + SSE stream | yes, if remote | bearer, or per-request principal (RFC 7662) | always |
| [`slack`](#slack) | — | a Slack user | posted into the thread | **no** — outbound socket | Slack bot + app tokens | default feature |
| [`room`](#room) | — | any occupant of the room | said back into the room | **no** — outbound WebSocket to the room server | none (`mock`), or SASL + optional room password (`xmpp`) | always |

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
  schedule "0 9 * * *"  # 5-field crontab
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

**Payload:** the parsed JSON POST body. A request that is not valid JSON is rejected before a
delivery starts.

**Reply:** synchronously, the journey results as JSON —
`{ "runs": [ { "journey": …, "result": …, "steps": … } ] }`. With `async: true` you get `202` and no
result.

## `connector`

A manifest-driven webhook listener. The channel selects a connector manifest and one named
`[[channels]]` binding; that binding defines event discrimination, payload mapping, verification
posture, and optional reply metadata without adding vendor-specific adapter code to flux.

```flux
channel widget_events
  kind "connector"
  connector "widget"
  service "hooks"
  binding "hooks"
  addr "127.0.0.1:8790"
  path "/widget"
```

- `connector` — the connector id declared by the manifest (required).
- `binding` — the exact `[[channels]].name` to serve (required).
- `service` — selects a named service and its `<connector>-<service>.connector.toml` manifest.
- `manifest` — optional explicit manifest path instead of the default under
  `~/.flux/connectors/`.
- `addr` — the listener address (required for the currently supported webhook transport).
- `path` — the POST path; defaults to `/`.
- `credentials` — maps credential names from the manifest to deployment secrets.
- `token` — optional static bearer token; required on a non-loopback address for the currently
  servable `verification.kind = "none"` posture.

**Payload:** a JSON object built from the binding's dotted body-path map, plus `delivery_id` when the
binding selects one. Missing or `null` fields are omitted. A discriminator can produce
`<channel>.<event>` labels; an undeclared event is acknowledged with `204` and does not trigger a
run.

**Reply:** accepted events receive `202` immediately. flux validates reply metadata and requires its
named operation to be registered, but it does not yet invoke that operation automatically or turn a
journey result into a vendor reply.

Only webhook-transport bindings with explicit `verification.kind = "none"` can currently start.
HMAC bindings are validated and then refused because raw-body signature verification is not
implemented. See [Connector channels](./connector.md) for manifest placement, routing, authentication,
and the full limitation checklist.

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

## `room`

A **many-party meeting room** — the only channel kind where flux is one participant among several
rather than the other end of a 1:1 exchange. It joins the room, hears everything said in it, and
delivers each message as a turn that names **who** said it.

```flux
channel standup
  kind "room"
  backend "xmpp"
  url "wss://example.org/xmpp-websocket"
  room "standup@conference.example.org"
  domain "example.org"
  nick "flux"
```

- `backend` — which room implementation to join with. An unrecognized backend is a load error.
  - **`xmpp`** — any standards-compliant MUC: prosody, ejabberd, or a hosted Jitsi tenant. Speaks
    XMPP over a WebSocket (RFC 7395), so it needs **no browser and no vendor SDK**.
  - **`mock`** — the in-process one (the same role the `mock` model provider plays: no network, no
    vendor, fully scriptable).
- `room` — the room address as the **server** spells it. Take it from the server rather than assembling
  it: some hosts lowercase the room in its address while other identifiers keep the original case. Once
  joined, flux uses the address the server reported and ignores the case you wrote here.
- `nick` — the name flux joins under. Defaults to `flux`. The server may hand back a different one on
  a collision, and flux follows it.
- `address_rule` — when the agent should treat a turn as aimed at it. **Accepted but not yet enforced:**
  today every inbound message produces a turn, so a room with two people talking to each other will
  wake the program on every line.

Settings for `backend "xmpp"`:

- `url` — the WebSocket endpoint, `wss://…` (required). A room address says *which* room, never
  *where* to connect.
- `domain` — the XMPP domain to open the stream to. Defaults to the endpoint's host, which is right
  when the server and the conference component share a host and wrong when they do not (a room on
  `conference.example.org` usually lives on the server `example.org`).
- `user` / `password` — SASL `PLAIN` credentials. **Omit both to join anonymously**, which is what
  hosted Jitsi tenants expect (they authorize on the endpoint URL instead). Write the password as
  `password secret "KEY"`.
- `muc_password` — the room's own password, if it has one. Also a `secret` reference.
- `allow_private_net` — permit an endpoint that resolves to a private or loopback address, e.g. a
  prosody on your LAN. **Off by default:** flux's egress guard refuses internal addresses unless you
  say so.

**Payload:** `{ "room", "text", "speaker", "nick", "name" }`. `speaker` is the occupant's stable,
room-scoped address and `nick` their display name — two occupants can share a nick, so key anything
per-person on `speaker`.

**Reply:** non-empty run results are said back into the room, publicly for a public message and
privately for a private one. Our own echoed messages never become turns.

**No media.** Presence and text only — no audio and no screenshare. A delivery that fails (including
an op the approver denied) is logged and the room keeps running: people are still in it, and one
message going wrong is not a reason to walk out. If the *connection* dies mid-meeting, the room ends
and is logged, but the rest of the program's channels keep serving. A room that could never be joined
at all — wrong endpoint, refused credential — is a startup failure, because a silently absent agent is
worse than a loud stop.

:::warning A room is untrusted multi-party input
Anyone who can reach the room can type into it, and on many hosts anyone holding the link can join with
no account at all. **Being in a room grants an occupant no authority over flux:** a room-sourced turn
goes through exactly the same approval envelope as a prompt you typed yourself, so an operation that
needs approval is denied unless it is approved.
:::

## Adjacent surfaces

These are channels in spirit but are not `channel` kinds, and are documented separately:

- **[Realtime voice](../agent/realtime.md)** — a full-duplex voice session (experimental, SDK-level):
  a voice model is the acoustic front end while flux owns each turn.
- **[Endpoints](../agent/endpoints.md)** and **[Fleet](../agent/fleet.md)** — how agents find and
  address each other, including outbound A2A.

## Known limits

Stated plainly, because each one changes how you would deploy a channel:

1. **No webhook signature verification.** The generic `webhook` kind checks only an optional static
   bearer token. Connector manifests can describe HMAC verification, but flux refuses those bindings
   at startup until it can verify a signature over the raw request body. Do not edit a signed
   binding's manifest to say `none`: that would turn an authenticated vendor surface into an
   unauthenticated one.
2. **Concurrency has a bound and shared state.** Deliveries run concurrently, with isolated event
   cascades and nesting budgets, up to 64 at once by default. Set
   `FLUX_MAX_INFLIGHT_DELIVERIES` to a positive integer to change the process default. Additional
   deliveries wait rather than being dropped or rejected. Agent sessions and other app state remain
   shared, so events for the same conversation can interleave.
3. **Trigger matching is exact.** `on "<label>"` is an exact label match — normally the channel name,
   or `<channel>.<event>` for a connector binding with a discriminator. There is no globbing or
   prefix match.
4. **Headless means no approver.** Anything that would prompt for approval cannot be confirmed in a
   channel-driven run. Set an explicit `permissions` ceiling; do not rely on `--yes`, which can only
   auto-approve *within* the ceiling and never widens it.
5. **A room agent answers everything.** `room` ships presence and text against real MUC servers, but
   `address_rule` is carried and not yet enforced — every line said in the room becomes a turn,
   including two people talking to each other. Put a room agent in a busy room only if you mean to pay
   for every sentence in it.
6. **Rooms carry no media.** Audio and screenshare need a WebRTC endpoint and are not implemented; the
   `room` kind is text and presence only.
