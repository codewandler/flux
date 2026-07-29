---
title: HTTP API
description: "The session REST API, its SSE stream, the webhook trigger, and the usage endpoints exposed by flux app run --serve."
---

# HTTP API

`flux app run --serve <addr>` exposes an agent over HTTP. Three surfaces share the one router:

- **Sessions** — create a session, run turns against it over REST or SSE, read its usage.
- **A2A** — the JSON-RPC agent-to-agent protocol, documented in [A2A](./a2a.md).
- **Webhook** — a one-shot trigger that mints a session and runs a single turn.

```bash
# the built-in coding agent
flux app run --serve 127.0.0.1:3000

# a program's declared agent
flux app run support.flux --serve 127.0.0.1:3000 --yes
```

Every turn served here runs the same agent loop, through the same
[safety envelope](./safety.md), as a local `flux run`. Serving an agent does not widen what it may
do — a request cannot approve its own actions, so a server deployment is normally paired with an
explicit permission policy in [`.flux/config.toml`](../reference/config.md).

## Routes

| Method | Path | Auth | Purpose |
|---|---|---|---|
| `GET` | `/health` | exempt | Liveness probe; returns `ok`. |
| `GET` | `/.well-known/agent-card.json` | exempt | A2A discovery card. |
| `GET` | `/.well-known/agent.json` | exempt | Alias of the above. |
| `POST` | `/sessions` | per mode | Create a session. |
| `GET` | `/sessions/{id}` | per mode | Session metadata. |
| `POST` | `/sessions/{id}/messages` | per mode | Run one turn, buffered. |
| `GET` | `/sessions/{id}/stream` | per mode | Run one turn, streamed as SSE. |
| `GET` | `/sessions/{id}/usage` | per mode | Token tiers and cost for one session. |
| `GET` | `/usage` | per mode | Token tiers and cost across sessions. |
| `POST` | `/webhook` | per mode | Mint a session and run one turn. |
| `POST` | `/a2a` | per mode | A2A JSON-RPC dispatcher. |

The **multi-agent mount** serves N agents from one server, keyed by path:

| Method | Path | Auth | Purpose |
|---|---|---|---|
| `GET` | `/health` | exempt | Liveness probe. |
| `GET` | `/{agent_id}/.well-known/agent-card.json` | exempt | That agent's discovery card. |
| `GET` | `/{agent_id}/.well-known/agent.json` | exempt | Alias of the above. |
| `POST` | `/{agent_id}/a2a` | per mode | That agent's A2A dispatcher. |

It exposes A2A only — there is no per-agent session REST subtree. Each agent owns its own engine and
event store, and an unknown `agent_id` is an indistinguishable `404`. See
[Serving many agents](./a2a.md#serving-many-agents-from-one-server).

`/health` and the discovery card are **structurally** exempt: they are registered outside the auth
middleware rather than allow-listed by path string inside it, so no percent-encoding or double-slash
trick can smuggle another route past the check. Everything else authenticates per the configured
mode — see [Server authentication & tenancy](../security/server-auth.md).

## Sessions

### Create a session

```bash
curl -X POST http://127.0.0.1:3000/sessions
```

```json
{"id": "s_1", "model": "claude-sonnet-5"}
```

### Read session metadata

```bash
curl http://127.0.0.1:3000/sessions/s_1
```

```json
{"id": "s_1", "model": "claude-sonnet-5", "created_at_ms": 1753776000000}
```

An unknown id is a `404`.

### Run a turn

`POST /sessions/{id}/messages` takes `{"input": "…"}` and returns when the whole turn is done:

```bash
curl -X POST http://127.0.0.1:3000/sessions/s_1/messages \
  -H 'content-type: application/json' \
  -d '{"input": "which tests are failing?"}'
```

```json
{
  "text": "Two tests fail in flux-lang…",
  "tool_calls": ["cargo_test", "read"],
  "usage": {
    "input": 4120,
    "output": 380,
    "cache_creation": 0,
    "cache_read": 3900,
    "reasoning": 0
  }
}
```

`tool_calls` lists the operations the turn ran, in order. `usage` reports **every** token tier, not
just input and output — a caller pricing its own spend, or measuring real context-window occupancy,
needs the cache and reasoning tiers. It is `null` when the provider reported no usage.

### Stream a turn

`GET /sessions/{id}/stream?input=…` runs the same turn and streams it as
[Server-Sent Events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events):

```bash
curl -N 'http://127.0.0.1:3000/sessions/s_1/stream?input=which%20tests%20are%20failing%3F'
```

```
event: text
data: Two tests

event: tool
data: cargo_test

event: text
data:  fail in flux-lang…

event: done
data: end
```

| Event | Data | Meaning |
|---|---|---|
| `text` | the token text | An assistant text delta. Concatenate in order. |
| `tool` | the operation name | A tool call started. |
| `error` | the error message | The turn failed; `done` still follows. |
| `done` | `end` | The turn is over. Always last. |

The stream is keep-alive'd, so an idle connection during a long tool call stays open. Note that
`text` carries the delta only — the SSE path does not emit a usage summary, so read
`GET /sessions/{id}/usage` afterwards if you need one.

## Webhook

`POST /webhook` is the integration trigger: it mints a **fresh** session, runs one turn, and returns
both. Use it for a CI hook or a bridged chat message — anything that arrives as a single external
event with no conversation to continue.

```bash
curl -X POST http://127.0.0.1:3000/webhook \
  -H 'content-type: application/json' \
  -d '{"input": "release 0.34.0 failed on CI, triage it"}'
```

```json
{
  "session_id": "s_2",
  "text": "The failure is in the plugin-pack signing step…",
  "tool_calls": ["read", "grep"]
}
```

The returned `session_id` addresses the session for follow-up turns through the routes above.

## Usage and cost

Both usage endpoints return the same per-model row shape:

```bash
curl http://127.0.0.1:3000/sessions/s_1/usage
```

```json
{
  "session_id": "s_1",
  "models": [
    {
      "model": "claude-sonnet-5",
      "calls": 3,
      "usage": {"input": 4120, "output": 380, "cache_creation": 0, "cache_read": 3900, "reasoning": 0},
      "cost_usd": 0.0192,
      "subscription": false
    }
  ]
}
```

`cost_usd` is `null` for a model with no pricing entry; `subscription` marks spend covered by a
subscription rather than metered billing. `GET /usage` returns the same `models` array without the
`session_id` key, rolled up across sessions. See [Cost accounting](./cost.md) for where the figures
come from per provider.

## Limits and errors

| Status | When |
|---|---|
| `401` | Authentication required or failed for the configured mode. |
| `404` | Unknown session, or an unknown `agent_id` on the multi-agent mount. |
| `408` | The request exceeded the response-production timeout. |
| `413` | The request body exceeded the body cap. |
| `500` | The turn failed. |

The body cap and timeout apply to the whole surface and are set at router build time, overridable
per deployment with `FLUX_SERVER_MAX_BODY_BYTES` and `FLUX_SERVER_REQUEST_TIMEOUT_SECS` (positive
integers; `0` or an unparseable value falls back to the default rather than disabling the bound, so
the daemon is never accidentally left unlimited).

The timeout bounds how long a handler may take to **produce** a response, not how long a response
body may stream. That distinction is what lets `POST /sessions/{id}/messages` be bounded — it holds
the connection for a whole turn — while `GET /sessions/{id}/stream` returns its SSE response
promptly and then streams for the life of the turn without being severed.

## Tenancy

In principal mode every session is tagged with the caller's realm, and the whole `/sessions/{id}/*`
subtree is realm-guarded as one structural layer rather than per handler. Session ids are guessable
(`s_1`, `s_2`, …), so this matters: guarding reads while leaving the write route open would be a
cross-tenant read *and* write. `GET /usage` is realm-scoped in the same mode — one tenant cannot
read another's spend.

Authentication always runs before the realm check, so an unauthenticated caller gets a `401` and
never an existence signal.

## Related docs

- [A2A](./a2a.md) — the JSON-RPC protocol served on `/a2a`, and the multi-agent mount.
- [Server authentication & tenancy](../security/server-auth.md) — Open, shared-secret and OIDC modes.
- [Cost accounting](./cost.md) — what the usage figures mean per provider.
- [Configuration](../reference/config.md) — the `[server]` section and permission policy.
- [SDK: sessions](../sdk/sessions.md) — the same session model from embedded Rust.
