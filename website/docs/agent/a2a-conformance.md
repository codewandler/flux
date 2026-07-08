---
title: A2A protocol support
---

# A2A protocol support

What flux's [A2A protocol](https://a2a-protocol.org/) (v0.3.0) implementation supports today, what's
partial, and what isn't yet — so you know exactly what you can rely on when you point an A2A client at
flux or drive a remote agent with `flux a2a`. For how to use it, see [Agent-to-agent (A2A)](./a2a.md).

**Legend:** ✅ Supported · ⚠️ Partial · ❌ Not yet · 🚫 Non-goal

## The one thing to know

An A2A task on flux is **addressable**: `message/send` honors `configuration.blocking` — the spec
default (absent/`false`) returns a `submitted` task immediately and runs the turn in the
background, while `blocking: true` blocks to the finished `Task`. Poll a task with `tasks/get`,
stream it with `tasks/resubscribe`, stop it with `tasks/cancel`, or register a webhook with
`tasks/pushNotificationConfig/set`. A finished task stays readable for as long as its session is
retained (the TTL sweep). Multi-turn continuity is by `contextId`; a context runs one task at a
time, and its task id is stable across the context's turns.

**Upgrading:** a send that omits `blocking: true` now returns a `submitted` task instead of
blocking to completion — set `blocking: true` for the old synchronous behavior. Clients using
flux's own client/CLI are unaffected (they always sent `blocking: true`).

## Methods

| Method | Status | Notes |
|---|---|---|
| `message/send` | ✅ | `blocking: true` → synchronous completed Task; absent/`false` (the spec default) → `submitted` immediately, run in background. |
| `message/stream` | ✅ | Server-Sent Events. Disconnecting cancels the remote turn. |
| `tasks/get` | ✅ | Poll a live or finished task to its current state; unknown ids → `-32001`. |
| `tasks/cancel` | ✅ | Stops a live run between plan rounds; terminal tasks → `-32002`. |
| `tasks/resubscribe` | ✅ | Re-attach an SSE stream: live tasks stream to the final frame, finished tasks replay their terminal state. |
| `tasks/pushNotificationConfig/{set,get,list,delete}` | ✅ | Per-task webhooks; status transitions POST to your URL (`token` echoes back as `X-A2A-Notification-Token`). |
| `agent/getAuthenticatedExtendedCard` | ❌ | Returns `-32004 UnsupportedOperation`. |

## AgentCard

| Field | Status | Notes |
|---|---|---|
| `name`, `description`, `url`, `version` | ✅ | |
| `capabilities.streaming` | ✅ | |
| `capabilities.pushNotifications` | ✅ | Advertised `true` — per-task webhook delivery is implemented. |
| `defaultInputModes`, `defaultOutputModes` | ✅ | `text/plain`. |
| `skills` | ✅ | |
| `securitySchemes`, `security` | ✅ | Declared whenever the server enforces auth. |
| `protocolVersion` | ✅ | Emitted as `0.3.0` (spec-required). |
| `preferredTransport`, `interfaces` | ✅ | The card declares its JSON-RPC interface (`preferredTransport: "JSONRPC"` + one `interfaces` entry at `url`). |
| `provider`, `documentationUrl`, `iconUrl` | ✅ | Optional; emitted when configured for the served agent, otherwise omitted. |
| `supportsAuthenticatedExtendedCard` | ✅ | Emitted `false` (no extended-card method yet). |
| `signatures` | 🚫 | Card signing is out of scope. |

## Tasks, messages & streaming

| Item | Status | Notes |
|---|---|---|
| `Task` / `TaskStatus`; states `submitted` / `working` / `completed` / `canceled` / `failed` | ✅ | The full non-suspension lifecycle. |
| Task states `input-required` / `auth-required` | ❌ | Need the suspend/resume seam (resume on `taskId`). |
| `Task.history` | ✅ | Populated from the conversation; set `configuration.historyLength` to cap it to the most-recent messages. |
| `Task.artifacts` | ✅ | Carries a turn's structured (non-text) outputs. flux's built-in text agent produces none, so its tasks stay `[]`. |
| `Message` (`messageId`, `role`, `parts`, `contextId`) | ✅ | `contextId` drives continuity. |
| `Message.taskId` / `referenceTaskIds` | ⚠️/❌ | Parsed but not yet used to resume a task (`input-required` is the open slice). |
| Part: `text` | ✅ | |
| Part: `data` (inbound) | ✅ | Surfaced into the turn as structured JSON, so a data-only message runs a real turn. |
| Part: `file` (inbound) | ⚠️ | Refused with `-32005 ContentTypeNotSupported` (flux's turn is text-only) rather than silently dropped — send text or a `data` part. |
| `TaskStatusUpdateEvent` (streaming) | ✅ | Incremental `working` deltas + a final event. |
| `TaskArtifactUpdateEvent` (streaming) | ⚠️ | Emitted by a streaming surface that produces structured outputs; flux's built-in text agent produces none. |

## Transports & errors

| Item | Status | Notes |
|---|---|---|
| JSON-RPC 2.0 over HTTP | ✅ | The transport flux serves and speaks. |
| gRPC, HTTP+JSON/REST bindings | 🚫 | Non-goals. |
| Base JSON-RPC errors (`-32600`/`-32601`/`-32602`/`-32603`) | ✅ | |
| A2A errors `-32004` UnsupportedOperation, `-32005` ContentTypeNotSupported | ✅ | Emitted for defined-but-unsupported methods and for messages with a `file` part / no usable text or data; a genuinely-unknown method still returns `-32601`. |
| Task errors `-32001` TaskNotFound, `-32002` TaskNotCancelable, `-32003` PushNotificationNotSupported | ✅ | Unknown/foreign task ids are one constant `-32001`; `-32003` refuses non-public push URLs. |
| `-32006` / `-32007` | ❌ | No producing path yet. |

## Push notification delivery

Delivery is best-effort by design: one POST per status transition (never per-token deltas), a 10s
timeout, and no retries — the durable task state from `tasks/get` is the source of truth; a push
is a hint to poll. Webhook URLs must be public `http(s)` endpoints (loopback, private, and
link-local addresses are refused with `-32003`; set `FLUX_A2A_PUSH_ALLOW_LOCAL=1` for local
development). Configs live in server memory: re-register after a server restart.

## What's next

The one remaining lifecycle slice is suspension: surfacing the engine's suspend/resume seam as
`input-required` / `auth-required` tasks that a follow-up `message/send` carrying the same
`taskId` resumes.

**Non-goals:** gRPC and A2A REST transport bindings, an extensions-negotiation framework, and
`tasks/list`. flux keeps to a single JSON-RPC/HTTP binding and a tolerant pass-through for unknown
fields.

## See also

- [Agent-to-agent (A2A)](./a2a.md) — using the client and server, security modes, and continuity.
