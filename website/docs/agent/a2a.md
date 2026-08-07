---
title: Agent-to-agent (A2A)
description: "How flux serves and calls A2A v0.3 agents over JSON-RPC/HTTP, including CLI usage, server behavior, and security."
---

# Agent-to-agent (A2A)

<!-- Editors: the contributor deep-dive is docs/a2a.md in the repository. The two texts are
deliberately independent, not mirrors; keep both consistent with the shipped tree. -->

flux implements the JSON-RPC-over-HTTP binding of the
[A2A v0.3.0 specification](https://a2a-protocol.org/v0.3.0/specification/) in both directions. It
can expose a local flux agent to a compatible v0.3 client, and it can call a compatible v0.3 agent
from the CLI.

A2A is an agent protocol, not a model protocol. One request becomes one remote task, and the remote
agent runs its own loop with its own tools. flux handles the wire, continuity, streaming, and
security checks around that task.

:::info Version boundary
The wire formats and method names on this page are A2A v0.3.0. flux does not currently implement
A2A v1.0, gRPC, HTTP+JSON, or automatic cross-version negotiation. A2A v1.0 changed message parts,
roles, task states, agent cards, and other wire details; see the official
[v0.3-to-v1.0 migration and status page](https://a2a-protocol.org/latest/whats-new-v1/).
:::

- **Server** — `flux app run --serve` exposes a flux agent to clients that use the compatible v0.3
  JSON-RPC methods and message shapes documented below.
- **Client** — `flux a2a <URL>` connects to a v0.3 JSON-RPC agent and drives it from the CLI.

Both directions use `message/send` and `message/stream` (Server-Sent Events), with v0.3 message
parts discriminated by `kind`.

## Client — `flux a2a <URL>`

```bash
# Interactive REPL against a remote agent
flux a2a http://127.0.0.1:3000

# One-shot: a prompt on the command line
flux a2a http://127.0.0.1:3000 "What files are in the workspace?"

# One-shot: piped stdin
echo "List the top-level Rust crates." | flux a2a http://127.0.0.1:3000

# A token-gated endpoint (or set FLUX_A2A_TOKEN)
flux a2a https://agent.example.com --token mysecret
```

On connect, the client fetches the agent card (`/.well-known/agent-card.json`) to learn the agent's
name and whether it streams, then:

- Streams the reply live when the agent advertises `capabilities.streaming` (`message/stream`),
  otherwise blocks on `message/send`.
- In the REPL, **Ctrl-C** stops waiting for the current turn, **Ctrl-D** exits, and `/card` prints
  the remote agent card. When flux is the server, dropping its SSE connection also cancels the
  in-flight flux turn; another server may handle disconnects differently.

The `<URL>` may be a base origin (`http://host:port` targets `<origin>/a2a`) or a full JSON-RPC
endpoint URL; the client adopts the endpoint advertised by the card when present.

## Client — `flux tui --attach <URL|NAME>`

The REPL above is one client; the TUI is the other. `flux tui --attach` points the full chat surface
— panes, approval sheet, transcript, interrupt — at an agent that lives on another machine.

```bash
# Attach to a served agent by URL; the bearer token comes from $FLUX_A2A_TOKEN
flux tui --attach https://agent.internal:8787

# …from a differently-named variable
flux tui --attach https://agent.internal:8787 --attach-token-env DEPLOY_AGENT_TOKEN

# …by a named binding declared in .flux/config.toml
flux tui --attach cluster-agent

# Continue an existing remote conversation instead of starting a new one
flux tui --attach cluster-agent --attach-context release-train
```

A named binding is an `[[endpoint.static]]` entry declaring `protocol = "a2a"`:

```toml
[[endpoint.static]]
id = "cluster-agent"
url = "https://agent.internal:8787"
protocol = "a2a"
credential_ref = "env/DEPLOY_AGENT_TOKEN"   # a location, never a value
```

The bearer credential is always a **reference**: `--attach-token-env` names an environment
variable, and a binding's `credential_ref` names a location. There is no flag that takes the token
itself, and a URL carrying `user:pass@` is refused, so a production credential never reaches a shell
history, a process listing or a CI log.

:::danger `--attach` is not `--remote` or `--host`
They are opposite postures and flux refuses them together.

| | `--remote <url>` / `--host <name>` | `--attach <url\|name>` |
|---|---|---|
| what runs on your machine | the agent — planning, model calls, approvals | a viewer |
| what runs on the far side | the guarded effects only | **the whole agent** |
| who approves an effect | **you, here** | the remote's posture (see below) |
| where the session is stored | your local event store | **the remote's event store** |
:::

### What the attached surface can and cannot do

On connect, the TUI probes the served agent and prints one line per affordance. Anything the far
side does not implement is shown **disabled with its reason** rather than left inert:

- **Live streaming** — on when the agent card declares `capabilities.streaming`; otherwise turns
  arrive whole via `message/send`, and the pane says so.
- **Interrupt (Ctrl-C)** — delivered as `tasks/cancel`, which stops the remote turn. Against an
  agent that does not implement it (flux's reduced *embeddable* dispatch does not), Ctrl-C reports
  that the remote turn **is still running** instead of pretending it stopped.
- **Reattach replay** — read from the remote's `tasks/get` history.
- **Approvals** — see below.
- **Tool calls and results** — *not carried by the A2A wire at all.* `message/stream` emits text and
  lifecycle status only, so an attached tool pane stays empty. The surface states this rather than
  leaving you to infer it from an agent that appears to do nothing between paragraphs.

### Approvals from an attached TUI

If the served agent runs the remote-approval posture (`flux app run --serve --remote-approval`),
parked effects are raised in the TUI's **ordinary** approval sheet and answered with the same
`y` / `a` / `n` / `d` keys. The decision echoes the request's `fingerprint`, so it is bound to the
effect you were shown.

Four postures, each reported as itself — "nothing is parked right now" and "nobody is ever asked"
never look the same:

| what the remote answers | what the TUI shows |
|---|---|
| `200` | answerable here — **with the caveat that answers are attributed to the deployment's shared operator token, not to you** |
| `501` | never raised — the agent runs a headless approver, constrained by policy, sandbox and budgets |
| `401` / `403` | not answerable here — this credential is not admitted to the approval routes |
| anything else | unknown, with the transport's own words |

Per-principal approval authorization does not exist yet: the server refuses to combine principal
authentication with one deployment-wide approval queue, because that would let one tenant answer
another's effects. Until that lands, more than one human supervising one attached agent share a
single operator identity.

### Which session artifacts live on which machine

This is the part that surprises people, so it is stated plainly.

| artifact | attached (`--attach`) | ordinary `flux tui` |
|---|---|---|
| conversation history | **the remote's event store** | your `~/.flux/events.db` |
| the plan, dispatches and evidence for a turn | **the remote's** | yours |
| token usage and cost accounting | **the remote's** (`GET /usage` there) | yours (`flux usage`) |
| approvals you answered | **the remote's** queue | your local approver |
| the transcript in your terminal | in memory, for as long as the TUI is open | a rendering of your local session |
| composer input history (what *you* typed) | local | local |

Consequently **an attached conversation never appears in `flux sessions` and can never be
`flux replay`ed on your machine.** Nothing is written locally, so there is nothing to list, and a
local row that replayed into nothing would be worse than no row. To inspect an attached agent's
work, run `flux sessions` / `flux replay` **on the host serving it**, or read the conversation over
the wire with `tasks/get`.

For the same reason, slash commands that act on the local engine or the local store — `/model`,
`/compact`, `/new`, `/clear`, `/evidence`, `/sessions`, `/fork`, `/insights` — are refused by name
while attached, rather than silently applying to an idle local engine that is producing none of the
output you can see.

### Known gaps

- A `contextId` cannot be resolved to its remote task without running a turn, so a freshly started
  process that attaches to an existing conversation replays its history only after you send
  something. Within a session — including across a dropped stream — reattach replays immediately.
- `GET /sessions/{id}` returns identity only (`id`, `model`, `created_at_ms`), not history.

## Server — `flux app run --serve`

```bash
# Loopback only (no token required)
flux app run --serve 127.0.0.1:3000 --yes

# Non-loopback bind — a bearer token is required
FLUX_SERVER_TOKEN=mysecret flux app run --serve 0.0.0.0:3000 --yes

# Serve the sole agent declared in a program
flux app run support.flux --serve 127.0.0.1:3000 --yes
```

Without a program file, `--serve` exposes the built-in coding agent. With a program file it exposes
the program's sole declared agent; multi-agent programs should declare an `a2a` channel with an
explicit `agent`. On startup flux prints the agent-card and endpoint URLs.

### Routes

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/.well-known/agent-card.json` | exempt | A2A discovery card |
| `GET` | `/health` | exempt | Liveness probe |
| `POST` | `/a2a` | per mode | JSON-RPC 2.0 dispatcher (`message/send`, `message/stream`) |

The discovery card is always public so external agents can find flux without a token. Every other
route requires authentication per the configured mode (see [Security](#security)).

These are the A2A routes only. The same server also exposes a session REST API, an SSE stream, a
webhook trigger and two usage endpoints — see the [HTTP API reference](./http-api.md).

### Serving many agents from one server

One server can serve **N agents keyed by path** — `GET /:agent_id/.well-known/agent-card.json` and
`POST /:agent_id/a2a` — each with the same session machinery (TTL, `contextId` continuity, SSE).
Each agent's card advertises its own `/:agent_id/a2a` endpoint, agents are resolved per request
(a fixed set, or a dynamic per-tenant resolver keyed on the authenticated principal), and an unknown
agent id is an indistinguishable `404`. Agents are isolated by construction — each has its own
engine and event store. Embedders reach this via `flux_server::router_multi`.

### `message/send`

Runs one flux turn and returns the resulting `Task`. The A2A spec default is **non-blocking**:
without `configuration.blocking: true` the call returns a `submitted` task immediately and runs the
turn in the background. The example below sets `blocking: true` for the synchronous response shown.

```bash
curl -s http://localhost:3000/a2a \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer mysecret' \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "message/send",
    "params": {
      "message": {
        "kind": "message",
        "messageId": "m-1",
        "role": "user",
        "parts": [{"kind": "text", "text": "What files are in the workspace?"}]
      },
      "configuration": { "blocking": true }
    }
  }'
```

The `result` is a `Task` whose `status.message` holds the agent's reply.

### `message/stream` — streaming

Same request shape with `"method": "message/stream"` and an `Accept: text/event-stream` header. flux
streams `status-update` events as SSE: working events carry incremental text deltas, and the final
event (`"final": true`) carries the terminal state. Closing the connection mid-stream cancels the
in-flight turn cleanly between plan rounds.

### Calling flux from another v0.3 client

1. Start flux with a token: `FLUX_SERVER_TOKEN=mytoken flux app run --serve 0.0.0.0:3000 --yes`.
2. Fetch `http://<your-host>:3000/.well-known/agent-card.json` and use its `url` as the JSON-RPC
   endpoint.
3. Send the bearer token in `Authorization: Bearer <token>`.
4. Use the v0.3 `message/stream` method for SSE, or `message/send` with
   `configuration.blocking: true` for a synchronous response.

## Security

Three authentication modes:

- **Open** — no auth; the server refuses a non-loopback bind in this mode (auto-approve + open
  listener is remote code execution).
- **Shared secret** — set `FLUX_SERVER_TOKEN`; every request presents `Authorization: Bearer <token>`
  (constant-time compared). The whole server is one auth realm. On a non-loopback bind also set
  `[server] external_url` so the public card advertises a trusted endpoint rather than a
  `Host`-header-derived one.
- **Per-request principal auth** — set `[server] introspect_url` in `.flux/config.toml`. Every
  request's bearer is resolved to a principal via RFC 7662 token introspection (with TTL + negative
  caching); sessions are tagged with and scoped to the caller's tenant realm, cross-tenant access
  returns an indistinguishable `404`, and each turn runs under the caller's identity. `[server]
  external_url` is required (it is what the public card advertises). A program's `a2a` channel takes
  the same knobs.

Whenever auth is enabled the discovery card **declares** its bearer scheme (`securitySchemes` /
`security`), and in principal mode the card's `url` comes from `external_url`, never the request
`Host` header. The card and `/health` are the only auth-exempt routes, and the exemption is
structural (registered outside the middleware), not a path-string comparison, so encoding tricks
cannot bypass it.

**Stateful sessions.** A request whose `contextId` matches a live A2A session continues it
(multi-turn memory); a request without one gets a fresh session per task. Sessions are pruned by a
TTL (`[server] a2a_session_ttl_secs`, default 1h). In principal mode continuity is realm-keyed — the
same `contextId` under two tenants is two isolated conversations (`contextId` is a grouping key, not
a security boundary).

## Related docs

- [A2A protocol support](./a2a-conformance.md) — method, task-state, and error-code support.
- [CLI](./cli.md) — the local command surface.
- [Server authentication & tenancy](../security/server-auth.md) — inbound auth modes and realm isolation.
