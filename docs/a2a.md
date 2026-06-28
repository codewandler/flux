# A2A — Agent-to-Agent protocol

flux speaks the [A2A protocol](https://google.github.io/A2A/) over its HTTP server,
so any A2A-compatible client (Claude Code, other agents, custom scripts) can call
it as a first-class agent.

## Starting the server

```bash
# Listen on all interfaces, auto-approve all tool calls
flux serve 0.0.0.0:3000 --yes

# Loopback only (no token required)
flux serve 127.0.0.1:3000 --yes

# With a bearer token (required for non-loopback)
FLUX_SERVER_TOKEN=mysecret flux serve 0.0.0.0:3000 --yes
```

On startup flux prints:

```
flux server listening on http://0.0.0.0:3000
  A2A agent card:  http://0.0.0.0:3000/.well-known/agent.json
  A2A endpoint:    http://0.0.0.0:3000/a2a
```

## Routes

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/.well-known/agent.json` | exempt | A2A discovery card |
| `POST` | `/a2a` | Bearer token | JSON-RPC 2.0 dispatcher |

The discovery card is always public so external agents can find flux without
needing a token. Every other route requires `Authorization: Bearer <token>` when
a token is configured.

## A2A methods

### `tasks/send` — synchronous

Runs one flux turn and returns the result when complete.

```bash
curl -s http://localhost:3000/a2a \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer mysecret' \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tasks/send",
    "params": {
      "id": "task-1",
      "message": {
        "role": "user",
        "parts": [{"type": "text", "text": "What files are in the workspace?"}]
      }
    }
  }'
```

Response:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "id": "task-1",
    "status": {
      "state": "completed",
      "message": {
        "role": "agent",
        "parts": [{"type": "text", "text": "..."}]
      },
      "timestamp": "2024-01-15T12:00:00Z"
    },
    "final": true
  }
}
```

### `tasks/sendSubscribe` — streaming (SSE)

Runs one flux turn and streams `task_status_update` events as Server-Sent Events.
Each working event carries only the incremental text delta; the final `completed`
event carries the full reply.

```bash
curl -sN http://localhost:3000/a2a \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer mysecret' \
  -H 'Accept: text/event-stream' \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tasks/sendSubscribe",
    "params": {
      "id": "task-2",
      "message": {
        "role": "user",
        "parts": [{"type": "text", "text": "List the top-level Rust crates."}]
      }
    }
  }'
```

Event stream:

```
event: task_status_update
data: {"id":"task-2","status":{"state":"working","message":null,"timestamp":"..."},"final":false}

event: task_status_update
data: {"id":"task-2","status":{"state":"working","message":{"role":"agent","parts":[{"type":"text","text":"The "}]},"timestamp":"..."},"final":false}

... (one event per token delta) ...

event: task_status_update
data: {"id":"task-2","status":{"state":"completed","message":{"role":"agent","parts":[{"type":"text","text":"The workspace contains ..."}]},"timestamp":"..."},"final":true}
```

If you close the connection mid-stream, the in-flight agent turn is cancelled
cleanly between plan rounds.

## Calling flux from Claude Code

1. Start flux with `serve` and a token:

   ```bash
   FLUX_SERVER_TOKEN=mytoken flux serve 0.0.0.0:3000 --yes
   ```

2. In Claude Code, add flux as an MCP/A2A tool pointing at
   `http://<your-host>:3000`. Claude Code will fetch the discovery card from
   `/.well-known/agent.json` automatically to learn flux's capabilities.

3. Pass the bearer token as the credential for the connection.

4. Send tasks using `tasks/sendSubscribe` for a streaming experience, or
   `tasks/send` for a single blocking call.

## Discovery card

`GET /.well-known/agent.json` returns:

```json
{
  "name": "flux",
  "description": "flux — a precise, autonomous coding agent ...",
  "url": "http://<host>/a2a",
  "version": "<semver>",
  "capabilities": {
    "streaming": true,
    "pushNotifications": false,
    "stateTransitionHistory": false
  },
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

The `url` is derived from the request's `Host` and `X-Forwarded-Proto` headers,
so it is correct whether accessed directly or through a reverse proxy.

## Security notes

- A non-loopback bind **requires** `FLUX_SERVER_TOKEN`. The server refuses to
  start otherwise — an open listener with `--yes` is effectively remote code
  execution.
- The discovery card (`/.well-known/agent.json`) and `/health` are the only
  routes exempt from auth. This is structural (registered outside the
  middleware layer), not a path-string comparison, so percent-encoding tricks
  cannot bypass it.
- Each A2A task creates a fresh session (stateless mode). Sessions are not
  currently pruned automatically — see the TODO in `a2a.rs` if you are running
  at high volume.
