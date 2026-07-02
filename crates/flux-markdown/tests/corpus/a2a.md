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

> **Continuity.** Each turn is sent as an independent task, matching today's stateless `flux app run --serve`.
> The client carries the A2A `contextId`/`messageId`/`taskId` identifiers, so a *stateful* remote
> keeps conversation memory and server-side statefulness can be added later without client changes.

---

## Server — `flux app run --serve`

```bash
# Listen on all interfaces, auto-approve all tool calls
flux app run --serve 0.0.0.0:3000 --yes

# Loopback only (no token required)
flux app run --serve 127.0.0.1:3000 --yes

# With a bearer token (required for non-loopback)
FLUX_SERVER_TOKEN=mysecret flux app run --serve 0.0.0.0:3000 --yes

# Serve the sole agent declared in a program
flux app run support.flux --serve 127.0.0.1:3000 --yes
```

Without a program file, `--serve` exposes the built-in coding agent. With a program file, it exposes the
program's sole declared agent; multi-agent programs should declare an `a2a` channel with an explicit `agent`.

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
| `POST` | `/a2a` | Bearer token | JSON-RPC 2.0 dispatcher |

The discovery card is always public so external agents can find flux without a token. Every other
route requires `Authorization: Bearer <token>` when a token is configured.

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

- A non-loopback bind **requires** `FLUX_SERVER_TOKEN`. The server refuses to start otherwise — an
  open listener with `--yes` is effectively remote code execution.
- The discovery card and `/health` are the only routes exempt from auth. This is structural
  (registered outside the middleware layer), not a path-string comparison, so percent-encoding
  tricks cannot bypass it.
- Each A2A task creates a fresh session (stateless mode). Sessions are not currently pruned
  automatically — see the TODO in `a2a.rs` if you are running at high volume.
