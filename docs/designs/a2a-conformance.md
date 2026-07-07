# A2A protocol conformance (epic)

**Status:** Tier 1 shipped (2026-07-07); Tiers 2–3 backlog · **Pillar:** Agent · **Epic slug:** `a2a-conformance`

Tracks the gap between the [A2A protocol](https://a2a-protocol.org/) (v1.0) and flux's
implementation, and sequences the work to close it. The living support matrix lives in
[`../a2a-conformance.md`](../a2a-conformance.md) (contributor copy) and
[`website/docs/agent/a2a-conformance.md`](../../website/docs/agent/a2a-conformance.md) (public copy);
this design is the *why and in what order*.

## Context

flux speaks A2A in both directions — server (`flux app run --serve`, the `a2a` program channel) and
client (`flux a2a <URL>`). After v0.4.0 (multi-tenant principal auth + multi-agent mount) the wire
surface is stable, so it's worth measuring against the spec and turning the gaps into a ranked
backlog. The core shape of our implementation — and the root of most gaps — is one architectural
choice:

> **flux runs an A2A request as one synchronous turn and returns a `completed` `Task`.** There is no
> retained, addressable async task: `message/send` blocks to completion, `configuration.blocking` is
> ignored, and the returned `Task` has no id a later call can look up. Continuity is by `contextId`
> (realm-keyed A-48 session reuse), not by task id.

That choice keeps the server simple and is fine for the request/reply and streaming agents flux
serves today. It is also exactly what blocks the whole "task management" half of the spec
(`tasks/get` server-side, `tasks/cancel`, `tasks/resubscribe`, non-blocking send, `input-required`
resumption, push notifications). So the gaps split cleanly into *conformance polish that fits the
current model* (Tiers 1–2) and *a deliberate model change* (Tier 3).

## Gap analysis (condensed; full matrix + file:line evidence in the support doc)

- **Methods** — `message/send` ✅, `message/stream` ✅, `tasks/get` client-only ⚠️. `tasks/cancel`,
  `tasks/resubscribe`, `tasks/pushNotificationConfig/*`, `agent/getAuthenticatedExtendedCard` ❌
  (unimplemented, but now answered `-32004 UnsupportedOperation` rather than generic `-32601` — A-50).
- **AgentCard** — ✅ closed by A-49: `protocolVersion`, `preferredTransport`, a populated `interfaces`
  entry, the optional `provider`/`documentationUrl`/`iconUrl`, and `supportsAuthenticatedExtendedCard: false`.
- **Fidelity** — `Task.history`/`artifacts`, `TaskArtifactUpdateEvent`, file/data `Part`s,
  `Message.taskId`/`referenceTaskIds`, and non-terminal `TaskState`s are *modeled and
  client-decodable but never produced* by the server. (Text-less inbound messages are now refused
  with `-32005` — A-50 — rather than silently run empty; *accepting* file/data input is A-51.)
- **Error codes** — base JSON-RPC `-32600/-32601/-32602/-32603` ✅; A2A `-32004`/`-32005` ✅ (A-50);
  the task-lifecycle codes `-32001/-32002/-32003/-32006/-32007` are defined but await A-53.
- **Transports** — JSON-RPC/HTTP only.
- **Solid** — realm-keyed `contextId` continuity (A-48), security-scheme advertisement + external-URL
  card (v0.4.0), SSE streaming with disconnect-cancels-turn.

## Tiers

### Tier 1 — conformance quick-wins (✅ shipped)

Cheap, high interop value, no architecture change.

- **[A-49](../stories/A-49-agent-card-conformance-fields.md)** ✅ — AgentCard conformance fields:
  `protocolVersion` (single-source `flux_a2a::PROTOCOL_VERSION`), honest `interfaces` +
  `preferredTransport` (the JSON-RPC interface we actually serve, keyed to `url`), optional
  `provider`/`documentationUrl`/`iconUrl` (emitted from `CardInfo` when set), and
  `supportsAuthenticatedExtendedCard: false`.
- **[A-50](../stories/A-50-a2a-error-codes.md)** ✅ — A2A-specific JSON-RPC error codes: the
  `-32001..-32007` constants (`flux_a2a::error`); `-32004 UnsupportedOperation` for
  defined-but-unsupported methods (instead of generic `-32601`) via one shared classifier both
  dispatch sites use; `-32005 ContentTypeNotSupported` when a message has no usable text part.

### Tier 2 — I/O fidelity (`backlog`)

Moderate; still within the synchronous-turn model.

- **[A-51](../stories/A-51-inbound-multimodal-parts.md)** — accept `file`/`data` input `Part`s
  (surface to the agent) or refuse cleanly with `-32005`, rather than silently dropping them.
- **[A-52](../stories/A-52-outbound-task-fidelity.md)** — populate `Task.history` + honor
  `historyLength`; emit `TaskArtifactUpdateEvent`/`Task.artifacts` for structured outputs.

### Tier 3 — stateful task model (`backlog`, design-first)

- **[A-53](../stories/A-53-stateful-a2a-task-model.md)** — the architectural decision below, captured
  as its own design doc, which then fans into implementation stories.

## The Tier-3 architectural question (for A-53's design)

To support the task-management half of the spec, an A2A `Task` must become a **first-class,
addressable, potentially-async** object rather than a synchronous return value. Open questions the
A-53 design must answer:

1. **Retention & identity.** Where does a `Task` live between calls? The natural home is a projection
   over `events.db` (like conversation/run-trace already are — see [event-store
   unification](event-store-unification.md)), keyed by an A2A `task-id` that maps to a flux run.
   Retention/TTL must be realm-scoped, reusing the A-48 realm key so tasks never leak across tenants.
2. **Async execution.** Non-blocking `message/send` must return `submitted`/`working` immediately and
   run the turn on a background task the client polls (`tasks/get`) or resubscribes to
   (`tasks/resubscribe`). This is a real lifecycle change to the currently-synchronous handler.
3. **Cancellation.** `tasks/cancel` needs a handle to an in-flight run. flux already cancels a
   streaming turn on SSE disconnect via a `CancellationToken`; `tasks/cancel` generalizes that to an
   out-of-band signal against a retained task.
4. **Suspension.** `input-required`/`auth-required` map naturally onto the engine's existing
   suspend/resume seam (the same one journeys park on), surfaced as A2A task states.
5. **Push notifications.** `tasks/pushNotificationConfig/*` + webhook delivery layer on top of the
   retained-task model (deliver `TaskStatusUpdateEvent`/`TaskArtifactUpdateEvent` to a registered
   URL). Push is the last/optional slice; it presupposes 1–2.

The design should decide how much of this to build (e.g. `tasks/get`+cancel+resubscribe first, push
notifications as a later slice) and must not regress the synchronous fast path clients use today.

## Non-goals (documented, not filed)

- **gRPC and A2A HTTP+JSON/REST transport bindings.** JSON-RPC over HTTP is sufficient for flux's
  clients; a second/third binding is a large surface for little gain. Revisit only on concrete demand.
- **Extensions negotiation framework.** Keep the current tolerant `#[serde(flatten)] extra`
  passthrough (unknown fields survive round-trips); no extension-URI declaration/activation logic.
- **`tasks/list`.** Depends on task retention; folds into Tier 3 if ever pursued.

## Consequences

Tiers 1–2 are additive and non-breaking (new card fields serialize only when present; new error codes
only replace today's generic ones on already-failing paths). Tier 3 is a model change scoped behind
its own design and is likely a minor (breaking) bump if handler signatures change. Repo policy: all
artifacts stay protocol-focused — no downstream-consumer names.
