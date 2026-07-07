---
title: A2A protocol support
---

# A2A protocol support

What flux's [A2A protocol](https://a2a-protocol.org/) (v1.0) implementation supports today, what's
partial, and what isn't yet — so you know exactly what you can rely on when you point an A2A client at
flux or drive a remote agent with `flux a2a`. For how to use it, see [Agent-to-agent (A2A)](./a2a.md).

**Legend:** ✅ Supported · ⚠️ Partial · ❌ Not yet · 🚫 Non-goal

## The one thing to know

flux runs an A2A request as **one synchronous turn** and returns a `completed` `Task`. There is no
retained, addressable async task: `message/send` blocks until the turn finishes, `configuration.blocking`
is ignored, and the returned `Task` has no id you can look up later. Multi-turn continuity is by
`contextId` (the same `contextId` continues a conversation), not by task id. This is why the
task-management methods below (`tasks/get` server-side, `tasks/cancel`, `tasks/resubscribe`,
non-blocking send, push notifications) are Not-yet.

## Methods

| Method | Status | Notes |
|---|---|---|
| `message/send` | ✅ | Synchronous; returns a `completed` Task. |
| `message/stream` | ✅ | Server-Sent Events. Disconnecting cancels the remote turn. |
| `tasks/get` | ⚠️ | flux's **client** can call it against remote agents; flux's **server** does not implement it. |
| `tasks/cancel` | ❌ | Cancel a streaming turn by dropping the SSE connection instead. |
| `tasks/resubscribe` | ❌ | |
| `tasks/pushNotificationConfig/{set,get,list,delete}` | ❌ | |
| `agent/getAuthenticatedExtendedCard` | ❌ | |

## AgentCard

| Field | Status | Notes |
|---|---|---|
| `name`, `description`, `url`, `version` | ✅ | |
| `capabilities.streaming` | ✅ | |
| `capabilities.pushNotifications` | ✅ | Advertised `false` — flux does not send push notifications. |
| `defaultInputModes`, `defaultOutputModes` | ✅ | `text/plain`. |
| `skills` | ✅ | |
| `securitySchemes`, `security` | ✅ | Declared whenever the server enforces auth. |
| `protocolVersion` | ❌ | Planned. |
| `preferredTransport`, `interfaces` | ⚠️ | JSON-RPC is served at `url`; a dedicated interface entry is planned. |
| `provider`, `documentationUrl`, `iconUrl`, `supportsAuthenticatedExtendedCard` | ❌ | Planned (optional metadata). |
| `signatures` | 🚫 | Card signing is out of scope. |

## Tasks, messages & streaming

| Item | Status | Notes |
|---|---|---|
| `Task` / `TaskStatus`; states `working` / `completed` / `failed` | ✅ | |
| Task states `input-required` / `auth-required` / `canceled` / `submitted` | ❌/⚠️ | Not emitted under the synchronous model. |
| `Task.history`, `Task.artifacts` | ⚠️ | Present in the type; the server does not yet populate them. |
| `Message` (`messageId`, `role`, `parts`, `contextId`) | ✅ | `contextId` drives continuity. |
| `Message.taskId` / `referenceTaskIds` | ⚠️/❌ | Not used for task addressing yet. |
| Part: `text` | ✅ | |
| Part: `file` / `data` (inbound) | ⚠️ | Currently ignored on input — send text. |
| `TaskStatusUpdateEvent` (streaming) | ✅ | Incremental `working` deltas + a final event. |
| `TaskArtifactUpdateEvent` (streaming) | ⚠️ | Decoded by the client; not emitted by the server. |

## Transports & errors

| Item | Status | Notes |
|---|---|---|
| JSON-RPC 2.0 over HTTP | ✅ | The transport flux serves and speaks. |
| gRPC, HTTP+JSON/REST bindings | 🚫 | Non-goals. |
| Base JSON-RPC errors (`-32600`/`-32601`/`-32602`/`-32603`) | ✅ | |
| A2A-specific errors (`-32001..-32007`) | ❌ | Unsupported methods currently return `-32601`; dedicated codes are planned. |

## What's next

Conformance work is tracked as an epic: near-term, the card gains `protocolVersion` and honest
transport interfaces and the server returns A2A-specific error codes; further out, structured
artifacts and full task history; and, as a larger effort, a stateful task model that unlocks
`tasks/get`, cancellation, resubscription, non-blocking sends, and push notifications.

**Non-goals:** gRPC and A2A REST transport bindings, an extensions-negotiation framework, and
`tasks/list`. flux keeps to a single JSON-RPC/HTTP binding and a tolerant pass-through for unknown
fields.

## See also

- [Agent-to-agent (A2A)](./a2a.md) — using the client and server, security modes, and continuity.
