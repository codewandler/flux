# Design: Flow-driven voice — the realtime driver defers to flow suspensions

**Status:** implemented (2026-07-11) · **Pillar:** Agent · **Layer:** L4 (flux-flow voice + engine) · **Story:** [D-132](../stories/D-132-voice-defers-to-flow-suspensions.md) · **Builds on:** [D-131](../stories/D-131-flow-driven-session-primitive.md) ([design](flow-driven-session.md))

## Why

D-131 gave the engine a **flow-driven session**: an authored flow runs to its first top-level
`await`, speaks its authored prompt, and resumes deterministically on each user turn — no planner in
the loop. D-132 extends that to the **realtime/voice** channel (ai-agent-platform R-20, voice half):
a phone/WebRTC caller hears the flow's authored prompts spoken (TTS), their replies resume the flow,
and the model does cognition **only** where the flow calls it (an `ai_segment`). Classic IVR
determinism, but authored as a Flux-Lang flow with the full safety envelope underneath.

**The seam already exists (test-only).** `VoiceSessionDriver::run_flow_turns` +
`VoiceTurnHandler` (`crates/flux-flow/src/voice/driver.rs:37,202`) were built as the Phase-2
"engine-owned-turns" spike: the realtime model is the acoustic front-end (STT in, TTS out) while a
flux-side `handler` owns each turn. But the handler is a scripted test stub, it only reacts to caller
input (never speaks first), and it returns a bare `String` (no completion/hangup signal). D-132 makes
it real by bridging `FlowEngine` (D-131) into that seam.

## Invariants (verify before ship)

1. **Zero planner on the deterministic path.** A flow-driven voice session speaks the flow's authored
   prompts and resumes on caller input with **no** provider/planner call for the skeleton — only an
   `ai_segment` may call the model. *Verify:* a two-`await` flow driven over the mock realtime session
   speaks the two authored prompts + the completion line with a planner call-count of `0`.
2. **One safety envelope, no bypass.** Ops a voice-driven flow dispatches traverse the **FlowEngine's**
   shared `Arc<Executor>` (D-131), exactly as a text turn — *not* the `VoiceSessionDriver`'s own
   executor (unused in flow mode). The approver applies identically.
3. **Barge-in is untouched.** A flow-authored prompt is just an active response; a caller `SpeechStarted`
   cancels it via the existing `run_flow_turns` barge-in path. No new cancellation logic.
4. **Completion is terminal.** When the flow completes (no re-suspension), the driver speaks the final
   line, signals the consumer (`VoiceSink::session_ended`), and ends the session loop (`session.close()`)
   — the voice analog of a text turn returning the flow result.
5. **Telemetry parity.** Turns + usage land in the event store exactly as text: the handler drives
   through `FlowEngine::start_flow_turn`/`run_turn`, which already `begin_turn`/`end_turn` +
   `record_call_usage`. *Verify:* a driven session records one turn per spoken prompt.

## Approach

The whole change is one crate, `flux-flow`, and needs no `realtime` feature (the voice module mocks
the seam; only a live OpenAI run needs the concrete provider).

### 1. Signal completion vs. continuation — `VoiceReply`

`VoiceTurnHandler::turn` returns a bare `String` today, so the driver can't tell "speak this and wait"
from "speak this and hang up." Introduce (`voice/driver.rs`):

```rust
pub enum VoiceReply {
    /// Speak this, then await the caller's next turn (a flow suspension / an ordinary reply).
    Continue(String),
    /// Speak this final line, then end the call (the flow completed).
    Complete(String),
}
```

### 2. Speak first + terminal — extend `VoiceTurnHandler` + the driver

```rust
pub trait VoiceTurnHandler: Send + Sync {
    /// The opening line spoken BEFORE any caller input — a flow-driven session runs its flow to the
    /// first `await` and speaks that authored prompt. Default: nothing (caller speaks first).
    async fn start(&self) -> Option<VoiceReply> { None }
    async fn turn(&self, user_text: &str) -> VoiceReply;   // was `-> String`
}
```

`run_flow_turns` (`driver.rs:202`) gains two edits, everything else (barge-in, audio, error) unchanged:
- On `RealtimeEvent::SessionReady` (today a no-op): `handler.start().await` → speak it.
- The `InputTranscriptDone` branch and the `SessionReady` branch both route the reply through one
  `speak_reply` helper: `Continue(t)` → `session.send_text(&t)`; `Complete(t)` → speak (if non-empty),
  `sink.session_ended(&t)`, then **break** the loop (→ the existing `session.close()`).

### 3. Consumer hangup hook — `VoiceSink::session_ended`

`voice/sink.rs`: add `fn session_ended(&mut self, _result: &str) {}` (default no-op, pre-1.0 additive,
matching the other callbacks) so a telephony consumer hangs up / hands off when the flow completes.

### 4. The bridge — `EngineVoiceHandler`

A `FlowEngine`-backed `VoiceTurnHandler` (`voice/driver.rs` or a small `voice/engine_handler.rs`):

```rust
pub struct EngineVoiceHandler {
    engine: Arc<FlowEngine>,
    session_id: String,
    flow: DraftAst,
}
```

- `start()`: `engine.start_flow_turn(&session_id, &flow, &mut PromptCapture)` → classify.
- `turn(text)`: `engine.run_turn(&session_id, text, &mut PromptCapture)` → classify. The engine's
  suspension-first branch routes to `resume_suspended` automatically (D-131), so the handler owns no
  suspension logic.
- `classify(spoken)`: `if engine.flow.has_suspension(&session_id) { Continue(spoken) } else {
  Complete(spoken) }`.

`PromptCapture` is a tiny `AgentSink` that accumulates `text_delta` into a `String` — the engine
surfaces the authored prompt via `sink.text_delta(&prompt)` (D-131), so the capture *is* the prompt.

### 5. Completion signal — `FlowStore::has_suspension`

`state.rs`: add `pub fn has_suspension(&self, session_id) -> Result<bool>` (a `SELECT EXISTS` peek —
does **not** consume, unlike `take_suspension`). This is how the handler distinguishes a re-suspended
flow (Continue) from a completed one (Complete) after a turn.

### 6. SDK front door — DEFERRED (follow-up)

`FlowClient::run_voice_session` (`flux-sdk/src/flow.rs:551`) is the model-owned entry. A
`run_voice_flow_session(flow)` variant would drive `run_flow_turns` with an `EngineVoiceHandler` — but
`FlowClient` holds only a bare in-memory `FlowStore` and **no `EventStore`**, while
`FlowEngine::assemble` requires one; assembling a full engine there is a larger change than the story
warrants. The production voice callers (flux-server `lib.rs:1296`, flux-agent `lib.rs:269`) already
hold a `FlowEngine`, so the **`pub` driver-level API (`run_flow_turns` + `EngineVoiceHandler`) is the
deliverable** — the same shape D-131's `start_flow_turn` takes (a public engine/driver API surfaces
wire, no SDK wrapper in the story). A `FlowClient` voice-flow convenience is a thin follow-up.

## Testing

- **Failing-first (invariant 1 + 4 + 5):** build a `FlowEngine` over a **call-counting** mock provider;
  a two-`await` flow (`echo "What day?" → await → echo "Which time?" → await → echo "Booked!"`) driven
  through `run_flow_turns` with an `EngineVoiceHandler` and a scripted `[SessionReady,
  InputTranscriptDone("friday"), InputTranscriptDone("noon")]`. Assert `log.spoken == ["What day?",
  "Which time?", "Booked!"]`, the planner call-count is `0`, the sink saw `session_ended`, and the
  event store recorded three turns.
- **Existing `flow_owns_two_voice_turns`** updated to the `VoiceReply::Continue` shape (behavior
  unchanged: two spoken replies).
- Gate: `cargo test -p codewandler-flux-flow` · clippy `-D warnings` · fmt · `flux-codegate`.

## Non-goals

- A CLI/TUI voice surface — D-132 is the driver + engine bridge; surfaces wire it via the SDK.
- Barge-in *mid-`ai_segment`* refinement (the segment runs to its bound before the next prompt) —
  the existing barge-in cancels the spoken response, which is sufficient for v1.
- Multi-party / handoff routing beyond the `session_ended` signal.
