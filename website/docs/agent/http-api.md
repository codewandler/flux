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
| `GET` | `/approvals` | per mode | Effects parked awaiting a human decision. |
| `POST` | `/approvals/{id}` | per mode | Deliver one decision for one parked effect. |

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

`tool_calls` lists the operations the turn ran, in order. `usage` reports the five tiers shown above
— `input`, `output`, `cache_creation`, `cache_read`, `reasoning` — rather than just input and output,
because a caller pricing its own spend or measuring real context-window occupancy needs the cache and
reasoning figures. It is `null` when the provider reported no usage.

Three tiers flux tracks internally are **not** on the wire, here or on the usage endpoints: the
one-hour cache-write split, and audio input/output. They are all subsets of a tier that *is* reported,
so no tokens go missing from a total — but a caller re-deriving cost from these numbers alone will
price a 1h cache write as a five-minute one. The `cost_usd` the usage endpoints return is computed
server-side from the full breakdown and does account for it.

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
| `429` | The caller exceeded request rate or live work, or completed provider usage tripped a call/spend circuit breaker. `Retry-After` and `X-Flux-Limit` identify when and which dimension may be retried. |
| `413` | The request body exceeded the body cap. |
| `500` | The turn failed. |
| `503` | Principal mode only: the authentication backend was unreachable. Deliberately distinct from `401` — the token was never judged, so retrying later is the right response, not re-authenticating. |

The body cap and timeout apply to the whole surface and are set at router build time, overridable
per deployment with `FLUX_SERVER_MAX_BODY_BYTES` and `FLUX_SERVER_REQUEST_TIMEOUT_SECS` (positive
integers; `0` or an unparseable value falls back to the default rather than disabling the bound, so
the daemon is never accidentally left unlimited).

The timeout bounds how long a handler may take to **produce** a response, not how long a response
body may stream. That distinction is what lets `POST /sessions/{id}/messages` be bounded — it holds
the connection for a whole turn — while `GET /sessions/{id}/stream` returns its SSE response
promptly and then streams for the life of the turn without being severed. When a buffered REST,
webhook, or blocking A2A request reaches its deadline, Flux cancels the owning turn and waits for
durable turn and child-work finalization before returning `408`; a timeout cannot detach live work.

Streaming bodies have their own lifecycle bound: REST SSE uses a 256-event channel. A consumer
that disconnects or stops draining cancels the request-owned turn; the producer finalizes a valid
cancelled session instead of continuing approved effects or buffering token events without limit.

Request admission is keyed by authenticated principal in principal mode, by the configured shared
realm in shared-secret mode, and by one loopback-development bucket in open mode. It covers every
authenticated protected route, including session and usage reads; `/health` and discovery cards
remain exempt. A request is charged once at that boundary. The same key's separate live-work cap
covers REST turns, fresh-session webhooks, blocking/background A2A work, and long-lived SSE.

Provider call and priced-spend thresholds are **retrospective completed-usage circuit breakers**,
not exact prepaid caps. Flux charges each durable call fact after its exact owning turn/producer
finishes, including calls that reported zero tokens, then rejects new work once the threshold is
observed. Turns already admitted can finish and overshoot the threshold;
`max_inflight_per_principal` bounds how many can do so concurrently. Exact turn attribution keeps
overlapping requests for one session assigned to the right principal even when they finish or drop
in reverse order. The in-process principal/realm buckets are cardinality-bounded and stale buckets
are swept.

These controls are **per replica**. A multi-replica deployment must also enforce aggregate request,
concurrency, and spend policy at an authenticated reverse proxy or shared control plane, using the
same principal/tenant claim. Do not rely on client IP as the tenancy key behind a proxy. Limit
rejections emit a secret-free `flux_server_limit_rejections_total` log signal and never include a
bearer or shared-secret value.

In principal mode bearer introspection necessarily runs **before** the in-process request limiter:
the verified principal selects the bucket. Protect the listener and introspection dependency with
reverse-proxy/identity-provider arrival limits as well; Flux's per-principal limiter cannot shield
that pre-admission authentication call from raw request floods.

## Approvals

Only when the server was started with `--remote-approval` (see
[Topologies](../topologies.md#served-agent-thin-client)). Under any other approval posture both
routes answer `501` with a body saying so, rather than an empty list — "nothing is waiting" and
"nobody is ever asked" must not look alike.

`GET /approvals` returns everything the agent is currently blocked on, oldest first:

```json
{
  "approvals": [
    {
      "id": "ap_3f1c9a02b7d4e615_0",
      "fingerprint": "…",
      "tool": "write",
      "subjects": ["report.txt"],
      "summary": null,
      "destructive": false,
      "mutating": true,
      "intents": {
        "intents": [
          {
            "behavior": "filesystem_write",
            "target": { "type": "path", "path": "report.txt" },
            "role": "write_target",
            "certainty": "certain"
          }
        ]
      },
      "plan": null,
      "waiting_secs": 4
    }
  ],
  "timeout_secs": 120
}
```

`POST /approvals/{id}` delivers one decision:

```json
{ "fingerprint": "…", "decision": "deny", "reason": "not that path" }
```

`decision` is `allow` or `deny`; `reason` is optional and, on a denial, is passed to the model.
`fingerprint` is **required and must match** — echo the opaque value returned by `GET`; do not
reconstruct it. It canonically binds the complete effect, including structured intent targets and
exact plan requirements, to the decision you were shown.

| Status | Meaning |
|---|---|
| `200` | Recorded. The waiting effect proceeds or is refused accordingly. |
| `400` | `decision` was neither `allow` nor `deny`. Nothing was approved; the effect is still parked. |
| `404` | No such parked request — already answered (including a replay), timed out, or its turn ended. |
| `409` | The `fingerprint` names a different effect than this request. Nothing was approved, and the request stays parked. |
| `410` | The run waiting on it is gone. |
| `422` | The body is missing `fingerprint` or `decision`. |
| `501` | This server is not running the remote-approval posture. |

Three properties are load-bearing, and a client should be written expecting them:

- **An effect nobody answers is denied** after `timeout_secs`
  ([`FLUX_APPROVAL_TIMEOUT_SECS`](../reference/config.md), default 120). Silence is never an
  approval.
- **A decision is single-use.** Answering removes the request, so a captured decision cannot be
  replayed onto a later effect.
- **A decision is bound to its effect.** An `allow` you obtained for one request cannot be delivered
  against another, even an identical-looking one — that is what the `id` plus `fingerprint` pair is
  for.

⚠ Remote approval supports the single-operator server modes: a shared bearer token, or an open
loopback listener. Router construction refuses it with per-request principal authentication. One
deployment-wide queue in principal mode would otherwise let any authenticated principal list and
answer every other principal's effects despite their session realms being isolated; that topology
needs a separately authorized supervisor identity first. An unauthenticated non-loopback bind is
still refused. Treat the shared token—and the ability to POST here—as equivalent to the agent's own
authority.

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
