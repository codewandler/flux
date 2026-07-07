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
| `tasks/cancel` | ❌ | No server code; returns `-32004 UnsupportedOperation` (A-50). Needs an addressable in-flight task. |
| `tasks/resubscribe` | ❌ | No server code; returns `-32004` (A-50). Needs a retained task stream. |
| `tasks/pushNotificationConfig/{set,get,list,delete}` | ❌ | No server code; returns `-32004` (A-50). |
| `agent/getAuthenticatedExtendedCard` | ❌ | No extended card; returns `-32004` (A-50). |

Reusable `flux-a2a::server::dispatch` handles only `message/send`; `message/stream` is added by the
`flux-server` HTTP layer.

## AgentCard fields

| Field | Status | Notes |
|---|---|---|
| `name`, `description`, `url`, `version` | ✅ | |
| `protocolVersion` | ✅ | Emitted (A-49); single-source `flux_a2a::PROTOCOL_VERSION`. |
| `preferredTransport` | ✅ | `JSONRPC` (A-49), keyed to the served `url`. |
| `interfaces` (`additionalInterfaces`) | ✅ | One JSON-RPC entry advertising the served `url` (A-49); the `AgentInterface` is no longer emitted empty. |
| `capabilities.streaming` | ✅ | |
| `capabilities.pushNotifications` | ✅ | Advertised `false` (honest); flips true only with push support. |
| `capabilities.stateTransitionHistory` | ❌ | Survives inbound via passthrough; never emitted. |
| `capabilities.extensions` | ❌ | Passthrough only; negotiation is a non-goal. |
| `defaultInputModes`, `defaultOutputModes` | ✅ | `text/plain`. |
| `skills` | ✅ | |
| `securitySchemes`, `security` | ✅ | Advertised whenever auth is enabled (v0.4.0). |
| `provider`, `documentationUrl`, `iconUrl` | ✅ | Modeled + emitted when the served agent's `CardInfo` sets them (A-49); the default card sets none, so the keys stay omitted. |
| `supportsAuthenticatedExtendedCard` | ✅ | Emitted `false` (A-49; honest — no extended-card method yet). |
| `signatures` | 🚫 | Card signing — out of scope for now. |

## Task lifecycle

| Item | Status | Notes |
|---|---|---|
| `Task`, `TaskStatus` | ✅ | Returned by send/stream. |
| `TaskState`: `working`, `completed`, `failed` | ✅ | Emitted. |
| `TaskState`: `unknown` | ✅ | Serde catch-all. |
| `TaskState`: `submitted`, `rejected` | ⚠️ | Defined; never emitted (synchronous model). |
| `TaskState`: `input-required`, `auth-required`, `canceled` | ❌ | Need suspension / cancellation (A-53). |
| `Task.history` | ✅ | Populated from the conversation projection, capped to `configuration.historyLength` (A-52). |
| `Task.artifacts` | ✅ | Emitted for a runner's structured (non-text) reply parts via the `A2aTurn` rich-output seam (A-52); the built-in text agent produces none, so its tasks stay `[]`. |

## Message & Parts

| Item | Status | Notes |
|---|---|---|
| `Message` (`messageId`, `role`, `parts`, `kind`) | ✅ | |
| `Message.contextId` | ✅ | Drives realm-keyed continuity (A-48). |
| `Message.taskId` | ⚠️ | Parsed; ignored. |
| `Message.referenceTaskIds`, `metadata`, `extensions` | ❌ | Not modeled on `Message`. |
| Part: `text` | ✅ | |
| Part: `data` | ✅ | Inbound: surfaced into the turn input as structured JSON, so a data-only message runs a real turn (A-51). Outbound: a runner's structured parts become `Task.artifacts` (A-52). |
| Part: `file` | ⚠️ | Inbound `file` parts are refused with `-32005` (A-51 scope decision — flux's text turn can't consume file bytes), never silently dropped; still never produced outbound. |

## Streaming events

| Event | Status | Notes |
|---|---|---|
| `TaskStatusUpdateEvent` | ✅ | Emitted (initial `working`, per-delta, final) + decoded. |
| `Task` / `Message` as stream result | ✅ | Client-decodable. |
| `TaskArtifactUpdateEvent` | ⚠️ | Frame shaping added (`flux_a2a::server::artifact_update_value`, A-52) for a streaming surface with structured outputs; flux's built-in text agent produces none, so it emits none. |

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
| `-32004` UnsupportedOperation | ✅ | Emitted (A-50) for a defined-but-unsupported method; a genuinely-unknown name keeps `-32601`. |
| `-32005` ContentTypeNotSupported | ✅ | Emitted when an inbound message carries a `file` part (A-51) or otherwise has parts but no usable text/data part (A-50). |
| `-32001` / `-32002` / `-32003` / `-32006` / `-32007` | ❌ | Constants defined (`flux_a2a::error`); the task-lifecycle codes await the retained-task model (A-53). |

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
| 1 (✅ shipped) | [A-49](stories/A-49-agent-card-conformance-fields.md) | `protocolVersion`, `preferredTransport`, populated `interfaces`, `provider`/`documentationUrl`/`iconUrl`, `supportsAuthenticatedExtendedCard` |
| 1 (✅ shipped) | [A-50](stories/A-50-a2a-error-codes.md) | A2A error codes: `-32004` for unsupported methods, `-32005` for unusable content |
| 2 (✅ shipped) | [A-51](stories/A-51-inbound-multimodal-parts.md) | Inbound `data` parts surfaced into the turn; `file` parts refused with `-32005` |
| 2 (✅ shipped) | [A-52](stories/A-52-outbound-task-fidelity.md) | `Task.history` + `historyLength`; artifacts from a runner's structured outputs |
| 3 (design-first) | [A-53](stories/A-53-stateful-a2a-task-model.md) | The stateful task model: `tasks/get` server-side, `cancel`, `resubscribe`, non-blocking send, `input-required`, push |

**Non-goals** (documented, not filed): gRPC + A2A REST bindings, extensions-negotiation framework,
`tasks/list`. See the epic design for rationale.

## See also

- [a2a.md](a2a.md) — how to use the client and server.
- [designs/a2a-conformance.md](designs/a2a-conformance.md) — the epic (why, ordering, the Tier-3
  architectural decision).
