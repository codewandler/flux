# SDK surface — a standard agent SDK, the flux way

Status: **approved** (2026-07-11). Epic: `sdk-surface` (stories D-142…D-159, four waves).
Owner surface: `crates/flux-sdk` (`codewandler-flux-sdk`).

## Problem

`flux-sdk` exposes two front doors on the one engine — `Client` (a collected turn) and
`FlowClient` (`compile → analyze → execute`) — and very little else. Measured against what an
agent SDK is expected to offer (streaming events, custom function tools, persistent sessions
with resume, approval callbacks, sub-agents, usage/cost, cancellation), and against what flux
itself already ships behind the CLI, the gaps are structural:

- **No streaming.** `flux_flow::AgentSink` exists (text/thinking/plan deltas, tool calls and
  results, observations, turn end) but `Client::run` collects through a private sink that drops
  tool results; a consumer cannot observe a live turn or cancel one.
- **No persistence.** `EventStore::in_memory()` / `FlowStore::in_memory()` are hardcoded;
  sessions die with the process. History, run traces, cost/efficiency projections, replay/fork —
  all recorded, none reachable.
- **No resume, no suspensions.** `FlowClient` turns a top-level `await` into an error; the
  engine's suspension-first resume branch and `start_flow_turn` (D-131) have no SDK door — which
  is also exactly why the flow-driven voice front door (D-132) was deferred.
- **Envelope asymmetry.** `FlowClient` takes custom ops, packs, an injected `Approver`, and
  sub-agents; `Client` takes none of those — builtins only, binary allow/deny.
- **Dependency scavenger hunt.** Types in the SDK's own public signatures (`Provider`, `Tool`,
  `Approver`, `SubAgents`, `VoiceSink`, …) are not re-exported; a real consumer adds 4–5 crates.
- **Provider-by-spec construction exists only inside `flux-cli`** (`build_provider`), so every
  embedder re-implements model-spec → provider wiring, including the `claude`/`codex`
  subscription token sources.

## Shape decision: two doors + a `Session` handle

`Client` and `FlowClient` stay distinct — a durable conversational agent and a one-shot flow
lifecycle are different products on one engine, and the docs already teach that split. Neither
absorbs the other; there is **no third client type**:

- **`Client`** owns the assembled engine (`Arc<FlowEngine>`, expensive, long-lived).
- **`Session`** is a cheap cloneable handle `{Arc<FlowEngine>, session_id}` — the resumable
  surface that carries turns, streaming, flow-driven sessions, voice, projections, and the time
  machine. Flow-driven sessions (D-131/D-132) are `Session::start_flow(...)`, not a new type,
  because they ride exactly the machinery `Client` owns (`FlowEngine::start_flow_turn` plus
  `run_turn`'s suspension-first resume branch).
- **`FlowClient`** keeps rejecting top-level `await` — its one-shot contract is a feature — with
  the error message retargeted at `Session::start_flow`.

| Need | Door |
|---|---|
| Conversational turns, streaming, resume, durable `await` flows, voice, replay/fork | `Client` → `Session` |
| compile/parse → analyze → execute one flow, seeded inputs, optimizer, composites | `FlowClient` |
| Author the AST in Rust | `dsl` + `recipes` (unchanged) |

## Storage

```rust
pub struct Storage(/* private */);
impl Storage {
    pub fn in_memory() -> Self;                                  // default; today's behavior
    pub fn dir(path: impl Into<PathBuf>) -> Self;                // <dir>/events.db + <dir>/flow.db
    pub fn custom(events: Arc<EventStore>, flow: FlowStore) -> Self;  // BYO (covers Postgres)
}
```

`Storage::dir` uses the CLI's exact `events.db`/`flow.db` convention, so a directory persisted by
the SDK is readable by `flux sessions` / `flux replay` / `flux fork`. This single seam unlocks
four features at once: resume (`open_session` against a store that outlives the process),
suspensions-as-a-feature (the engine already persists and resumes them; the SDK stops erroring),
flow-driven voice (`EngineVoiceHandler` needs `Arc<FlowEngine>` + a durable session id), and the
time machine (replay/fork operate on `(EventStore, Executor, session_id)` — all pub on the
engine). In-memory stays wired through `FlowStore::in_memory_with_events` so events remain
connected.

## Client / Session surface

```rust
impl Client {
    // Session lifecycle — THE resumability seam:
    pub fn create_session(&self) -> Result<Session>;
    pub fn open_session(&self, id: &str) -> Result<Session>;     // picks up suspensions for free
    pub fn latest_session(&self) -> Result<Option<Session>>;
    // Source-compatible conveniences over the default session (created eagerly at build, as
    // before 0.16 — keeps `session_id()` infallible; `latest_session` documents the implication):
    pub fn session_id(&self) -> &str;
    pub async fn run(&self, input: &str) -> Result<TurnOutput>;
    // Documented escape hatches:
    pub fn event_store(&self) -> Arc<EventStore>;
    pub fn engine(&self) -> &Arc<FlowEngine>;
}

impl Session {
    pub fn id(&self) -> &str;
    // Turns:
    pub async fn send(&self, input: &str) -> Result<TurnOutput>;
    pub fn stream(&self, input: &str) -> TurnStream;             // live events + cancel
    pub async fn send_with(&self, input: &str, sink: &mut dyn AgentSink,
                           cancel: &CancellationToken) -> Result<TurnOutput>;
    // Flow-driven sessions (D-131):
    pub async fn start_flow(&self, flow: &DraftAst) -> Result<TurnOutput>;
    pub fn suspended(&self) -> Result<bool>;                     // resume = just send()
    // History / observability (EventStore projections):
    pub fn history(&self) -> Result<Vec<Message>>;
    pub fn turns(&self) -> Result<Vec<TurnSummary>>;
    pub fn run_trace(&self) -> Result<Vec<RunEvent>>;
    pub fn cost(&self, pricing: &PricingTable) -> Result<Vec<ModelCost>>;
    pub fn efficiency(&self) -> Result<Option<EfficiencySummary>>;
    // Flow-driven voice (D-132):
    pub async fn run_voice_flow(&self, provider: &dyn RealtimeProvider, config: RealtimeConfig,
                                flow: DraftAst, sink: &mut dyn VoiceSink,
                                cancel: &CancellationToken) -> Result<()>;
    // Time machine (persistent, cassette-recorded sessions):
    pub async fn replay(&self, turn: Option<usize>, sink: &mut dyn AgentSink) -> Result<ReplayReport>;
    pub async fn fork(&self, at_turn: usize) -> Result<Fork>;    // Fork::{inject, edit, diff}
}
```

`TurnStream` is a stream of owned `AgentEvent`s (an enum mirroring `AgentSink` **1:1**, marked
`#[non_exhaustive]`) plus `cancel()` and `finish() -> TurnOutput`, implemented with an internal
channel sink and a spawned `run_turn_cancellable`. `TurnOutput` gains `suspended: bool` and
`#[non_exhaustive]` (wave 2 fields, attribute lands in wave 1).

**Concurrency invariant:** one engine runs one turn at a time (the loop host's `set_turn`).
`Client` enforces it honestly with an internal async turn-guard acquired by
`send`/`stream`/`start_flow`; two `Session`s on one `Client` serialize. Multi-tenant embedders
build one `Client` per agent.

## Builder parity

`ClientBuilder` gains what `FlowClientBuilder` already proved out, plus the `AgentSpec`
pass-throughs the CLI uses: `approver(Arc<dyn Approver>)`, `register_op`, `register_pack`,
`tools(subset)`, `with_cognition`, `with_sub_agents(SubAgents)`, `groups`, `ambient_signals`,
`compact_threshold_chars`, `context_budget`, `from_spec(AgentSpec)` (full-control escape hatch),
`storage(Storage)` (also on `FlowClientBuilder`), and the fallible
`try_with_live_datasource(domain, backend)` integration seam. Shared builder state is factored into
a private `envelope.rs` so the two builders cannot drift. `FlowClient` additionally gains
`execute_with_sink`/`execute_streamed` (wave 4) and `ExecutionResult.usage` (wave 2 — requires
`flux-cognition` to surface per-call usage it drops today).

## Datasource split

The SDK preserves two contracts instead of forcing one abstraction over unlike storage models:

- Indexed knowledge is still an operation pack. A host builds a
  `flux_capabilities::DatasourceBackend`, ingests records, and uses
  `try_register_pack(|registry| try_register_datasource_ops(registry, backend))`. That contract owns
  a local snapshot and exposes search plus record-oriented retrieval.
- An async system of record is first-class on the conversational builder. A host implements the
  SDK-re-exported `LiveDatasource` and calls `try_with_live_datasource`; the builder atomically
  retains its generated `<domain>.list` / `<domain>.get` tools, per-domain `ToolGroup`, and ambient
  configured signal. Later `groups` or `ambient_signals` setters cannot tear that surface apart.

The live projection snapshots entity/filter/page metadata and declared network/connection access at
registration. Its one invocation contract supplies both whole-plan preview and dispatch with exact
`datasource.read(<domain>/<entity>)` plus backend resource requirements. Payload filters, cursors,
IDs, and weak references are not authority subjects. The lower-level registration function remains
available to non-SDK hosts; the SDK convenience adds composition, not a second dispatch path.

## Re-export rule

**If a type appears in any public SDK signature, the SDK re-exports it.** Grouped modules keep
the crate root focused: `flux_sdk::tools` (Tool, ToolContext, ToolResult, ToolRegistry, FnTool,
tool_fn, ToolSpec, Risk), `flux_sdk::approval` (Approver, ApprovalChoice, RiskApprover,
IntentSet), `flux_sdk::subagents` (SubAgents, SpawnLimits, Role, RoleRegistry, ProviderFactory,
SubAgentApprover), `flux_sdk::voice` (VoiceSink, VoiceReply, RealtimeProvider, RealtimeConfig),
`flux_sdk::observe` (Message, TurnSummary, RunEvent, ModelCost, EfficiencySummary, RunDiff,
Observation), and `flux_sdk::datasource` (LiveDatasource, LiveAccess, LiveDatasourceSurface, plus
the row/filter/page/reference contracts). Root-level exports include `Provider`, `AgentSink`,
`AgentSpec`, `Permissions`, `Usage`, `PricingTable`, `ContextBlock`, `CancellationToken`,
`EventStore`, and `ToolGroup`. `flux-spec`, `flux-datasource`, `flux-capabilities`, and `tokio` are
real (non-dev) dependencies.

## Feature-gated batteries (decision: opt-in features, default = none)

```toml
[features]
default = []
providers = ["dep:flux-providers"]   # re-export + from_spec("claude/sonnet") one-stop
pricing   = ["dep:flux-credentials"] # load_pricing_table() → Session::cost ergonomics
plugins   = ["dep:flux-plugin"]      # subprocess plugin tools into the same gated registry
```

The provider-agnostic stance stays literally true for the default build. The model-spec resolver
is **not** written in the SDK: wave 3 moves `parse_model_spec`/`build_provider` from
`crates/flux-cli/src/main.rs` down into a new `flux_providers::spec` module (L1 may depend on
L1 `flux-credentials` for the `claude`/`codex` token sources); the CLI delegates, byte-identical.
The SDK's `providers` feature is then a re-export plus `providers::from_spec(spec)`.

**Publish-order gotcha (wave 3):** `flux-sdk → flux-providers` (optional dep) flips the topo
order in `scripts/publish-crates-io.sh` — move `providers` BEFORE `sdk` in the list (crates.io
requires optional deps to be published; every closure member already is — only the order moves).

## Safety audit

No new dispatch path anywhere in this design. Every new door — streamed turns, flow turns, voice
flow turns, plugin tools, live datasource tools, forks — funnels into the one shared `Executor`
(authorization → approval → guarded IO). Registered custom tools land in the same registry the
envelope gates; an injected `Approver` tightens, never bypasses (deny rules still take
precedence). Live datasource plan preview and dispatch consume the same exact typed requirements;
the backend must still perform real IO through guarded host surfaces. Replay's scope guarantees
nothing executes during replay. The layering map holds: SDK is L6; `flux-datasource` L0,
`flux-providers`/`flux-credentials` L1, `flux-plugin` L4, and `flux-capabilities` L5.

## Out of scope

- **A2A serving** (`flux-server` routers) — embedding ≠ serving; L6 peer product.
- **A2A client** — expressible as a registered `Tool`; revisit on demand.
- **`flux-app` / `flux-channels`** — the whole-`.flux`-program hosts are peer products.
- **Raw loop-host internals** — `Client::engine()` is the documented advanced escape hatch.
- **Postgres convenience constructors** — reachable via `Storage::custom`.

## Waves

- **Wave 1 — table stakes (D-142…D-146):** Storage + `Session` handle; Client envelope parity
  (+ `envelope.rs`, flux-spec promoted); `AgentSink` re-export + `send_with`; `AgentEvent` +
  `TurnStream` + cancellation; re-export sweep + one-import doc-test. One MINOR release.
- **Wave 2 — differentiators (D-147…D-151):** `start_flow`/`suspended`/`TurnOutput.suspended`;
  Client sub-agents; groups/ambient/compaction knobs; `ExecutionResult.usage` (flux-cognition
  usage surfacing); Session projections + `pricing` feature. MINOR.
- **Wave 3 — providers, plugins, voice (D-152…D-155):** `flux_providers::spec` extraction;
  `providers` feature; `plugins` feature; `Session::run_voice_flow`.
- **Wave 4 — time machine + flow streaming (D-156…D-159):** `Session::replay`; `Session::fork`
  + diff; `FlowClient::execute_with_sink`/`execute_streamed`; indexed-datasource recipe doc.

Each story is independently shippable with a failing-first test; acceptance lives in the story
files. Website docs gain `sdk/sessions.md`, `sdk/streaming.md` (wave 1), `sdk/durable-flows.md`
(wave 2), `sdk/providers.md` (wave 3), `sdk/time-machine.md` (wave 4).
