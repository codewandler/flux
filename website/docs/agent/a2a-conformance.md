---
title: A2A protocol support
description: "Protocol support matrix for flux's A2A implementation and what each method returns."
---

# A2A protocol support

This is the compatibility matrix for flux's [A2A protocol](https://a2a-protocol.org/) (v0.3.0)
implementation. Use it when you need to know which methods, task states, agent-card fields, and error
paths are stable enough to rely on.

For the operator-facing setup guide, start with [Agent-to-agent (A2A)](./a2a.md).

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
| `tasks/pushNotificationConfig/{set,get,list,delete}` | ✅ | Per-task webhooks; status transitions POST to your URL (`token` echoes back as `X-A2A-Notification-Token`); destinations are DNS-checked and redirects are refused. |
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
| Task errors `-32001` TaskNotFound, `-32002` TaskNotCancelable, `-32003` PushNotificationNotSupported | ✅ | Unknown/foreign task ids are one constant `-32001`; `-32003` refuses destinations outside the scoped URL policy. |
| `-32006` / `-32007` | ❌ | No producing path yet. |

## Push notification delivery

Delivery is best-effort by design: one POST per status transition (never per-token deltas), a 10s
timeout, and no retries — the durable task state from `tasks/get` is the source of truth; a push
is a hint to poll. Webhook URLs must be public `http(s)` endpoints by default. Flux resolves the
hostname and refuses loopback, private, link-local, CGNAT, mapped, and internal destinations;
delivery repeats that check and refuses redirects so a notification token cannot be forwarded to a
different origin. Operators can name exact private destinations in
`FLUX_A2A_PUSH_PRIVATE_HOSTS`; `FLUX_A2A_PUSH_ALLOW_LOCAL=1` is a compatibility shortcut limited to
the three loopback spellings. Configs live in server memory: re-register after a server restart.

## What's next

The one remaining lifecycle slice is suspension: surfacing the engine's suspend/resume seam as
`input-required` / `auth-required` tasks that a follow-up `message/send` carrying the same
`taskId` resumes.

**Non-goals:** gRPC and A2A REST transport bindings, an extensions-negotiation framework, and
`tasks/list`. flux keeps to a single JSON-RPC/HTTP binding and a tolerant pass-through for unknown
fields.

## Related docs

- [Agent-to-agent (A2A)](./a2a.md) — run flux as an A2A client or server.
- [Server authentication & tenancy](../security/server-auth.md) — protect exposed endpoints.
