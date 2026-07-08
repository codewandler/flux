# A2A protocol conformance (epic)

**Status:** Tiers 1–3 implemented (Tier 3 core 2026-07-08; the `input-required` suspension slice remains open) · **Pillar:** Agent · **Epic slug:** `a2a-conformance`

Tracks the gap between the [A2A protocol](https://a2a-protocol.org/) (v0.3.0) and flux's
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
- **Fidelity** — `Task.history` (from the conversation projection, bounded by `historyLength`) and
  inbound `data` parts (surfaced into the turn) are now produced (A-51/A-52); `Task.artifacts` +
  `TaskArtifactUpdateEvent` are produced for a runner's structured outputs at the `A2aTurn` seam
  (A-52), and inbound `file` parts are refused with `-32005` (A-51). Still *modeled but not produced*:
  `Message.taskId`/`referenceTaskIds`, non-terminal `TaskState`s, and outbound `file` parts.
- **Error codes** — base JSON-RPC `-32600/-32601/-32602/-32603` ✅; A2A `-32004`/`-32005` ✅ (A-50);
  the task-lifecycle codes `-32001/-32002/-32003` are emitted since A-54/55/57; `-32006/-32007` remain defined-only.
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

### Tier 2 — I/O fidelity (✅ shipped)

Moderate; still within the synchronous-turn model.

- **[A-51](../stories/A-51-inbound-multimodal-parts.md)** ✅ — inbound parts, decided per kind in one
  shared boundary (`flux_a2a::server::extract_input`, used by every dispatch site): `data` parts are
  **surfaced** into the turn input as structured JSON (a data-only message now runs a real turn, not
  an empty one); `file` parts are **refused** with `-32005` (flux's turn is text-only, so a file is
  never silently dropped — even alongside text). `Part` gained first-class `as_data`/`as_file`
  accessors.
- **[A-52](../stories/A-52-outbound-task-fidelity.md)** ✅ — `Task.history` is populated from the
  conversation projection, capped to `configuration.historyLength` (new field); a runner's structured
  (non-text) reply parts become `Task.artifacts` at the `A2aTurn` seam, plus a reusable
  `artifact_update_value` frame shaper for streaming surfaces.

  **Scope boundary (documented, not a gap):** flux's engine emits only text — there is no structured
  (non-text) output channel in `AgentSink`. So artifacts are a capability of the reusable `A2aTurn`
  server seam (a runner that produces `A2aReply.extra_parts` gets them); flux-server's built-in text
  agent produces none, so its tasks carry `history` but empty `artifacts` — which is correct, not
  missing. Native flux-server artifact emission would first need an engine structured-output seam;
  that is out of scope here and left as a future follow-up if a structured-output producer lands.

### Tier 3 — stateful task model (design ✅; A-54..A-57 implemented 2026-07-08)

- **[A-53](../stories/A-53-stateful-a2a-task-model.md)** ✅ — the architectural decision below is now
  captured in its own design doc, [a2a-stateful-task-model](a2a-stateful-task-model.md), which fans
  into implementation stories **[A-54](../stories/A-54-addressable-tasks-get-nonblocking.md)**
  (foundation: task projection + non-blocking send + server-side `tasks/get`),
  **[A-55](../stories/A-55-tasks-cancel.md)** (`tasks/cancel`),
  **[A-56](../stories/A-56-tasks-resubscribe.md)** (`tasks/resubscribe`), and
  **[A-57](../stories/A-57-a2a-push-notifications.md)** (push notifications). Those are the deliberate
  model change; they are greenlit and executed separately.

## The Tier-3 architectural question (answered in the A-53 design)

To support the task-management half of the spec, an A2A `Task` must become a **first-class,
addressable, potentially-async** object rather than a synchronous return value. The questions below
are now answered in [a2a-stateful-task-model](a2a-stateful-task-model.md); they remain here as the
epic-level statement of the problem:

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
