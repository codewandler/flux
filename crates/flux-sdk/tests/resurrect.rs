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
    // A real interrupted turn already carries its opening user message — the crash happens *between*
    // a turn's two writes. Seeding only the `TurnStarted` telemetry described a turn that cannot
    // occur, which the typed close (A-101) rejects.
    flux_events::SessionLog::open(&events, session)
        .unwrap()
        .open_turn(flux_core::Message::user_text("do the thing"))
        .unwrap();
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

/// `auto_resurrect(false)` leaves the killed turn alone — `send` runs its own new turn without
/// touching it. D-183: that is now the exact out-of-order shape `resurrect::interrupted`'s
/// tail-guard exists to catch — the interrupted turn is no longer the session's most recent one
/// (the new "what next?" turn ran and closed after it), so a later `interrupted()`/explicit
/// `resurrect()` must refuse loudly rather than finish it out of conversational order. Turning
/// auto-resurrect off is a deliberate opt into manual recovery, not an opt into silently corrupting
/// the transcript order later.
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
    let err = session.interrupted().unwrap_err().to_string();
    assert!(
        err.contains("out of conversational order") || err.contains("ran and closed after it"),
        "the tail-guard must refuse loudly now that a newer turn ran on top of the still-open \
         crashed one, got: {err}"
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
    // D-183: same tail-guard as the durable `auto_resurrect(false)` case above — `send` ran its
    // own new turn on top of the still-open (never-persisted-past-process, but still logically
    // interrupted) turn, so `interrupted()` now refuses loudly instead of reporting it forever.
    assert!(session
        .interrupted()
        .unwrap_err()
        .to_string()
        .contains("ran and closed after it"));

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

// --- D-183: every turn-entry point resurrects ---------------------------------------------

/// D-183 failing-first: a durable session crashes mid-turn; the embedder resumes via `stream()`
/// (not `send`) — the interrupted turn must be finished FIRST, before `stream()`'s own new turn
/// runs, and reported on the returned `TurnOutput`. A LATER `send()` on the same session must then
/// find nothing left to resurrect. Before D-183, `stream()` skipped `auto_resurrect_step`
/// entirely: the new turn ran on top of the still-open crashed turn, and a later `send()` would
/// resurrect it out of order (appending a stale assistant message after newer ones) — which is now
/// also refused loudly by `resurrect::interrupted`'s tail-guard, so a regression here would fail
/// hard instead of silently reordering the transcript.
#[tokio::test]
async fn stream_resurrects_an_interrupted_turn_before_its_own_new_turn_runs() {
    let dir = tmp_dir("stream-resurrect");
    let counter = Arc::new(AtomicUsize::new(0));
    let cl = client(
        &dir,
        Storage::dir(dir.join("store")),
        counter.clone(),
        Box::new(ProseProvider("streamed answer")),
        None,
    );
    let session = cl.create_session().unwrap();
    let ast = two_step_plan();
    seed_crash(&cl, session.id(), &ast, 1);

    let stream = session.stream("what next?");
    let out = stream.finish().await.unwrap();

    let report = out
        .resurrected
        .as_ref()
        .expect("stream() must finish the interrupted turn first, and report it");
    assert_eq!(report.outcome, "resurrected", "{report:?}");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "the crash-tail statement ran live, resurrecting the interrupted turn"
    );
    assert_eq!(
        out.text, "streamed answer",
        "the new turn's own output is not polluted by the resurrected one"
    );
    assert!(
        session.interrupted().unwrap().is_none(),
        "the interrupted turn is closed now, in order, before the new turn ran"
    );

    // A later `send()` must find nothing left to resurrect — no out-of-order resurrect.
    let out2 = session.send("and then?").await.unwrap();
    assert!(
        out2.resurrected.is_none(),
        "nothing is left to resurrect: stream() already finished the interrupted turn"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// D-183 coverage: `start_flow` also runs the `auto_resurrect` pre-step — a flow-driven turn must
/// not start on top of a still-open crashed turn either.
#[tokio::test]
async fn start_flow_resurrects_an_interrupted_turn_first() {
    let dir = tmp_dir("start-flow-resurrect");
    let counter = Arc::new(AtomicUsize::new(0));
    let cl = client(
        &dir,
        Storage::dir(dir.join("store")),
        counter.clone(),
        Box::new(NeverProvider),
        None,
    );
    let session = cl.create_session().unwrap();
    let crashed = two_step_plan();
    seed_crash(&cl, session.id(), &crashed, 1);

    // The flow `start_flow` itself drives — deliberately distinct from the crashed turn's own
    // plan, and dispatches no ops of its own so the counter's only increment is the resurrect.
    let flow = DraftAst {
        body: vec![Node::Return {
            value: Box::new(Node::Lit {
                value: serde_json::json!("flow started"),
            }),
        }],
        ..Default::default()
    };

    let out = session.start_flow(&flow).await.unwrap();

    let report = out
        .resurrected
        .as_ref()
        .expect("start_flow must finish the interrupted turn first, and report it");
    assert_eq!(report.outcome, "resurrected", "{report:?}");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert_eq!(
        out.text, "flow started",
        "the started flow's own output is not polluted by the resurrected one"
    );
    assert!(!out.suspended, "the flow ran straight through, no await");
    assert!(
        session.interrupted().unwrap().is_none(),
        "the interrupted turn is closed now"
    );

    std::fs::remove_dir_all(&dir).ok();
}
