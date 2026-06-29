# Design: realtime voice-to-voice as a first-class flux provider

**Status:** **implemented** — Phase 1 (provider primitive) + the Phase 2 engine-owned-turns spike landed
(story [D-06](../stories/D-06-realtime-voice-provider.md)) · **Layer:** L0 (`flux-core` vocabulary) + L1
seam (`flux-provider`) + L1 impl (`flux-providers::realtime`, feature-gated) + L3 driver
(`flux-flow::voice`) · **Owner:** Timo

## As built (deltas from the proposal below)

The proposal weighed two open forks; both resolved toward the user's standing "prefer single crate with
modules" preference and the crate-consolidation precedent:

- **Modules, not new crates** (resolves the "crate vs. module" fork). The OpenAI impl is
  `crates/flux-providers/src/realtime/` behind a **`realtime` Cargo feature** (so the default HTTP build
  stays lean; the WS deps — `tokio-tungstenite`/`tokio-util`/`base64` — are optional). The driver is
  `crates/flux-flow/src/voice/`. **Zero new crates**, so `flux-codegate::layer()` and workspace members are
  untouched — layering stays green by construction.
- **`RealtimeProvider`** name kept (the other fork).
- **`send_tool_result(call_id, output)`** — dropped the proposal's `is_error` arg (OpenAI's
  `function_call_output` has no error flag; the driver folds error-ness into the output text).
- **Added `RealtimeEvent::InputTranscriptDone`** (full caller transcript) — the engine-owned-turns spike
  consumes it as one user turn.
- **Phase 2 spike** = `VoiceSessionDriver::run_flow_turns` + a `VoiceTurnHandler` seam (a flux-side decider
  that, in production, wraps `FlowEngine::run_turn`); proven by `flow_owns_two_voice_turns` against a mock.
  No engine-suspension work — per-turn `run_turn`, not cross-turn `await` (still future).
- **SDK seam** = `FlowClient::run_voice_session(provider, config, sink, cancel)` (mirrors `with_sub_agents`).
- **`ContentBlock::Audio`** stayed deferred (not needed by the voice hot path).
- managed-agents rewiring is a **separate pass in that repo** (the "Changed (managed-agents consumer)" list is the
  guide).

## Why

The downstream managed-agents service runs a **voice-to-voice** model (OpenAI Realtime) to power phone agents
over the RTVBP channel. Today that model runs **entirely outside flux**. managed-agents owns a hand-rolled
WebSocket client (`crates/realtime`), audio resampling (`crates/audio`), and the whole acoustic turn loop
(`channel-rtvbp/src/backend.rs`); flux is reached **only** behind the function-call seam — one-shot
`flux_sdk::FlowClient::execute` per tool call (the "behaviour runner"). That split is deliberate and
documented (managed-agents `docs/designs/behaviour-runner.md`: *"realtime drives voice; the behaviour runner
drives logic"*), but it costs:

- A **parallel model stack** flux would otherwise own — WS client, GA event parsing, key handling — that
  gets **none** of flux's provider infrastructure (credential seam, model resolution, the `Executor`
  safety envelope, audit).
- Tools **declared twice** and hand-synced: once model-facing (`realtime::Tool` in
  `agent-core/src/spec.rs` / `build_flow_backend`) and once as flux ops (`register_op` in
  `agent-core/src/presets.rs`).
- The OpenAI key read in **three** places; the realtime **voice** model and the flow **cognition** model
  are two independent provider wirings ("must not be conflated").

The goal: make a voice-to-voice model a **first-class flux provider**, so its tool calls flow through the
**same `Executor::dispatch` envelope** and its tools are declared **once** from the live `ToolRegistry` —
giving voice agents the same permission / approval / redaction / evidence guarantees as text agents, and
letting managed-agents delete the parallel stack.

## The crux: half-duplex vs. full-duplex

flux's model seam is **half-duplex**. From [`crates/flux-provider/src/lib.rs`](../../crates/flux-provider/src/lib.rs):

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    async fn stream(&self, req: Request) -> Result<ChunkStream>;   // build one Request → consume one stream
}
```

`Request` (`:58`) carries `model / system / messages / tools / max_tokens / temperature / … / metadata` —
all **text**. The turn loop calls it once per planner step deep inside
[`flux-flow/src/compile.rs`](../../crates/flux-flow/src/compile.rs) (`stream_blocks` → `provider.stream`),
the model emits a **plan** (`emit_plan`), and the deterministic engine runs it.

A voice-to-voice model is **full-duplex and session-oriented**: you open a long-lived connection and then
*concurrently* push input audio and pull output audio + transcript deltas + tool calls until hangup, with
**the model** — not flux — driving acoustic turn-taking (server-side VAD), barge-in, and response
cancellation. This does not fit `stream(Request) -> ChunkStream`. The two are different enough that they
must be **sibling abstractions**, not one trait. Crucially, the *valuable* seam — routing the model's
function calls through flux's `Executor` envelope with a single tool declaration — is fully achievable
across that sibling.

## Current surface (what we build on)

flux, verified:
- `Provider` / `Request` / `Chunk` / `ToolDef { name, description, input_schema }`
  ([`flux-provider/src/lib.rs`](../../crates/flux-provider/src/lib.rs) `:51`, `:58`, trait `:125`);
  `TokenSource` trait (`:181`) — the reusable auth seam.
- `ContentBlock` enum (Text / Thinking / RedactedThinking / ToolUse / ToolResult / **Image{source}**) —
  serde internally-tagged on `type`, snake_case
  ([`flux-core/src/content.rs`](../../crates/flux-core/src/content.rs)); `AudioSource` would mirror
  `ImageSource`.
- `ToolSpec { name, description, input_schema, output_schema, effects, risk, idempotency }`
  ([`flux-spec/src/lib.rs`](../../crates/flux-spec/src/lib.rs) `:59`); `ToolRegistry::specs() ->
  Vec<ToolSpec>` ([`flux-runtime/src/lib.rs`](../../crates/flux-runtime/src/lib.rs) `:288`).
- `Executor::dispatch(name, params) -> ToolResult` ([`flux-runtime/src/lib.rs`](../../crates/flux-runtime/src/lib.rs)
  `:778`) — the **6-stage envelope** (pre-tool hooks → permission rules → evidence + destructive markers →
  approval gate → guarded IO → redaction). **No bypass path.**
- `AgentSink` (text_delta / thinking_delta / tool_call / tool_result / observation / turn_end)
  ([`flux-flow/src/agent_sink.rs`](../../crates/flux-flow/src/agent_sink.rs)) — the output-sink pattern the
  voice sink mirrors.
- Layering, from [`crates/flux-codegate/src/lib.rs`](../../crates/flux-codegate/src/lib.rs) `layer()`:
  **L0** core/spec/secret/policy/…; **L1** `flux-provider` / `flux-providers` / `flux-credentials` /
  `flux-a2a`; **L2** `flux-system` / `flux-runtime` / `flux-tools` / `flux-events`; **L3** `flux-agent` /
  `flux-orchestrate` / `flux-flow` / `flux-eval` / `flux-cognition`.

managed-agents stack to lift/generalize (outside this repo, `downstream-managed-services`):
- `crates/realtime/src/{lib,event,config}.rs` — the hand-rolled OpenAI Realtime WS client
  (`RealtimeHandle` outbound; `mpsc::Receiver<ServerEvent>` inbound; `SessionConfig::to_ga_session()`,
  GA shape, server-VAD, `Tool::function(...)`). Targets **GA** (beta header rejected).
- `crates/audio/src/lib.rs` — PCM16 8 kHz ⇄ 24 kHz stateful resampling.
- `crates/channel-rtvbp/src/backend.rs` — `RealtimeBackend` wiring audio both ways and, on
  `ServerEvent::FunctionCall`, calling `agent_core::ToolDispatcher::dispatch` (off-thread) =
  `FlowRunner` = one-shot `FlowClient::execute`. Barge-in: `SpeechStarted` → guarded `cancel_response`.

## The design

A **sibling, session-oriented seam** that coexists with `Provider`, layered so `flux-codegate` stays green.

### L0 — audio vocabulary (`crates/flux-core/src/audio.rs`)

Pure value types, exactly the role `ImageSource` plays today (no IO, no deps beyond L0):

```rust
pub enum AudioEncoding { Pcm16, G711Ulaw, G711Alaw, Opus }

pub struct AudioFormat { pub encoding: AudioEncoding, pub sample_rate: u32, pub channels: u8 }
impl AudioFormat {
    pub const OPENAI_PCM16:   Self = /* Pcm16, 24_000, 1 */;
    pub const TELEPHONY_ULAW: Self = /* G711Ulaw, 8_000, 1 */;
}
```

`ContentBlock::Audio { source: AudioSource }` (with `AudioSource { Base64{media_type,data} | Url{url} }`)
is **specified but deferred to Phase 2** — adding a variant to the serde-tagged `ContentBlock` forces a
match arm in every half-duplex provider codec, so it stays off the voice critical path. The realtime
session never emits `ContentBlock`s for audio frames anyway: frames are a **transport** concern; the
**transcripts** are the textual record that lands in history.

### L1 — the session seam (`crates/flux-provider/src/realtime.rs`)

Sibling of `Provider`. Lives at L1 (it is the provider seam, and `RealtimeConfig` references L1 `ToolDef`):

```rust
#[async_trait]
pub trait RealtimeProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn connect(&self, config: RealtimeConfig) -> Result<RealtimeConnection>;
}

pub struct RealtimeConnection {
    pub session: Arc<dyn RealtimeSession>,   // input/control half — Arc so it clones into the off-loop task
    pub events:  RealtimeEventStream,        // output half
}
pub type RealtimeEventStream = Pin<Box<dyn Stream<Item = Result<RealtimeEvent>> + Send>>;

#[async_trait]
pub trait RealtimeSession: Send + Sync {
    async fn send_audio(&self, frame: &[u8]) -> Result<()>;   // model-native bytes; impl owns base64
    async fn commit_audio(&self) -> Result<()>;               // no-op under server VAD
    async fn send_text(&self, text: &str) -> Result<()>;      // DTMF-as-text / tests
    async fn create_response(&self) -> Result<()>;
    async fn cancel_response(&self) -> Result<()>;            // barge-in — idempotent (see Risks)
    async fn send_tool_result(&self, call_id: &str, output: &str, is_error: bool) -> Result<()>;
    fn close(&self);
}

pub enum RealtimeEvent {
    SessionReady,
    AudioDelta(Vec<u8>),               // DECODED model-native frames — base64 stays a wire detail
    OutputTranscriptDelta(String),
    InputTranscriptDelta(String),
    TextDelta(String),
    SpeechStarted, SpeechStopped,      // caller VAD → barge-in signal
    ResponseStarted,
    ToolCall { call_id: String, name: String, arguments: String },  // plain strings — never an L2 type
    ResponseDone,
    Error { code: Option<String>, message: String },
}

pub struct RealtimeConfig {
    pub model: String,
    pub system: Option<String>,
    pub tools: Vec<ToolDef>,           // ← single source of truth (built from the live ToolRegistry)
    pub voice: Option<String>,
    pub input_format:  AudioFormat,
    pub output_format: AudioFormat,
    pub turn_detection: TurnDetection,
    pub temperature: Option<f32>,
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

pub enum TurnDetection {
    ServerVad { threshold: Option<f32>, prefix_padding_ms: Option<u32>, silence_duration_ms: Option<u32> },
    SemanticVad { eagerness: Option<String> },
    None,   // client delimits turns via commit_audio + create_response
}
```

Two boundary decisions are baked into the trait: **(a)** `AudioDelta`/`send_audio` carry **decoded
bytes**, not base64 — encoding is the impl's wire detail (today managed-agents leaks it as
`ServerEvent::AudioDelta(String)`); **(b)** `ToolCall` carries only strings, so the L1 enum never names an
L2 runtime type. Both keep the layering honest.

### L1 — the OpenAI Realtime impl (new crate `flux-realtime`)

The concrete provider, **lifted from managed-agents `crates/realtime`** (`lib`/`event`/`config`). It is a
persistent **WebSocket** (tokio-tungstenite) with a writer task + reader task; it does **not** compose with
the HTTP `NativeProvider = WireCodec × Credential` pipeline (`flux-providers`' identity), so it is a
**sibling L1 crate**, not a `flux-providers` module — keeping that crate's "HTTP family" identity crisp and
isolating the tungstenite dep.

- Auth: follow `OpenAiCred`'s pattern (a **private** `enum Secret { ApiKey(String) | OAuth(Arc<dyn
  TokenSource>) }`, exactly as `flux-providers/src/openai.rs:491` does), reusing the shared
  `flux_provider::TokenSource` trait. `Credential::apply` is **reqwest-bound** and useless against a
  tungstenite handshake, so the `Authorization: Bearer …` header is set on the WS request directly. (`Secret`
  is not a shared type today — it's per-provider — so we copy the pattern, not the type.)
- GA endpoint `wss://api.openai.com/v1/realtime?model=…`, **no** `OpenAI-Beta` header; parse both GA and
  beta event names defensively (as the lifted code already does).
- A single `openai_realtime(secret)` constructor consolidates managed-agents' three scattered key reads.
- The benign `response_cancel_not_active` race is swallowed **here**, so the trait can promise
  `cancel_response` is idempotent and the L3 driver stays clean.

### L3 — the voice session driver (new crate `flux-voice`)

The only layer that may legally see both the L1 session and the L2 `Executor` (L3 ≥ max(L1, L2)). It owns
the event loop, the output sink, and the tool-routing glue.

**Single declaration** — `ToolDef`s from the live registry (kills the double-declaration):

```rust
/// Build the session's function declarations from the same specs the Executor gates.
pub fn tool_defs_from_registry(registry: &ToolRegistry) -> Vec<ToolDef> {
    registry.specs().into_iter()                        // Vec<ToolSpec> (flux-spec, L0)
        .map(|s| ToolDef { name: s.name, description: s.description, input_schema: s.input_schema })
        .collect()                                      // drop output_schema/effects/risk — the model doesn't need them
}
```

`RealtimeConfig.tools = tool_defs_from_registry(executor.registry())` — one source of truth. (Today
managed-agents builds `realtime::Tool` in `spec.rs` *and* registers ops in `presets.rs`, hand-synced.)

**Off-loop dispatch through the full envelope** — `VoiceSessionDriver::run` is a `tokio::select!` loop that
keeps the audio arm hot and, on a `ToolCall`, **spawns** the dispatch so a slow tool never stalls audio or
barge-in:

```rust
pub struct VoiceSessionDriver { executor: Arc<Executor> }

// inside run(conn, sink, cancel), on RealtimeEvent::ToolCall { call_id, name, arguments }:
let params: Value = serde_json::from_str(&arguments).unwrap_or(Value::Null);
sink.tool_call(&name, &params);
let (exec, session, done_tx) = (self.executor.clone(), session.clone(), done_tx.clone());
tokio::spawn(async move {
    let result = exec.dispatch(&name, params).await;        // ← full L2 safety envelope, no bypass
    let _ = session.send_tool_result(&call_id, &render(&result), result.is_error).await;
    let _ = session.create_response().await;                // continue the turn (debounced — see Risks)
    let _ = done_tx.send((name, result)).await;             // back to the loop for sink/audit only
});
```

`Executor::dispatch` is `&self` async behind an `Arc`, so cloning into the task is free and the full
6-stage envelope runs on **every** voice tool call — identical to text agents. This mirrors managed-agents'
existing off-thread dispatch, but lands on the real envelope instead of one-shot `FlowClient::execute`.

**The sink** — a symmetric cousin of `AgentSink`:

```rust
pub trait VoiceSink: Send {
    fn audio(&mut self, _frame: &[u8]) {}              // model-native output frames
    fn output_transcript(&mut self, _t: &str) {}
    fn input_transcript(&mut self, _t: &str) {}
    fn barge_in(&mut self) {}                           // flush playout
    fn tool_call(&mut self, _name: &str, _input: &Value) {}
    fn tool_result(&mut self, _name: &str, _r: &ToolResult) {}   // L2 ToolResult — fine at L3
    fn response_done(&mut self) {}
    fn error(&mut self, _msg: &str) {}
}
```

managed-agents' RTVBP channel implements `VoiceSink` (or a thin mpsc adapter to its existing `BotEvent`s) and
pushes caller audio through the `Arc<dyn RealtimeSession>` it also holds — mirroring today's
`(BackendHandle, Receiver<BotEvent>)` split.

### Audio-format / transport boundary (decided)

The flux provider speaks **model-native format only**; telephony/WebRTC resampling stays in the
consumer/channel — RTVBP=8 kHz, WebRTC=48 kHz, local mic=16 kHz all differ and none is flux's business.
`RealtimeConfig.input_format/output_format` is the seam: the consumer declares what it sends/receives and
the impl tags the wire. **Sharpener:** OpenAI Realtime accepts `g711_ulaw` natively, so the RTVBP 8 kHz
path can set `input/output_format = TELEPHONY_ULAW` and let the model resample **server-side** — the
channel then needs **zero** client resampling and `crates/audio` becomes optional. `crates/audio` **stays
in managed-agents**; if flux ever grows a second in-tree voice consumer, lift it as a pure `flux-audio` utility
(L0/L1) offered à la carte — never wired into the provider or driver.

## Reconciliation with "the LLM is not the runtime"

In the half-duplex loop the model emits a **plan** the engine executes (propose/dispose). The realtime
path drops that indirection by necessity — sub-second acoustic turn-taking can't round-trip a flux planner
per utterance. The clean split:

- **Realtime model owns:** acoustic turns (VAD, when to speak, barge-in timing), *which* function to call,
  the spoken words.
- **flux owns:** the tool *implementations*, the **safety/audit envelope** (`Executor::dispatch`), the
  evidence trail, and the **single** tool-declaration source.

The invariant that matters — *every effectful action crosses the guarded envelope* — is **preserved**.
What Phase 1 gives up is planning/DAG indirection and cross-turn control-flow ownership; that trade is
correct for a latency-bound voice surface.

## Staging

**Phase 1 — the provider primitive (the model becomes a flux provider):**
- L0 `AudioFormat`/`AudioEncoding` (`ContentBlock::Audio` deferred).
- L1 `flux-provider::realtime` seam.
- L1 `flux-realtime` OpenAI impl (lifted; decoded-bytes boundary; idempotent cancel; debounced
  `create_response`; one `openai_realtime` constructor).
- L3 `flux-voice` `VoiceSessionDriver` + `VoiceSink` + `tool_defs_from_registry`.
- managed-agents keeps `crates/audio` + the RTVBP transport, **rewires** `RealtimeBackend` onto
  `flux-voice`/`flux-realtime`, and **deletes** the model-facing `Tool` list in `spec.rs` in favour of
  `tool_defs_from_registry`.

**Phase 2 — engine-owned voice turns (spike):**
- A `VoiceSessionDriver` mode where turn boundaries + `ToolCall`s drive a **suspendable `FlowEngine`
  flow** instead of dispatching ops directly — a flux-lang flow owns the conversation across turns. This
  is the deferred cross-turn `await` from managed-agents' `behaviour-runner.md`; it needs flux-lang top-level
  `await` / cross-turn suspension, which one-shot `FlowClient::execute` cannot do today. The spike proves
  the seam (drive a 2-turn scripted flow against a mock `RealtimeProvider`) without committing the engine
  work.
- `ContentBlock::Audio` if a turn's audio must be persisted / fed to a half-duplex multimodal model.

## Crate / layer map

| Piece | Crate | Layer |
|---|---|---|
| `AudioFormat` / `AudioEncoding` / `AudioSource` | `flux-core` (new `audio.rs`) | **L0** |
| `RealtimeProvider` / `RealtimeSession` / `RealtimeEvent` / `RealtimeConfig` / `TurnDetection` | `flux-provider` (new `realtime.rs`) | **L1** |
| OpenAI Realtime impl (lifted from managed-agents `crates/realtime`) | `flux-providers::realtime` (feature `realtime`) | **L1** |
| `VoiceSessionDriver` / `VoiceSink` / `VoiceTurnHandler` / `tool_defs_from_registry` | `flux-flow::voice` | **L3** |

> **As built:** modules in existing L1/L3 crates (not new crates) — so `flux-codegate::layer()` and
> workspace members are unchanged. The proposal's new-crate variant is below for the record.

## Risks / open forks / known limitations

- **Layering pitfall (the load-bearing one):** `flux-provider` (L1) must **never** reach `Executor` (L2).
  `RealtimeEvent::ToolCall` is plain strings; the L3 `flux-voice` driver is the *only* place the L1 session
  and the L2 executor meet. `VoiceSink::tool_result(&ToolResult)` pulls L2 `ToolResult` into `flux-voice`
  (fine at L3) but must **not** leak into the L1 trait.
- **`cancel_response` idempotency:** GA server-VAD auto-cancels then rejects an explicit cancel with
  `response_cancel_not_active`. The benign-race handling lives in `flux-realtime` so the trait can promise
  idempotency.
- **Multiple tool calls per response (latent bug the abstraction fixes):** managed-agents calls
  `create_response()` once **per** function call, which can fire conflicting responses when a turn emits
  several. The driver should **debounce** to one `create_response` per `ResponseDone`, sending all
  `send_tool_result`s first. Open question: detect "all tool calls for this response are in" by counting
  `response.output_item.done` vs `response.done`.
- **Backpressure:** audio is high-rate; bounded mpsc both directions with an explicit drop/flush on
  barge-in; the `select!` audio arm must stay hot (hence off-loop dispatch).
- **`ContentBlock::Audio` is a breaking enum widening** — matched in every provider codec; Phase 2, with a
  codec audit.
- **Open fork — trait name:** `RealtimeProvider` (tracks the OpenAI brand, recognizable) vs.
  model-agnostic `DuplexProvider`/`LiveProvider` (Gemini Live etc. would also implement it). Recommend
  shipping `RealtimeProvider` and aliasing later if a second impl lands.
- **Open fork — crate vs. module:** separate `flux-realtime` crate (recommended; isolates WS deps, keeps
  `flux-providers` HTTP-only) vs. a `flux-providers/src/realtime/openai.rs` module (fewer crates, same
  layer/correctness, muddier identity).
- **Open fork — D-06 rank:** placement in the Downstream-enablement track is the product owner's call.

## New / changed files (as built)

**New (flux):**
- `crates/flux-core/src/audio.rs` — L0 — `AudioFormat`/`AudioEncoding`
- `crates/flux-provider/src/realtime.rs` — L1 — traits + `RealtimeConfig` + `RealtimeEvent`
- `crates/flux-providers/src/realtime/{mod,client,config,event}.rs` — L1 — OpenAI Realtime impl (lifted),
  feature `realtime`
- `crates/flux-flow/src/voice/{mod,driver,sink,tests}.rs` — L3 — `VoiceSessionDriver`, `VoiceSink`,
  `VoiceTurnHandler`, `tool_defs_from_registry`

**Changed (flux):**
- `crates/flux-core/src/lib.rs` — re-export `audio`
- `crates/flux-provider/src/lib.rs` — `pub mod realtime;`
- `crates/flux-providers/src/lib.rs` + `Cargo.toml` — feature-gated `realtime` module + optional WS deps
- `crates/flux-flow/src/lib.rs` — `pub mod voice;`
- `crates/flux-sdk/src/flow.rs` + `Cargo.toml` — `FlowClient::run_voice_session` + `tokio-util` dep
- root `Cargo.toml` — add `tokio-tungstenite` to `[workspace.dependencies]`
- (no `flux-codegate` / workspace-member change — modules, not crates)

**Changed (managed-agents consumer, outside this repo — separate follow-up pass):**
- `crates/channel-rtvbp/src/backend.rs` — consume `flux_flow::voice` + `flux_providers::realtime`
  (`flux-providers` with the `realtime` feature); resampling stays here (or drop it via `g711`)
- `crates/agent-core/src/spec.rs` — delete the model-facing `Tool` list; source from
  `flux_flow::tool_defs_from_registry`
- `crates/agent-core/src/{flow.rs,dispatch.rs}` — retire/adapt `ToolDispatcher` onto the
  `flux_flow::VoiceSessionDriver`

## Test plan (for the implementation pass — out of scope this pass)

- **Hermetic, no API key.** A mock `RealtimeProvider` whose `events` stream is scripted; `flux-voice`
  tests assert:
  - `tool_call_routes_through_executor` — a scripted `ToolCall` reaches `Executor::dispatch`, the result
    is fed back via `send_tool_result` + `create_response`, and a denied/destructive op is gated by the
    envelope (proving no bypass).
  - `tools_declared_once` — `tool_defs_from_registry` yields the registry's specs; the session config
    carries exactly those.
  - `barge_in_cancel_is_idempotent` — `SpeechStarted` with no active response is `Ok` and emits
    `barge_in()`.
  - `create_response_debounced` — a turn emitting two tool calls fires exactly one `create_response`.
- **Phase 2 spike:** a 2-turn scripted flow drives a mock session, proving the suspendable-flow seam
  without the engine work.
- **Gate:** `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`
  (the new crates' layer classification).

## Notes

- **Reuse, don't reinvent:** lifts managed-agents' proven `crates/realtime` (WS client, GA event model) and
  `RealtimeBackend` patterns; reuses `flux_provider::{TokenSource, ToolDef}`, `flux_spec::ToolSpec`,
  `flux_runtime::{Executor, ToolRegistry, ToolResult}`, and mirrors `AgentSink`. No redesign of `Provider`,
  the engine, or the safety envelope.
- **Couples with** [D-01](../stories/D-01-flow-input-seeding.md) (the behaviour-runner flow a Phase-2
  voice flow would run on) and [D-02](../stories/D-02-tenant-event-substrate.md) (voice tool calls become
  audited events once D-02 tags the substrate). Serves managed-agents **R-03**-adjacent voice work and the
  managed-agents voice surface.
- **Non-goals:** a new sandbox boundary; in-tree audio resampling; remote/distributed realtime; replacing
  the half-duplex `Provider` (the two coexist).
