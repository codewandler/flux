//! Hermetic voice-driver tests — a mock realtime session, no API key, no network.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use flux_core::{canonical_model_spec, Usage};
use flux_events::{cost_summary, EventStore};
use flux_provider::{RealtimeConnection, RealtimeEvent, RealtimeEventStream, RealtimeSession};
use flux_runtime::{
    AllowApprover, Approver, DenyApprover, Executor, PermissionManager, Tool, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_spec::{Effect, Risk, ToolSpec};
use flux_system::{System, Workspace};

use super::{
    tool_defs_from_registry, EngineVoiceHandler, Speaker, UsageRecording, VoiceReply,
    VoiceSessionDriver, VoiceSink, VoiceTurnHandler, SOLE_SPEAKER_ID,
};
use crate::ast::{DraftAst, Node, SymbolName};
use crate::engine::FlowEngine;
use crate::state::FlowStore;
use flux_provider::{ChunkStream, Provider, Request};

// --- mock session --------------------------------------------------------------------------------

#[derive(Default)]
struct SessionLog {
    tool_results: Vec<(String, String)>, // (call_id, output)
    spoken: Vec<String>,                 // send_text replies (engine-owned-turns mode)
    create_responses: usize,
    cancels: usize,
}

struct MockSession {
    log: Arc<Mutex<SessionLog>>,
}

#[async_trait]
impl RealtimeSession for MockSession {
    async fn send_audio(&self, _frame: &[u8]) -> flux_core::Result<()> {
        Ok(())
    }
    async fn commit_audio(&self) -> flux_core::Result<()> {
        Ok(())
    }
    async fn send_text(&self, text: &str) -> flux_core::Result<()> {
        self.log.lock().unwrap().spoken.push(text.to_string());
        Ok(())
    }
    async fn create_response(&self) -> flux_core::Result<()> {
        self.log.lock().unwrap().create_responses += 1;
        Ok(())
    }
    async fn cancel_response(&self) -> flux_core::Result<()> {
        self.log.lock().unwrap().cancels += 1;
        Ok(())
    }
    async fn send_tool_result(&self, call_id: &str, output: &str) -> flux_core::Result<()> {
        self.log
            .lock()
            .unwrap()
            .tool_results
            .push((call_id.to_string(), output.to_string()));
        Ok(())
    }
    fn close(&self) {}
}

/// Yield the scripted events, then pend forever (a real WS stream stays open) so the driver keeps
/// processing tool completions until the test cancels it.
fn scripted(evs: Vec<RealtimeEvent>) -> RealtimeEventStream {
    let head = stream::iter(evs.into_iter().map(Ok::<RealtimeEvent, flux_core::Error>));
    Box::pin(head.chain(stream::pending::<flux_core::Result<RealtimeEvent>>()))
}

// --- mock sink -----------------------------------------------------------------------------------

#[derive(Default)]
struct CaptureSink {
    tool_calls: Vec<String>,
    tool_results: Vec<(String, bool)>, // (name, is_error)
    barge_ins: usize,
    audio_frames: usize,
    usages: Vec<Option<Usage>>,
}

impl VoiceSink for CaptureSink {
    fn audio(&mut self, _frame: &[u8]) {
        self.audio_frames += 1;
    }
    fn tool_call(&mut self, name: &str, _input: &Value) {
        self.tool_calls.push(name.to_string());
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        self.tool_results.push((name.to_string(), result.is_error));
    }
    fn barge_in(&mut self) {
        self.barge_ins += 1;
    }
    fn response_done(&mut self, usage: Option<&Usage>) {
        self.usages.push(usage.cloned());
    }
}

// --- tools ---------------------------------------------------------------------------------------

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only("echo", "echo text", json!({"type": "object"}))
    }
    async fn execute(&self, _ctx: &ToolContext, params: Value) -> flux_core::Result<ToolResult> {
        Ok(ToolResult::ok(
            params["text"].as_str().unwrap_or("").to_string(),
        ))
    }
}

static BOOMED: AtomicBool = AtomicBool::new(false);

struct BoomTool;

#[async_trait]
impl Tool for BoomTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only("boom", "destructive", json!({"type": "object"}))
            .with_effects(vec![Effect::Process])
            .with_access(vec![flux_spec::AccessKind::Process])
            .with_risk(Risk::Destructive)
    }
    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> flux_core::Result<ToolResult> {
        BOOMED.store(true, Ordering::SeqCst);
        Ok(ToolResult::ok("boomed"))
    }
}

// --- harness -------------------------------------------------------------------------------------

static DIRN: AtomicUsize = AtomicUsize::new(0);

fn executor(approver: Arc<dyn Approver>, registry: ToolRegistry) -> Arc<Executor> {
    let n = DIRN.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("flux-voice-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())));
    Arc::new(Executor::new(
        registry,
        PermissionManager::new(),
        approver,
        ctx,
    ))
}

fn registry(tool: Arc<dyn Tool>) -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(tool);
    r
}

/// Poll a predicate until true, with a generous bound, so tests don't hang on a logic bug.
async fn wait_until(pred: impl Fn() -> bool) {
    for _ in 0..400 {
        if pred() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("condition not met in time");
}

/// Drive the session concurrently with a controller that cancels once `ready` holds.
async fn drive(
    exec: Arc<Executor>,
    log: Arc<Mutex<SessionLog>>,
    events: RealtimeEventStream,
    sink: &mut CaptureSink,
    ready: impl Fn() -> bool,
) {
    let session: Arc<dyn RealtimeSession> = Arc::new(MockSession { log });
    let conn = RealtimeConnection { session, events };
    let cancel = CancellationToken::new();
    let driver = VoiceSessionDriver::new(exec);
    let controller = {
        let cancel = cancel.clone();
        async move {
            wait_until(ready).await;
            // small grace so a buggy second `create_response` would have fired before we assert
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            cancel.cancel();
        }
    };
    tokio::join!(driver.run(conn, sink, &cancel), controller);
}

// --- tests ---------------------------------------------------------------------------------------

#[tokio::test]
async fn tool_call_routes_through_executor() {
    let exec = executor(Arc::new(AllowApprover), registry(Arc::new(EchoTool)));
    let log = Arc::new(Mutex::new(SessionLog::default()));
    let events = scripted(vec![
        RealtimeEvent::ResponseStarted,
        RealtimeEvent::ToolCall {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: json!({"text": "hello"}).to_string(),
        },
        RealtimeEvent::ResponseDone {
            usage: Some(Usage {
                input_tokens: 100,
                output_tokens: 20,
                ..Default::default()
            }),
        },
    ]);
    let mut sink = CaptureSink::default();
    let log2 = log.clone();
    drive(exec, log.clone(), events, &mut sink, move || {
        log2.lock().unwrap().tool_results.len() == 1
    })
    .await;

    let log = log.lock().unwrap();
    // The model's tool call ran through Executor::dispatch and the echoed output went back.
    assert_eq!(
        log.tool_results,
        vec![("c1".to_string(), "hello".to_string())]
    );
    // The response's usage reached the sink via `response_done` (C-38).
    assert_eq!(
        sink.usages,
        vec![Some(Usage {
            input_tokens: 100,
            output_tokens: 20,
            ..Default::default()
        })]
    );
    assert_eq!(sink.tool_calls, vec!["echo".to_string()]);
    assert_eq!(sink.tool_results, vec![("echo".to_string(), false)]);
    // Continuation fired exactly once after the (single) tool call resolved.
    assert_eq!(log.create_responses, 1);
}

#[test]
fn tools_declared_once() {
    let reg = registry(Arc::new(EchoTool));
    let defs = tool_defs_from_registry(&reg);
    // Exactly the registry's specs become the model-facing declarations — declared once.
    assert_eq!(defs.len(), reg.specs().len());
    assert!(defs.iter().any(|d| d.name == "echo"));
}

#[tokio::test]
async fn barge_in_cancel_is_idempotent() {
    let exec = executor(Arc::new(AllowApprover), registry(Arc::new(EchoTool)));
    let log = Arc::new(Mutex::new(SessionLog::default()));
    let events = scripted(vec![
        RealtimeEvent::SpeechStarted, // no active response — must NOT cancel, must not error
        RealtimeEvent::ResponseStarted,
        RealtimeEvent::SpeechStarted, // active response — cancels once
    ]);
    let mut sink = CaptureSink::default();
    let log2 = log.clone();
    drive(exec, log.clone(), events, &mut sink, move || {
        log2.lock().unwrap().cancels == 1
    })
    .await;

    let log = log.lock().unwrap();
    assert_eq!(log.cancels, 1, "only the active response is cancelled");
    assert_eq!(sink.barge_ins, 2, "both barge-ins surface to the sink");
}

#[tokio::test]
async fn barge_in_disarms_pending_continuation() {
    // A barge-in mid-tool-call must NOT force a tool-driven continuation (else the model speaks over
    // the user). The tool result still flows back for history; there's just no forced `create_response`.
    let exec = executor(Arc::new(AllowApprover), registry(Arc::new(EchoTool)));
    let log = Arc::new(Mutex::new(SessionLog::default()));
    let events = scripted(vec![
        RealtimeEvent::ResponseStarted,
        RealtimeEvent::ToolCall {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: json!({"text": "x"}).to_string(),
        },
        RealtimeEvent::SpeechStarted, // user interrupts while the tool is running
        RealtimeEvent::ResponseDone { usage: None }, // the cancelled response completes
    ]);
    let mut sink = CaptureSink::default();
    let log2 = log.clone();
    drive(exec, log.clone(), events, &mut sink, move || {
        log2.lock().unwrap().tool_results.len() == 1
    })
    .await;

    let log = log.lock().unwrap();
    assert_eq!(
        log.tool_results.len(),
        1,
        "tool result still flows back for history"
    );
    assert_eq!(log.cancels, 1, "the active response was cancelled");
    assert_eq!(
        log.create_responses, 0,
        "barge-in disarmed the forced continuation"
    );
}

#[tokio::test]
async fn create_response_debounced() {
    let exec = executor(Arc::new(AllowApprover), registry(Arc::new(EchoTool)));
    let log = Arc::new(Mutex::new(SessionLog::default()));
    let events = scripted(vec![
        RealtimeEvent::ResponseStarted,
        RealtimeEvent::ToolCall {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: json!({"text": "a"}).to_string(),
        },
        RealtimeEvent::ToolCall {
            call_id: "c2".into(),
            name: "echo".into(),
            arguments: json!({"text": "b"}).to_string(),
        },
        RealtimeEvent::ResponseDone { usage: None },
    ]);
    let mut sink = CaptureSink::default();
    let log2 = log.clone();
    drive(exec, log.clone(), events, &mut sink, move || {
        log2.lock().unwrap().tool_results.len() == 2
    })
    .await;

    let log = log.lock().unwrap();
    assert_eq!(log.tool_results.len(), 2, "both tool calls were dispatched");
    // One `create_response` for the whole response, not one-per-call.
    assert_eq!(log.create_responses, 1);
}

#[tokio::test]
async fn denied_tool_is_gated() {
    BOOMED.store(false, Ordering::SeqCst);
    let exec = executor(Arc::new(DenyApprover), registry(Arc::new(BoomTool)));
    let log = Arc::new(Mutex::new(SessionLog::default()));
    let events = scripted(vec![
        RealtimeEvent::ResponseStarted,
        RealtimeEvent::ToolCall {
            call_id: "c1".into(),
            name: "boom".into(),
            arguments: json!({}).to_string(),
        },
        RealtimeEvent::ResponseDone { usage: None },
    ]);
    let mut sink = CaptureSink::default();
    let log2 = log.clone();
    drive(exec, log.clone(), events, &mut sink, move || {
        log2.lock().unwrap().tool_results.len() == 1
    })
    .await;

    // The envelope gated the destructive op: it never executed, and the model got an error result.
    assert!(
        !BOOMED.load(Ordering::SeqCst),
        "destructive op must not run"
    );
    assert_eq!(sink.tool_results, vec![("boom".to_string(), true)]);
}

// --- Phase 2: engine-owned turns -----------------------------------------------------------------

/// Stands in for a `FlowEngine`-backed handler: advances a scripted flow one reply per turn, and
/// records who the driver attributed each turn to (D-204).
struct ScriptHandler {
    replies: Vec<String>,
    n: AtomicUsize,
    speakers: Mutex<Vec<Speaker>>,
}

impl ScriptHandler {
    fn new(replies: Vec<String>) -> Self {
        Self {
            replies,
            n: AtomicUsize::new(0),
            speakers: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl VoiceTurnHandler for ScriptHandler {
    async fn turn(&self, speaker: &Speaker, _user_text: &str) -> VoiceReply {
        self.speakers.lock().unwrap().push(speaker.clone());
        let i = self.n.fetch_add(1, Ordering::SeqCst);
        VoiceReply::Continue(self.replies.get(i).cloned().unwrap_or_default())
    }
}

#[tokio::test]
async fn flow_owns_two_voice_turns() {
    // A flux-side handler (standing in for a FlowEngine flow) owns the conversation across two user
    // turns; the realtime model is the acoustic front-end (transcribe in, speak out).
    let exec = executor(Arc::new(AllowApprover), registry(Arc::new(EchoTool)));
    let log = Arc::new(Mutex::new(SessionLog::default()));
    let events = scripted(vec![
        RealtimeEvent::InputTranscriptDone("book a table".into()),
        RealtimeEvent::InputTranscriptDone("friday".into()),
    ]);
    let handler = ScriptHandler::new(vec!["what day?".into(), "booked for friday".into()]);
    let session: Arc<dyn RealtimeSession> = Arc::new(MockSession { log: log.clone() });
    let conn = RealtimeConnection { session, events };
    let cancel = CancellationToken::new();
    let mut sink = CaptureSink::default();
    let driver = VoiceSessionDriver::new(exec);

    let controller = {
        let cancel = cancel.clone();
        let log = log.clone();
        async move {
            wait_until(move || log.lock().unwrap().spoken.len() == 2).await;
            cancel.cancel();
        }
    };
    tokio::join!(
        driver.run_flow_turns(conn, &mut sink, &handler, &cancel),
        controller,
    );

    // The flow drove both turns: each user transcript produced the next scripted reply, in order.
    assert_eq!(
        log.lock().unwrap().spoken,
        vec!["what day?".to_string(), "booked for friday".to_string()]
    );
    // A realtime call is 1:1, so both turns are attributed to the sole caller (D-204) — the seam now
    // always names a speaker, and this surface's answer is "the only one there is".
    let speakers = handler.speakers.lock().unwrap();
    assert_eq!(speakers.len(), 2);
    assert!(
        speakers.iter().all(|s| s.id() == SOLE_SPEAKER_ID),
        "a phone line attributes every turn to the sole caller: {speakers:?}"
    );
}

// --- D-132: flow-driven voice (a FlowEngine owns the whole call) ---------------------------------

/// A provider that must NOT be called on the deterministic flow-driven path — counts calls so the
/// test can assert zero adaptive-stage invocations (D-132 invariant 1).
struct CountingProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for CountingProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> flux_core::Result<ChunkStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::empty()))
    }
}

/// A sink that records the flow-driven session's terminal hangup hook (`session_ended`).
#[derive(Default)]
struct EndSink {
    ended: Option<String>,
}

impl VoiceSink for EndSink {
    fn session_ended(&mut self, result: &str) {
        self.ended = Some(result.to_string());
    }
}

/// Build a `FlowEngine` over a never-called counting provider, with `echo` registered (the flow's
/// authored-prompt op). The shared `events` store lets the test read back recorded turns.
fn flow_engine(events: Arc<EventStore>, calls: Arc<AtomicUsize>) -> FlowEngine {
    let dir = std::env::temp_dir().join(format!("flux-voice-d132-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(EchoTool));
    flux_tools::register_reflect(&mut reg);
    flux_tools::register_evidence(&mut reg);
    let exec = Executor::new(
        reg,
        PermissionManager::from_rules(&["echo".into()], &[]),
        Arc::new(AllowApprover),
        ToolContext::new(system),
    );
    let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
    FlowEngine::assemble(
        Arc::new(CountingProvider { calls }),
        exec,
        events,
        flow,
        "mock".into(),
        "test".into(),
        1024,
        5,
        Vec::new(),
        0,
        Vec::new(),
        dir,
    )
    .unwrap()
}

#[tokio::test]
async fn flow_driven_voice_session_speaks_authored_prompts_and_hangs_up() {
    // A two-`await` flow owns the whole call: it speaks first, resumes on each caller turn, and ends
    // the call when it completes — with ZERO adaptive-stage invocations (echo + await only).
    let events = Arc::new(EventStore::in_memory().unwrap());
    let sid = events.create_session("mock").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = Arc::new(flow_engine(events.clone(), calls.clone()));
    let driver_exec = engine.executor.clone(); // unused in flow mode, but the driver still needs one

    // A lone object arg maps to the op's named input (the voice `EchoTool` has no positional schema).
    let prompt = |t: &str| Node::Call {
        op: "echo".into(),
        args: vec![Node::Obj {
            fields: std::collections::BTreeMap::from([(
                "text".to_string(),
                Box::new(Node::Lit { value: json!(t) }),
            )]),
        }],
    };
    let await_reply = |name: &str| Node::Await {
        binding: Some(SymbolName(name.into())),
        source: "user_input".into(),
        as_type: None,
        condition: None,
    };
    let flow = DraftAst {
        body: vec![
            prompt("What day?"),
            await_reply("day"),
            prompt("Which time?"),
            await_reply("time"),
            prompt("Booked!"),
        ],
        ..Default::default()
    };
    let handler = EngineVoiceHandler::new(engine, sid.clone(), flow);

    let log = Arc::new(Mutex::new(SessionLog::default()));
    let evs = scripted(vec![
        RealtimeEvent::SessionReady, // speak-first → "What day?"
        RealtimeEvent::InputTranscriptDone("friday".into()), // resume → "Which time?"
        RealtimeEvent::InputTranscriptDone("noon".into()), // resume → complete → "Booked!" + hangup
    ]);
    let session: Arc<dyn RealtimeSession> = Arc::new(MockSession { log: log.clone() });
    let conn = RealtimeConnection {
        session,
        events: evs,
    };
    let cancel = CancellationToken::new();
    let mut sink = EndSink::default();
    let driver = VoiceSessionDriver::new(driver_exec);

    let controller = {
        let cancel = cancel.clone();
        let log = log.clone();
        async move {
            wait_until(move || log.lock().unwrap().spoken.len() == 3).await;
            cancel.cancel();
        }
    };
    tokio::join!(
        driver.run_flow_turns(conn, &mut sink, &handler, &cancel),
        controller,
    );

    // The flow spoke its OWN authored prompts, in order, ending on completion — no model improvisation.
    assert_eq!(
        log.lock().unwrap().spoken,
        vec![
            "What day?".to_string(),
            "Which time?".to_string(),
            "Booked!".to_string()
        ]
    );
    // Invariant 1: the deterministic skeleton never called an adaptive model stage.
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "flow-driven voice invoked no adaptive model stage"
    );
    // Invariant 4: completion fired the terminal hangup hook with the final line.
    assert_eq!(sink.ended.as_deref(), Some("Booked!"));
    // Invariant 5 (telemetry parity): each spoken prompt is a recorded first-class turn.
    assert_eq!(
        events.turns(&sid).unwrap().len(),
        3,
        "one first-class turn per driven prompt"
    );
}

// --- C-38: usage recording -------------------------------------------------------------------------

/// End-to-end: a usage-bearing response appends exactly one `CallUsage` row stamped with the
/// canonical model spec, a zero-usage response appends none, and `cost_summary` prices the
/// resulting stream to the hand-computed dollar figure — proving the wire→driver→store→pricing
/// chain, not just one link of it.
#[tokio::test]
async fn usage_recording_appends_one_row_and_cost_summary_prices_it() {
    let exec = executor(Arc::new(AllowApprover), registry(Arc::new(EchoTool)));
    let log = Arc::new(Mutex::new(SessionLog::default()));
    let events = scripted(vec![
        RealtimeEvent::ResponseStarted,
        RealtimeEvent::ResponseDone {
            usage: Some(Usage {
                input_tokens: 1_000_000,
                output_tokens: 500_000,
                ..Default::default()
            }),
        },
        RealtimeEvent::ResponseStarted,
        RealtimeEvent::ResponseDone { usage: None }, // must not append a placeholder row
    ]);
    let session: Arc<dyn RealtimeSession> = Arc::new(MockSession { log });
    let conn = RealtimeConnection { session, events };
    let cancel = CancellationToken::new();
    let mut sink = CaptureSink::default();

    let store = Arc::new(EventStore::in_memory().unwrap());
    let session_id = "s_voice_usage_test".to_string();
    let model_spec = canonical_model_spec(Some("openai"), "gpt-realtime");
    let driver = VoiceSessionDriver::new(exec).with_usage_recording(UsageRecording {
        events: store.clone(),
        session_id: session_id.clone(),
        model_spec: model_spec.clone(),
    });

    let controller = {
        let cancel = cancel.clone();
        let store = store.clone();
        let session_id = session_id.clone();
        async move {
            wait_until(move || {
                store
                    .load_stream(&session_id, None)
                    .map(|evs| !evs.is_empty())
                    .unwrap_or(false)
            })
            .await;
            // Grace period so a hypothetical second (buggy) append from the zero-usage response
            // would have landed before we assert.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            cancel.cancel();
        }
    };
    tokio::join!(driver.run(conn, &mut sink, &cancel), controller);

    let recorded = store.load_stream(&session_id, None).unwrap();
    let usage_rows: Vec<_> = recorded
        .iter()
        .filter_map(|e| match &e.kind {
            flux_events::EventKind::CallUsage { model, usage } => {
                Some((model.clone(), usage.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        usage_rows.len(),
        1,
        "the zero-usage response must not append a row"
    );
    assert_eq!(usage_rows[0].0, model_spec);
    assert_eq!(usage_rows[0].1.input_tokens, 1_000_000);
    assert_eq!(usage_rows[0].1.output_tokens, 500_000);

    // The usage also reached the sink's `response_done` for BOTH responses (Some, then None) —
    // recording is additive to, not instead of, the existing sink callback.
    assert_eq!(sink.usages.len(), 2);
    assert!(sink.usages[0].is_some());
    assert!(sink.usages[1].is_none());

    // cost_summary prices the stream end-to-end: gpt-realtime bills $4.00/M input, $24.00/M output.
    let pricing = flux_core::PricingTable::builtin();
    let summary = cost_summary(&recorded, &pricing);
    assert_eq!(summary.len(), 1);
    let row = &summary[0];
    assert_eq!(row.model, model_spec);
    let money = row.cost.expect("gpt-realtime must price");
    assert!(
        (money.usd - 16.0).abs() < 1e-9,
        "1.0·4.0 + 0.5·24.0 = 16.0, got {}",
        money.usd
    );
}
