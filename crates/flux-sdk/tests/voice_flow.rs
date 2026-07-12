//! D-155: `Session::run_voice_flow` — a flow-driven full-duplex voice session. An SDK-level port of
//! the engine driver's mock-realtime test: an authored two-`await` flow speaks first at
//! `SessionReady`, each caller turn resumes the suspension, and flow completion hangs up via
//! `VoiceSink::session_ended`. No planner runs (echo + await only).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_core::Result;
use flux_lang::ast::{DraftAst, Node, SymbolName};
use flux_provider::{
    ChunkStream, Provider, RealtimeConnection, RealtimeEvent, RealtimeEventStream, RealtimeSession,
    Request,
};
use flux_sdk::tools::{Tool, ToolContext, ToolResult, ToolSpec};
use flux_sdk::voice::{RealtimeConfig, RealtimeProvider, VoiceSink};
use flux_sdk::{CancellationToken, Client};
use futures::StreamExt;
use serde_json::{json, Value};

/// The session's text provider — never called (the flow has no `ai_segment`); panic if it is.
struct NeverProvider;
#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        panic!("a deterministic flow-driven voice session must not invoke the planner");
    }
}

/// The flow's authored-prompt op: echoes `text` back as its view (the spoken prompt).
struct EchoTool;
#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only("echo", "echo text", json!({"type": "object"}))
    }
    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            params["text"].as_str().unwrap_or("").to_string(),
        ))
    }
}

/// Records the text the flow spoke over the realtime session (`send_text`).
#[derive(Default)]
struct SessionLog {
    spoken: Vec<String>,
}

struct MockSession {
    log: Arc<Mutex<SessionLog>>,
}
#[async_trait]
impl RealtimeSession for MockSession {
    async fn send_audio(&self, _frame: &[u8]) -> Result<()> {
        Ok(())
    }
    async fn commit_audio(&self) -> Result<()> {
        Ok(())
    }
    async fn send_text(&self, text: &str) -> Result<()> {
        self.log.lock().unwrap().spoken.push(text.to_string());
        Ok(())
    }
    async fn create_response(&self) -> Result<()> {
        Ok(())
    }
    async fn cancel_response(&self) -> Result<()> {
        Ok(())
    }
    async fn send_tool_result(&self, _call_id: &str, _output: &str) -> Result<()> {
        Ok(())
    }
    fn close(&self) {}
}

/// A scripted realtime provider: speak-first (`SessionReady`) then two caller utterances.
struct FlowMockRealtime {
    log: Arc<Mutex<SessionLog>>,
}
#[async_trait]
impl RealtimeProvider for FlowMockRealtime {
    fn name(&self) -> &str {
        "mock-realtime"
    }
    async fn connect(&self, _config: RealtimeConfig) -> Result<RealtimeConnection> {
        let evs = vec![
            RealtimeEvent::SessionReady, // speak-first → "What day?"
            RealtimeEvent::InputTranscriptDone("friday".into()), // resume → "Which time?"
            RealtimeEvent::InputTranscriptDone("noon".into()), // resume → complete → "Booked!" + hangup
        ];
        // Yield the scripted events, then pend forever (a real WS stays open until cancel).
        let head = futures::stream::iter(evs.into_iter().map(Ok::<_, flux_core::Error>));
        let events: RealtimeEventStream =
            Box::pin(head.chain(futures::stream::pending::<Result<RealtimeEvent>>()));
        Ok(RealtimeConnection {
            session: Arc::new(MockSession {
                log: self.log.clone(),
            }),
            events,
        })
    }
}

/// Records the flow-driven session's terminal hangup hook.
#[derive(Default)]
struct EndSink {
    ended: Option<String>,
}
impl VoiceSink for EndSink {
    fn session_ended(&mut self, result: &str) {
        self.ended = Some(result.to_string());
    }
}

async fn wait_until(pred: impl Fn() -> bool) {
    for _ in 0..2000 {
        if pred() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    panic!("condition was never met");
}

#[tokio::test]
async fn run_voice_flow_speaks_authored_prompts_resumes_and_hangs_up() {
    let dir = std::env::temp_dir().join(format!("flux-sdk-voiceflow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .register_op(Arc::new(EchoTool))
        .build(Box::new(NeverProvider), &dir)
        .unwrap();
    let session = client.create_session().unwrap();

    // A two-`await` interview flow: it speaks first, resumes on each caller turn, completes on the
    // second answer. `echo` emits each authored prompt; a lone object arg maps to its named input.
    let prompt = |t: &str| Node::Call {
        op: "echo".into(),
        args: vec![Node::Obj {
            fields: BTreeMap::from([("text".to_string(), Box::new(Node::Lit { value: json!(t) }))]),
        }],
    };
    let await_reply = |name: &str| Node::Await {
        binding: Some(SymbolName(name.into())),
        source: "user_input".into(),
        as_type: None,
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

    let log = Arc::new(Mutex::new(SessionLog::default()));
    let provider = FlowMockRealtime { log: log.clone() };
    let config = RealtimeConfig::voice_agent("mock", "be a booking agent");
    let cancel = CancellationToken::new();
    let mut sink = EndSink::default();

    // End the (otherwise open) session once all three authored prompts have been spoken.
    let controller = {
        let cancel = cancel.clone();
        let log = log.clone();
        async move {
            wait_until(move || log.lock().unwrap().spoken.len() == 3).await;
            cancel.cancel();
        }
    };
    let (result, _) = tokio::join!(
        session.run_voice_flow(&provider, config, flow, &mut sink, &cancel),
        controller,
    );
    result.expect("the voice flow ran");

    // The flow spoke its OWN authored prompts, in order — no model improvisation.
    assert_eq!(
        log.lock().unwrap().spoken,
        vec![
            "What day?".to_string(),
            "Which time?".to_string(),
            "Booked!".to_string()
        ]
    );
    // The flow completing fired the hangup hook with its final line.
    assert_eq!(sink.ended.as_deref(), Some("Booked!"));

    std::fs::remove_dir_all(&dir).ok();
}
