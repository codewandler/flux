//! Hermetic voice-driver tests — a mock realtime session, no API key, no network.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use flux_provider::{RealtimeConnection, RealtimeEvent, RealtimeEventStream, RealtimeSession};
use flux_runtime::{
    AllowApprover, Approver, DenyApprover, Executor, PermissionManager, Tool, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_spec::{Effect, Risk, ToolSpec};
use flux_system::{System, Workspace};

use super::{tool_defs_from_registry, VoiceSessionDriver, VoiceSink, VoiceTurnHandler};

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
            .with_risk(Risk::High)
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
        RealtimeEvent::ResponseDone,
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
    drive(exec, log.clone(), events, &mut sink, || true).await;

    let log = log.lock().unwrap();
    assert_eq!(log.cancels, 1, "only the active response is cancelled");
    assert_eq!(sink.barge_ins, 2, "both barge-ins surface to the sink");
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
        RealtimeEvent::ResponseDone,
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
        RealtimeEvent::ResponseDone,
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

/// Stands in for a `FlowEngine`-backed handler: advances a scripted flow one reply per turn.
struct ScriptHandler {
    replies: Vec<String>,
    n: AtomicUsize,
}

#[async_trait]
impl VoiceTurnHandler for ScriptHandler {
    async fn turn(&self, _user_text: &str) -> String {
        let i = self.n.fetch_add(1, Ordering::SeqCst);
        self.replies.get(i).cloned().unwrap_or_default()
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
    let handler = ScriptHandler {
        replies: vec!["what day?".into(), "booked for friday".into()],
        n: AtomicUsize::new(0),
    };
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
}
