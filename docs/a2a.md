# A2A — Agent-to-Agent protocol

flux speaks the [A2A protocol](https://a2a-protocol.org/) in **both directions**:

- **Server** — `flux app run --serve` exposes a flux agent over A2A, so any A2A client (Claude Code,
  other agents, custom scripts) can call flux as a first-class agent. A `.flux` program can also declare an
  `a2a` channel directly.
- **Client** — `flux a2a <URL>` connects out to any spec-conformant A2A agent and drives it from
  the CLI exactly like a local agent (interactive REPL or one-shot).

Both directions share one wire definition (the `flux-a2a` crate) and speak the current spec:
`message/send` (blocking) and `message/stream` (SSE), with message parts keyed by `kind`. The
**server-side** protocol logic (JSON-RPC dispatch, the agent-card builder, message/event shaping) is
itself reusable — `flux_a2a::server`, an axum-free module over a small `A2aTurn` seam — so `flux-server`
and downstream HTTP surfaces can mount the same A2A endpoint without re-implementing the protocol.

---

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

On connect, the client fetches the agent card (`/.well-known/agent-card.json`, falling back to
`/.well-known/agent.json`) to learn the agent's name and whether it streams, then:

- **One user turn = one remote A2A task.** A2A is an *agent* protocol, not a model protocol — the
  remote runs its own loop (model + tools); flux just speaks the protocol and renders the reply.
- Streams the reply live when the agent advertises `capabilities.streaming` (via `message/stream`),
  otherwise blocks on `message/send` (and polls `tasks/get` if the agent answers with a still-running
  task).
- In the REPL, **Ctrl-C** interrupts a turn (dropping the SSE connection, which cancels the remote
  turn), **Ctrl-D** exits, and `/card` prints the remote agent card.

The `<URL>` may be a base origin (`http://host:port` → the client targets `<origin>/a2a`) or a full
JSON-RPC endpoint URL. The client adopts the endpoint advertised by the agent card when present.

> **Continuity (A-48).** The client mints one A2A `contextId` per chat session and sends it with
> every message. A flux server (and any stateful remote) keys **one session per `contextId`**, so
> multi-turn conversations keep memory across `message/send` calls — slot-filling presets work
> over plain A2A text. A remote that ignores the id simply behaves per-turn.

---

## Server — `flux app run --serve`

```bash
# Listen on all interfaces, auto-approve all tool calls
flux app run --serve 0.0.0.0:3000 --yes

# Loopback only (no auth required)
flux app run --serve 127.0.0.1:3000 --yes

# Shared-secret auth (required for a non-loopback bind)
FLUX_SERVER_TOKEN=mysecret flux app run --serve 0.0.0.0:3000 --yes

# Per-request principal auth: configure `[server] introspect_url` in .flux/config.toml (below)
flux app run --serve 0.0.0.0:3000 --yes

# Serve the sole agent declared in a program
flux app run support.flux --serve 127.0.0.1:3000 --yes
```

Without a program file, `--serve` exposes the built-in coding agent. With a program file, it exposes the
program's sole declared agent; multi-agent programs should declare an `a2a` channel with an explicit `agent`.

A non-loopback bind requires authentication — a shared secret (`FLUX_SERVER_TOKEN`) or per-request
principal auth (`[server] introspect_url`). See **Security notes** for all three modes.

On startup flux prints:

```
flux server listening on http://0.0.0.0:3000
  A2A agent card:  http://0.0.0.0:3000/.well-known/agent-card.json
  A2A endpoint:    http://0.0.0.0:3000/a2a  (message/send, message/stream)
```

### Routes

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/.well-known/agent-card.json` | exempt | A2A discovery card |
| `GET` | `/.well-known/agent.json` | exempt | discovery card (legacy alias) |
| `POST` | `/a2a` | per mode | JSON-RPC 2.0 dispatcher |

The discovery card is always public so external agents can find flux without a token. Every other
route requires authentication per the configured mode (see **Security notes**).

### Multi-agent mount (D-63)

One server can serve **N agents keyed by path**, each with the same session machinery (TTL,
`contextId` continuity, SSE). Routes gain an `/:agent_id` prefix:

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/:agent_id/.well-known/agent-card.json` | that agent's discovery card |
| `POST` | `/:agent_id/a2a` | that agent's JSON-RPC dispatcher |

Each agent's card advertises its own `/:agent_id/a2a` endpoint. Agents are resolved per request by
an `AgentResolver` (a fixed set, or a dynamic per-tenant resolver keyed on the authenticated
principal); an unknown agent id is a `404` indistinguishable from any other unknown resource. Auth
is one outer layer (all three modes below), so a resolver sees the authenticated principal but never
verifies tokens itself. Agents are isolated by construction — each owns its own engine and event
store. (Embedders reach this via `flux_server::router_multi`; the per-agent REST `/sessions` surface
is single-agent only.)

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

Response — the `result` is a `Task`:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "kind": "task",
    "id": "s_42",
    "contextId": "s_42",
    "status": {
      "state": "completed",
      "message": {
        "kind": "message",
        "messageId": "…",
        "role": "agent",
        "parts": [{"kind": "text", "text": "…"}]
      },
      "timestamp": "2026-01-15T12:00:00Z"
    },
    "artifacts": [],
    "history": []
  }
}
```

The reply message always carries the answer as a `text` part; an agent surface may append spec
`data` parts beside it (typed payloads, e.g. UI blocks a chat client renders natively) — clients
that only read text can ignore them.

### `message/stream` — streaming (SSE)

Runs one flux turn and streams `TaskStatusUpdate` events as Server-Sent Events. Each SSE frame is a
full JSON-RPC response whose `result` is a status-update event. Working events carry the incremental
text delta; the final event (`"final": true`) carries the terminal state and **no message** — the
streamed deltas are authoritative.

```bash
curl -sN http://localhost:3000/a2a \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer mysecret' \
  -H 'Accept: text/event-stream' \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "message/stream",
    "params": {
      "message": {
        "kind": "message",
        "messageId": "m-2",
        "role": "user",
        "parts": [{"kind": "text", "text": "List the top-level Rust crates."}]
      }
    }
  }'
```

Event stream:

```
data: {"jsonrpc":"2.0","id":1,"result":{"kind":"status-update","taskId":"s_43","contextId":"s_43","status":{"state":"working","timestamp":"…"},"final":false}}

data: {"jsonrpc":"2.0","id":1,"result":{"kind":"status-update","taskId":"s_43","contextId":"s_43","status":{"state":"working","message":{"kind":"message","role":"agent","parts":[{"kind":"text","text":"The "}]}},"final":false}}

... (one event per token delta) ...

data: {"jsonrpc":"2.0","id":1,"result":{"kind":"status-update","taskId":"s_43","contextId":"s_43","status":{"state":"completed","timestamp":"…"},"final":true}}
```

If you close the connection mid-stream, the in-flight agent turn is cancelled cleanly between plan
rounds.

### Discovery card

`GET /.well-known/agent-card.json` returns:

```json
{
  "name": "flux",
  "description": "flux — a precise, autonomous coding agent …",
  "url": "http://<host>/a2a",
  "version": "<semver>",
  "capabilities": { "streaming": true, "pushNotifications": false },
  "defaultInputModes": ["text/plain"],
  "defaultOutputModes": ["text/plain"],
  "skills": [{
    "id": "coding",
    "name": "Coding Agent",
    "description": "Read, write, edit, search, and execute code tasks in a workspace.",
    "inputModes": ["text/plain"],
    "outputModes": ["text/plain"]
  }]
}
```

The `url` is derived from the request's `Host` and `X-Forwarded-Proto` headers, so it is correct
whether accessed directly or through a reverse proxy.

### Calling flux from Claude Code

1. Start flux with a token: `FLUX_SERVER_TOKEN=mytoken flux app run --serve 0.0.0.0:3000 --yes`.
2. In Claude Code, add flux as an A2A agent pointing at `http://<your-host>:3000`. The card at
   `/.well-known/agent-card.json` is fetched automatically to learn flux's capabilities.
3. Pass the bearer token as the connection credential.
4. Use `message/stream` for a streaming experience, or `message/send` for a single blocking call.

---

## Security notes

- **Three auth modes (D-69).** Open (no auth; loopback binds only), shared secret
  (`FLUX_SERVER_TOKEN` — one `Authorization: Bearer <token>` for the whole deployment, compared in
  constant time), and **per-request principal auth**: configure `[server] introspect_url` in
  `.flux/config.toml` and every request's bearer is resolved to a principal via RFC 7662 token
  introspection (with TTL + negative caching). In principal mode `[server] external_url` is
  required, the optional knobs are `introspect_client_id` + `introspect_client_secret_env` (the
  NAME of the env var holding the client secret), `introspect_account_claim` (literal key first,
  dot-path on miss), `introspect_roles_claim`, `introspect_require_account`, and
  `introspect_allow_http` (explicit opt-in for trusted-network `http://` endpoints). Example:

  ```toml
  # .flux/config.toml  →  flux app run --serve 0.0.0.0:3000 --yes
  [server]
  introspect_url = "https://idp.example.com/oauth2/introspect"
  external_url   = "https://agents.example.com"     # required; advertised on the card
  introspect_account_claim = "org_id"               # → the tenancy realm
  introspect_roles_claim   = "roles"
  # optional client auth (client_secret_basic); the secret is the NAMED env var, never inline:
  introspect_client_id         = "flux-server"
  introspect_client_secret_env = "FLUX_INTROSPECT_SECRET"
  ```

  A program's **`a2a` channel** takes the same knobs (parity with `--serve`), except the client
  secret is a host-resolved `secret "ENV"` reference like every other channel secret:

  ```
  channel a2a
    addr "0.0.0.0:3000"
    introspect_url "https://idp.example.com/oauth2/introspect"
    external_url "https://agents.example.com"
    introspect_account_claim "org_id"
    introspect_client_id "flux-server"
    introspect_secret secret "FLUX_INTROSPECT_SECRET"
  ```
- A non-loopback bind **requires** authentication (a token or principal mode). The server refuses
  to start otherwise — an open listener with `--yes` is effectively remote code execution.
- In shared-secret mode on a non-loopback bind, set `[server] external_url` too: the public card
  otherwise advertises a `Host`-header-derived endpoint, so a poisoned `Host` on a card fetch would
  phish the shared secret to an attacker host. (Principal mode already requires `external_url`.)
- The discovery card and `/health` are the only routes exempt from auth. This is structural
  (registered outside the middleware layer), not a path-string comparison, so percent-encoding
  tricks cannot bypass it. Whenever auth is enabled the card **declares** the bearer scheme
  (`securitySchemes` + `security` — the A2A spec has clients authenticate with a scheme the card
  declares), and in principal mode the card's `url` derives from the configured `external_url`,
  never the request's `Host` header (the public card tells clients where to send bearer tokens).
- Auth failures are constant-shape: 401 carries the byte-constant
  `WWW-Authenticate: Bearer error="invalid_token"` challenge and body for every cause (no oracle);
  an unreachable introspection backend is a constant 503 (fail closed, backend detail logged
  server-side only). Requests with more than one `Authorization` header are rejected.
- **Per-principal isolation (principal mode).** Every session is tagged with the caller's realm —
  `acct:<account>` from the introspected account, or `user:<principal>` when the deployment maps no
  account claim (the two namespaces are kept disjoint so an account value can never collide with a
  principal-derived realm) — and
  every `/sessions/:id/*` route (including the `POST …/messages` write path) answers a
  cross-realm probe with a 404 byte-identical to a nonexistent id (A2A §13.1: never reveal the
  existence of resources the caller isn't authorized for). `GET /usage` returns only the caller's
  realm. Legacy untagged sessions (minted before principal mode was enabled) are unreachable in
  principal mode. Each turn runs the safety envelope under the request principal's
  `(Caller, Trust)` — including sub-agents spawned within the turn — never the service identity.
- **Stateful mode (A-48): one session per `contextId`.** A request whose `contextId` matches a
  live A2A session continues it (multi-turn memory via the engine's conversation projection); a
  request without a `contextId` gets a fresh session per task, as before. Sessions minted by the
  A2A surface are pruned lazily: every request first sweeps A2A sessions whose last activity is
  older than `[server] a2a_session_ttl_secs` in `.flux/config.toml` (default `3600` = 1 hour;
  `0` = never prune) — the sweep runs *before* the continuity lookup, so an expired conversation
  is never resumed (the same `contextId` then starts a fresh one). Only A2A-minted sessions are
  eligible — CLI/TUI sessions are never swept. In principal mode, continuity is **realm-keyed**:
  the same `contextId` under two realms is two isolated conversations (`contextId` is a grouping
  key, not a security boundary — per the A2A spec). In the shared-secret mode the whole server
  remains one realm, as before.

## Protocol support

Which A2A methods, AgentCard fields, task states, and error codes flux supports today — and which are
partial or not yet — is tracked in the living support matrix:
[a2a-conformance.md](a2a-conformance.md). The gap-closing work is sequenced in the
[`a2a-conformance`](designs/a2a-conformance.md) epic.
