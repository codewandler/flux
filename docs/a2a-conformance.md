# A2A protocol support

**What flux's [A2A](https://a2a-protocol.org/) (v1.0) implementation supports today, what's partial,
and what isn't yet.** This is the contributor copy (it links the backlog); the integrator-facing copy
is [`website/docs/agent/a2a-conformance.md`](../website/docs/agent/a2a-conformance.md). For *how to
use* A2A see [a2a.md](a2a.md); for the *why and in what order* of the gaps see the epic design
[designs/a2a-conformance.md](designs/a2a-conformance.md).

**Legend:** ✅ Supported · ⚠️ Partial · ❌ Not yet · 🚫 Non-goal

## The one thing to know

flux runs an A2A request as **one synchronous turn** and returns a `completed` `Task`. There is no
retained, addressable async task: `message/send` blocks to completion, `configuration.blocking` is
ignored, and the returned `Task` has no id a later call can look up. Continuity is by `contextId`
(realm-keyed session reuse, A-48), not by task id. This single choice is why the "task management"
half of the spec (`tasks/get` server-side, `tasks/cancel`, `tasks/resubscribe`, non-blocking send,
`input-required`, push) is Not-yet — closing it is a deliberate model change (A-53, Tier 3).

## Methods

| Method | Status | Notes |
|---|---|---|
| `message/send` | ✅ | Server + client. Synchronous; returns a `completed` Task. |
| `message/stream` | ✅ | HTTP server + client (SSE). Client disconnect cancels the turn between plan rounds. |
| `tasks/get` | ⚠️ | The **client** can call it; there is **no server handler** (→ `-32601`). Needs task retention. |
| `tasks/cancel` | ❌ | No server code; needs an addressable in-flight task. |
| `tasks/resubscribe` | ❌ | No server code; needs a retained task stream. |
| `tasks/pushNotificationConfig/{set,get,list,delete}` | ❌ | No server code. |
| `agent/getAuthenticatedExtendedCard` | ❌ | No extended card. |

Reusable `flux-a2a::server::dispatch` handles only `message/send`; `message/stream` is added by the
`flux-server` HTTP layer.

## AgentCard fields

| Field | Status | Notes |
|---|---|---|
| `name`, `description`, `url`, `version` | ✅ | |
| `protocolVersion` | ❌ | **Spec-required** and absent from the struct. |
| `preferredTransport` | ❌ | Not modeled. |
| `interfaces` (`additionalInterfaces`) | ⚠️ | Modeled (`AgentInterface`) but the server emits it **empty**, so no transport is declared. |
| `capabilities.streaming` | ✅ | |
| `capabilities.pushNotifications` | ✅ | Advertised `false` (honest); flips true only with push support. |
| `capabilities.stateTransitionHistory` | ❌ | Survives inbound via passthrough; never emitted. |
| `capabilities.extensions` | ❌ | Passthrough only; negotiation is a non-goal. |
| `defaultInputModes`, `defaultOutputModes` | ✅ | `text/plain`. |
| `skills` | ✅ | |
| `securitySchemes`, `security` | ✅ | Advertised whenever auth is enabled (v0.4.0). |
| `provider`, `documentationUrl`, `iconUrl` | ❌ | Optional; not modeled. |
| `supportsAuthenticatedExtendedCard` | ❌ | Not modeled. |
| `signatures` | 🚫 | Card signing — out of scope for now. |

## Task lifecycle

| Item | Status | Notes |
|---|---|---|
| `Task`, `TaskStatus` | ✅ | Returned by send/stream. |
| `TaskState`: `working`, `completed`, `failed` | ✅ | Emitted. |
| `TaskState`: `unknown` | ✅ | Serde catch-all. |
| `TaskState`: `submitted`, `rejected` | ⚠️ | Defined; never emitted (synchronous model). |
| `TaskState`: `input-required`, `auth-required`, `canceled` | ❌ | Need suspension / cancellation (A-53). |
| `Task.history` | ⚠️ | Modeled + client-read; server never populates. |
| `Task.artifacts` | ⚠️ | Modeled + client-read; server never populates. |

## Message & Parts

| Item | Status | Notes |
|---|---|---|
| `Message` (`messageId`, `role`, `parts`, `kind`) | ✅ | |
| `Message.contextId` | ✅ | Drives realm-keyed continuity (A-48). |
| `Message.taskId` | ⚠️ | Parsed; ignored. |
| `Message.referenceTaskIds`, `metadata`, `extensions` | ❌ | Not modeled on `Message`. |
| Part: `text` | ✅ | |
| Part: `data` | ⚠️ | Emittable via the rich-output seam; **ignored on input**. |
| Part: `file` | ⚠️ | Round-trips via passthrough; never produced, ignored on input. |

## Streaming events

| Event | Status | Notes |
|---|---|---|
| `TaskStatusUpdateEvent` | ✅ | Emitted (initial `working`, per-delta, final) + decoded. |
| `Task` / `Message` as stream result | ✅ | Client-decodable. |
| `TaskArtifactUpdateEvent` | ⚠️ | Client-decode only; server never emits. |

## Transports

| Transport | Status | Notes |
|---|---|---|
| JSON-RPC 2.0 over HTTP | ✅ | The one binding flux serves and speaks. |
| gRPC | 🚫 | Non-goal. |
| HTTP+JSON / REST binding | 🚫 | Non-goal. (flux's own `/sessions` REST API is unrelated to the A2A REST transport.) |

## Error codes

| Code | Status | Notes |
|---|---|---|
| `-32600` / `-32601` / `-32602` / `-32603` | ✅ | Emitted. |
| `-32700` Parse error | ⚠️ | Malformed JSON is rejected by the HTTP JSON extractor, not as an A2A-coded body. |
| `-32001..-32007` (A2A-specific) | ❌ | None emitted. A-50 lands `-32004`/`-32005`; the task-method codes arrive with A-53. |

## Push notifications & extensions

- **Push notifications** — ❌ entirely absent (capability advertised `false`, which is honest).
  Depends on the retained-task model (A-53).
- **Extensions** — ⚠️ tolerant passthrough only: unknown fields survive round-trips via
  `#[serde(flatten)]`, but there is no extension-URI declaration/activation. Negotiation is a 🚫
  non-goal.
- **Blocking vs non-blocking** — ⚠️ the client sends `configuration.blocking`; the server ignores it
  and is always synchronous. Non-blocking is A-53.

## Solid ground

Realm-keyed `contextId` continuity (A-48), security-scheme advertisement + external-URL card
(v0.4.0), and SSE streaming where a client disconnect cleanly cancels the in-flight turn.

## Gaps → backlog

Tracked under the [`a2a-conformance`](designs/a2a-conformance.md) epic:

| Tier | Story | Closes |
|---|---|---|
| 1 (ready) | [A-49](stories/A-49-agent-card-conformance-fields.md) | `protocolVersion`, `preferredTransport`, populated `interfaces`, `provider`/`documentationUrl`/`iconUrl`, `supportsAuthenticatedExtendedCard` |
| 1 (ready) | [A-50](stories/A-50-a2a-error-codes.md) | A2A error codes: `-32004` for unsupported methods, `-32005` for unusable content |
| 2 (backlog) | [A-51](stories/A-51-inbound-multimodal-parts.md) | Inbound `file`/`data` parts (accept or refuse cleanly) |
| 2 (backlog) | [A-52](stories/A-52-outbound-task-fidelity.md) | `Task.history` + `historyLength`, artifact emission |
| 3 (design-first) | [A-53](stories/A-53-stateful-a2a-task-model.md) | The stateful task model: `tasks/get` server-side, `cancel`, `resubscribe`, non-blocking send, `input-required`, push |

**Non-goals** (documented, not filed): gRPC + A2A REST bindings, extensions-negotiation framework,
`tasks/list`. See the epic design for rationale.

## See also

- [a2a.md](a2a.md) — how to use the client and server.
- [designs/a2a-conformance.md](designs/a2a-conformance.md) — the epic (why, ordering, the Tier-3
  architectural decision).
