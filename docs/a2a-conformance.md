# A2A protocol support

**What flux's [A2A](https://a2a-protocol.org/) (v1.0) implementation supports today, what's partial,
and what isn't yet.** This is the contributor copy (it links the backlog); the integrator-facing copy
is [`website/docs/agent/a2a-conformance.md`](../website/docs/agent/a2a-conformance.md). For *how to
use* A2A see [a2a.md](a2a.md); for the *why and in what order* of the gaps see the epic design
[designs/a2a-conformance.md](designs/a2a-conformance.md).

**Legend:** ✅ Supported · ⚠️ Partial · ❌ Not yet · 🚫 Non-goal

## The one thing to know

Since Tier 3 (A-54..A-57) an A2A task is **first-class and addressable**: task id = the flux
session id, a `Task` is a *projection* over the session's own turn-lifecycle events (no second
store), and `message/send` honors `configuration.blocking` — the spec default (absent/`false`)
returns a `submitted` task immediately and runs the turn in the background; `blocking: true` keeps
the synchronous completed-Task fast path. Live runs are held in an in-process registry (cancel /
resubscribe handles); terminal tasks are served purely from the event log, so a restart still
answers `tasks/get` for finished work. Continuity is by `contextId` (realm-keyed session reuse,
A-48); one context runs one task at a time (its id is stable across the context's turns).
`input-required`/`auth-required` (resume-on-`taskId`) remain the open slice.

## Methods

| Method | Status | Notes |
|---|---|---|
| `message/send` | ✅ | Server + client. `blocking: true` → synchronous completed Task; absent/`false` (spec default) → `submitted` now, run in background (A-54). |
| `message/stream` | ✅ | HTTP server + client (SSE). Client disconnect cancels the turn between plan rounds. |
| `tasks/get` | ✅ | Server + client (A-54): live tasks answer from the registry, finished ones from the event-log projection; realm-scoped; unknown/cross-realm → `-32001`. |
| `tasks/cancel` | ✅ | Fires the live run's `CancellationToken` out-of-band (A-55); terminal/not-in-process → `-32002`; unknown → `-32001`. |
| `tasks/resubscribe` | ✅ | Re-attach SSE (A-56): live tasks replay a state snapshot then follow the run's broadcast to the final frame; finished tasks replay the terminal frame and close. |
| `tasks/pushNotificationConfig/{set,get,list,delete}` | ✅ | Per-task webhook configs + best-effort delivery of status transitions (A-57); non-public URLs refused with `-32003`. |
| `agent/getAuthenticatedExtendedCard` | ❌ | No extended card; returns `-32004` (A-50). |

Reusable `flux-a2a::server::dispatch` handles only `message/send` (synchronous-turn); the stateful
task surface and `message/stream` live in the `flux-server` HTTP layer.

## AgentCard fields

| Field | Status | Notes |
|---|---|---|
| `name`, `description`, `url`, `version` | ✅ | |
| `protocolVersion` | ✅ | Emitted (A-49); single-source `flux_a2a::PROTOCOL_VERSION`. |
| `preferredTransport` | ✅ | `JSONRPC` (A-49), keyed to the served `url`. |
| `interfaces` (`additionalInterfaces`) | ✅ | One JSON-RPC entry advertising the served `url` (A-49); the `AgentInterface` is no longer emitted empty. |
| `capabilities.streaming` | ✅ | |
| `capabilities.pushNotifications` | ✅ | Advertised `true` on the flux-server surface (A-57 delivery implemented). |
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
| `TaskState`: `submitted` | ✅ | Emitted by a non-blocking send / the task projection (A-54). |
| `TaskState`: `canceled` | ✅ | Emitted by `tasks/cancel` + the projection of a cancelled run (A-55). |
| `TaskState`: `rejected` | ⚠️ | Defined; never emitted. |
| `TaskState`: `input-required`, `auth-required` | ❌ | Need the suspend/resume seam (resume-on-`taskId`) — the open Tier-3 slice. |
| `Task.history` | ✅ | Populated from the conversation projection, capped to `configuration.historyLength` (A-52). |
| `Task.artifacts` | ✅ | Emitted for a runner's structured (non-text) reply parts via the `A2aTurn` rich-output seam (A-52); the built-in text agent produces none, so its tasks stay `[]`. |

## Message & Parts

| Item | Status | Notes |
|---|---|---|
| `Message` (`messageId`, `role`, `parts`, `kind`) | ✅ | |
| `Message.contextId` | ✅ | Drives realm-keyed continuity (A-48). |
| `Message.taskId` | ⚠️ | Parsed; ignored (resume-on-`taskId` rides the `input-required` slice). |
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
| `-32001` TaskNotFound | ✅ | Emitted for unknown/cross-realm/non-A2A task ids — one constant answer (A-54). |
| `-32002` TaskNotCancelable | ✅ | Emitted for terminal or not-in-process tasks (A-55). |
| `-32003` PushNotificationNotSupported | ✅ | Emitted for refused push URLs (scheme/host policy, A-57). |
| `-32006` / `-32007` | ❌ | Constants defined (`flux_a2a::error`); no producing path yet. |

## Push notifications & extensions

- **Push notifications** — ✅ per-task webhook configs + best-effort delivery of status
  transitions (A-57): one POST per transition, 10s timeout, no retry (the durable task projection
  is the source of truth); config `token` rides as `X-A2A-Notification-Token`. SSRF posture: only
  public `http(s)` endpoints (loopback/private/link-local literals and `localhost` refused;
  `FLUX_A2A_PUSH_ALLOW_LOCAL=1` for local development).
- **Extensions** — ⚠️ tolerant passthrough only: unknown fields survive round-trips via
  `#[serde(flatten)]`, but there is no extension-URI declaration/activation. Negotiation is a 🚫
  non-goal.
- **Blocking vs non-blocking** — ✅ the server honors `configuration.blocking` (A-54); the spec
  default is non-blocking. **Behavior change:** a send that omits `blocking: true` now returns a
  `submitted` task instead of blocking to completion — poll `tasks/get` or use
  `tasks/resubscribe` (flux's own client/CLI always sent `blocking: true` and is unaffected).

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
| 3 (design ✅) | [A-53](stories/A-53-stateful-a2a-task-model.md) | The stateful task model design → A-54..A-57 |
| 3 (✅ shipped) | [A-54](stories/A-54-addressable-tasks-get-nonblocking.md) | Task projection + registry, non-blocking send, server-side `tasks/get` |
| 3 (✅ shipped) | [A-55](stories/A-55-tasks-cancel.md) | `tasks/cancel` via the live-task `CancellationToken` registry |
| 3 (✅ shipped) | [A-56](stories/A-56-tasks-resubscribe.md) | `tasks/resubscribe`: snapshot replay + live follow |
| 3 (✅ shipped) | [A-57](stories/A-57-a2a-push-notifications.md) | Push-notification configs + webhook delivery; card capability flipped |

**Non-goals** (documented, not filed): gRPC + A2A REST bindings, extensions-negotiation framework,
`tasks/list`. See the epic design for rationale.

## See also

- [a2a.md](a2a.md) — how to use the client and server.
- [designs/a2a-conformance.md](designs/a2a-conformance.md) — the epic (why, ordering, the Tier-3
  architectural decision).
