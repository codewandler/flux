---
title: Agent-to-agent (A2A)
---

# Agent-to-agent (A2A)

flux speaks the [A2A protocol](https://a2a-protocol.org/) in **both directions**. A2A is an *agent*
protocol, not a model protocol: one turn is one remote task, and the remote agent runs its own loop
(model plus tools). flux just speaks the wire and renders the reply.

- **Server** — `flux app run --serve` exposes a flux agent so any A2A client (Claude Code, other
  agents, custom scripts) can call flux as a first-class agent.
- **Client** — `flux a2a <URL>` connects out to any spec-conformant A2A agent and drives it from the
  CLI, exactly like a local agent.

Both directions use the current spec: `message/send` (blocking) and `message/stream` (Server-Sent
Events), with message parts keyed by `kind`.

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
- In the REPL, **Ctrl-C** interrupts a turn (dropping the SSE connection cancels the remote turn),
  **Ctrl-D** exits, and `/card` prints the remote agent card.

The `<URL>` may be a base origin (`http://host:port` targets `<origin>/a2a`) or a full JSON-RPC
endpoint URL; the client adopts the endpoint advertised by the card when present.

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
| `POST` | `/a2a` | per mode | JSON-RPC 2.0 dispatcher (`message/send`, `message/stream`) |

The discovery card is always public so external agents can find flux without a token. Every other
route requires authentication per the configured mode (see [Security](#security)).

### Serving many agents from one server

One server can serve **N agents keyed by path** — `GET /:agent_id/.well-known/agent-card.json` and
`POST /:agent_id/a2a` — each with the same session machinery (TTL, `contextId` continuity, SSE).
Each agent's card advertises its own `/:agent_id/a2a` endpoint, agents are resolved per request
(a fixed set, or a dynamic per-tenant resolver keyed on the authenticated principal), and an unknown
agent id is an indistinguishable `404`. Agents are isolated by construction — each has its own
engine and event store. Embedders reach this via `flux_server::router_multi`.

### `message/send` — synchronous

Runs one flux turn and returns the resulting `Task` when complete.

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

### Calling flux from Claude Code

1. Start flux with a token: `FLUX_SERVER_TOKEN=mytoken flux app run --serve 0.0.0.0:3000 --yes`.
2. In Claude Code, add flux as an A2A agent pointing at `http://<your-host>:3000`. The card at
   `/.well-known/agent-card.json` is fetched automatically to learn flux's capabilities.
3. Pass the bearer token as the connection credential.
4. Use `message/stream` for a streaming experience, or `message/send` for a single blocking call.

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

**Stateful sessions (A-48).** A request whose `contextId` matches a live A2A session continues it
(multi-turn memory); a request without one gets a fresh session per task. Sessions are pruned by a
TTL (`[server] a2a_session_ttl_secs`, default 1h). In principal mode continuity is realm-keyed — the
same `contextId` under two tenants is two isolated conversations (`contextId` is a grouping key, not
a security boundary).

## See also

- [A2A protocol support](./a2a-conformance.md) — which methods, card fields, task states, and error
  codes are supported / partial / not yet.
- [CLI](./cli.md) — the reference flux surface.
- [Concepts](../concepts.md) — plans, symbols, and the safety envelope every operation runs through.
- [flux on GitHub](https://github.com/codewandler/flux)
