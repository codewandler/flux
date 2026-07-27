//! D-178 (SDK slice): `Session::interrupted`/`Session::resurrect` and
//! `ClientBuilder::auto_resurrect` — finishing a turn a crash killed mid-execution, in place, with
//! zero model re-spend and no duplicate side effects for any op that got as far as recording a cell.
//!
//! A "crash" is seeded the same way the engine-level tests seed one (`flux_flow::resurrect`'s own
//! suite): an open turn (`TurnStarted`, an accepted `plan_source`, some completed statements, and
//! the cassette cells those statements' dispatches recorded) with **no** `TurnEnded`. That is
//! exactly the state a `kill -9` between two statements leaves behind, and it is reachable from the
//! SDK through the documented `event_store()` / `engine()` escape hatches — no test-only backdoor.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{ContentBlock, Result, StopReason};
use flux_flow::ast::{DraftAst, Node, NodeId, RunEvent, SymbolName};
use flux_lang::runtime::{flow_key, sha256_hex, stmt_hash16};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::tools::{Tool, ToolContext, ToolResult, ToolSpec};
use flux_sdk::{AgentSink, Client, Storage};
use serde_json::json;

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "flux-sdk-resurrect-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The op whose side effect must not happen twice.
struct CountedTool(Arc<AtomicUsize>);
#[async_trait]
impl Tool for CountedTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "counted",
            "bump a counter",
            json!({"type": "object", "properties": {}}),
        )
    }
    async fn execute(&self, _c: &ToolContext, _p: serde_json::Value) -> Result<ToolResult> {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ToolResult::ok(format!("count={n}")))
    }
}

/// Panics if the model is ever called — resurrection re-runs a DURABLE plan, so a model call during
/// one is a defect, not a cost concern.
struct NeverProvider;
#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        panic!("resurrecting a durable plan must never call the model");
    }
}

/// Answers any request with prose — for the tests that run a REAL turn after the resurrection.
struct ProseProvider(&'static str);
#[async_trait]
impl Provider for ProseProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        let chunks = if req.tools.iter().any(|t| t.name == "declare_intent") {
            vec![
                flux_core::Chunk::Block(ContentBlock::ToolUse {
                    id: "intent".into(),
                    name: "declare_intent".into(),
                    input: json!({ "intent": "answer", "capability_families": ["core"] }),
                }),
                flux_core::Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ]
        } else {
            vec![
                flux_core::Chunk::Block(ContentBlock::Text {
                    text: self.0.into(),
                }),
                flux_core::Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ]
        };
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

fn client(
    dir: &std::path::Path,
    storage: Storage,
    counter: Arc<AtomicUsize>,
    provider: Box<dyn Provider>,
    auto_resurrect: Option<bool>,
) -> Client {
    let mut builder = Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(storage)
        .register_op(Arc::new(CountedTool(counter)));
    if let Some(on) = auto_resurrect {
        builder = builder.auto_resurrect(on);
    }
    builder.build(provider, dir).unwrap()
}

/// `bind a = counted(); bind b = counted()` — two dispatching statements, so a crash can land
/// between them.
fn two_step_plan() -> DraftAst {
    DraftAst {
        body: (0..2)
            .map(|i| Node::Bind {
                name: SymbolName(format!("s{i}")),
                value: Box::new(Node::Call {
                    op: "counted".into(),
                    args: vec![],
                }),
                ty: None,
                effect: None,
            })
            .collect(),
        ..Default::default()
    }
}

/// Seed an interrupted turn on `session`: an accepted plan, `completed` finished statements (with
/// real bound values so the interpreter's fast-forward rehydrates), and one recorded cassette cell
/// per completed statement — then NO `TurnEnded`. Returns the turn id.
fn seed_crash(cl: &Client, session: &str, ast: &DraftAst, completed: usize) -> i64 {
    let events = cl.event_store();
    let flow = &cl.engine().flow;
    let turn_id = events.begin_turn(session, "do the thing", "mock").unwrap();
    events
        .record_plan_attempt(
            session,
            turn_id,
            flux_events::PlanAttempt {
                step: 1,
                outcome: "accepted".into(),
                plan_source: Some(flux_lang::format::format(ast)),
                ..Default::default()
            },
        )
        .unwrap();
    let key = flow_key(ast.name.as_deref(), &ast.body);
    for idx in 0..completed {
        events
            .record_run_event(
                session,
                &RunEvent::OpRecorded {
                    seq: idx as u32,
                    step: flux_flow::ast::StepId(format!("step_counted_{idx}")),
                    op: "counted".into(),
                    input_hash: sha256_hex("{}"),
                    input_hash_redacted: None,
                    input_view: Some("{}".into()),
                    input_view_truncated: false,
                    content: format!("count={}", idx + 1),
                    view: None,
                    is_error: false,
                    denied: false,
                    redacted: false,
                    truncated: false,
                },
            )
            .unwrap();
        let vid = flow
            .put_value(
                session,
                &flux_lang::ast::Value::String(format!("count={}", idx + 1)),
            )
            .unwrap();
        events
            .record_run_event(
                session,
                &RunEvent::StatementCompleted {
                    plan: key.clone(),
                    node: NodeId(idx as u32),
                    stmt: stmt_hash16(&ast.body[idx]),
                    value: Some(vid),
                    skipped: false,
                },
            )
            .unwrap();
    }
    turn_id
}

struct NullSink;
impl AgentSink for NullSink {}

// --- acceptance tests --------------------------------------------------------

/// Failing-first headline: `interrupted()` detects the killed turn, and `resurrect()` finishes it
/// in place — the completed statement is fast-forwarded (its side effect does NOT happen again),
/// only the crash tail runs live, and the model is never called.
#[tokio::test]
async fn resurrect_finishes_the_killed_turn_without_re_running_completed_side_effects() {
    let dir = tmp_dir("headline");
    let counter = Arc::new(AtomicUsize::new(0));
    let cl = client(
        &dir,
        Storage::dir(dir.join("store")),
        counter.clone(),
        Box::new(NeverProvider),
        None,
    );
    let session = cl.create_session().unwrap();
    let ast = two_step_plan();
    let turn_id = seed_crash(&cl, session.id(), &ast, 1);

    let it = session
        .interrupted()
        .unwrap()
        .expect("the seeded turn never ended");
    assert_eq!(it.turn_id, turn_id);
    assert_eq!(it.completed, 1, "one statement finished before the crash");

    let mut sink = NullSink;
    let report = session
        .resurrect(&mut sink)
        .await
        .unwrap()
        .expect("there was a turn to resurrect");

    assert_eq!(report.outcome, "resurrected", "{report:?}");
    assert!(report.diverged.is_none(), "{:?}", report.diverged);
    assert_eq!(
        report.statements_fast_forwarded, 1,
        "the completed statement is replayed from the ledger, not re-dispatched: {report:?}"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "exactly the crash-tail statement ran live — the completed one never re-fired"
    );
    assert!(
        session.interrupted().unwrap().is_none(),
        "the turn is closed now"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `auto_resurrect` defaults ON for a durable `Storage::dir`: the next turn finishes the killed one
/// first, and says so on its `TurnOutput` — never silently.
#[tokio::test]
async fn auto_resurrect_is_on_by_default_for_durable_storage_and_is_reported() {
    let dir = tmp_dir("auto-on");
    let counter = Arc::new(AtomicUsize::new(0));
    let cl = client(
        &dir,
        Storage::dir(dir.join("store")),
        counter.clone(),
        Box::new(ProseProvider("all done")),
        None,
    );
    let session = cl.create_session().unwrap();
    let ast = two_step_plan();
    seed_crash(&cl, session.id(), &ast, 1);

    let out = session.send("what next?").await.unwrap();

    let report = out
        .resurrected
        .as_ref()
        .expect("the interrupted turn was finished first, and reported");
    assert_eq!(report.outcome, "resurrected", "{report:?}");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert_eq!(
        out.text, "all done",
        "the new turn's own output is not polluted by the resurrected one"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `auto_resurrect(false)` leaves the killed turn alone — the interrupted turn is still there
/// afterwards, for the embedder to handle explicitly.
#[tokio::test]
async fn auto_resurrect_off_leaves_the_interrupted_turn_for_the_embedder() {
    let dir = tmp_dir("auto-off");
    let counter = Arc::new(AtomicUsize::new(0));
    let cl = client(
        &dir,
        Storage::dir(dir.join("store")),
        counter.clone(),
        Box::new(ProseProvider("ignored it")),
        Some(false),
    );
    let session = cl.create_session().unwrap();
    let ast = two_step_plan();
    seed_crash(&cl, session.id(), &ast, 1);

    let out = session.send("what next?").await.unwrap();

    assert!(out.resurrected.is_none(), "auto-resurrect was turned off");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "nothing from the killed plan ran"
    );
    assert!(
        session.interrupted().unwrap().is_some(),
        "the killed turn is still open, waiting for an explicit resurrect()"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// In-memory storage defaults auto-resurrect OFF — a crash takes the store with it, so there is
/// never anything to resurrect and the check would be pure overhead on every turn.
#[tokio::test]
async fn auto_resurrect_defaults_off_for_in_memory_storage() {
    let dir = tmp_dir("in-memory");
    let counter = Arc::new(AtomicUsize::new(0));
    let cl = client(
        &dir,
        Storage::in_memory(),
        counter.clone(),
        Box::new(ProseProvider("hi")),
        None,
    );
    let session = cl.create_session().unwrap();
    let ast = two_step_plan();
    seed_crash(&cl, session.id(), &ast, 1);

    let out = session.send("what next?").await.unwrap();

    assert!(out.resurrected.is_none());
    assert!(session.interrupted().unwrap().is_some());

    std::fs::remove_dir_all(&dir).ok();
}

/// `interrupted()` is `Ok(None)` — not an error, not a false positive — on a session whose turns all
/// closed cleanly.
#[tokio::test]
async fn interrupted_is_none_on_a_clean_session() {
    let dir = tmp_dir("clean");
    let cl = client(
        &dir,
        Storage::dir(dir.join("store")),
        Arc::new(AtomicUsize::new(0)),
        Box::new(ProseProvider("hello")),
        None,
    );
    let session = cl.create_session().unwrap();
    session.send("hi").await.unwrap();

    assert!(session.interrupted().unwrap().is_none());

    std::fs::remove_dir_all(&dir).ok();
}
