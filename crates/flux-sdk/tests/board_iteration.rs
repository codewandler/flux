//! C-236: a coordinator Program reasons over the board — typed rows, not prose.
//!
//! Before this story, board ops returned human text with no `output_schema`, and `board.list`
//! (`render_compact`) omitted `runner`/`task_id`/`depends_on`/`repo`, so `each`/`match` had nothing
//! typed to bind. These journeys parse real Flux-Lang source and run it through the real executor
//! (`FlowClient`), the same path a first-class `board` Program declaration takes.
//!
//! It lives in `flux-sdk` for the same reason `fleet_board_recovery.rs` does: the `WorkBoard` port
//! is L5 (`flux-capabilities`), the flow surface is the SDK, and only here can a test drive both.
//! Offline throughout: `MemoryBoard`, a mock provider that is never called, and a loopback A2A
//! worker stub for the URL-chain journey.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use flux_capabilities::{try_register_work_board, MemoryBoard, WorkBoard};
use flux_core::{Error, Result};
use flux_datasource::board::ItemDraft;
use flux_orchestrate::FleetStatusTool;
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::FlowClient;
use flux_system::net::PrivateNetAllow;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ── Harness ─────────────────────────────────────────────────────────────────

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "flux-sdk-board-iteration-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create board-iteration test workspace");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// These journeys dispatch pure/datasource ops only; the model is never consulted.
struct NeverProvider;

#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "never"
    }

    async fn stream(&self, _request: Request) -> Result<ChunkStream> {
        Err(Error::Other(
            "a board-iteration journey must not invoke the model".into(),
        ))
    }
}

/// A `ToolContext` for seeding the board directly (the journeys themselves go through ops).
fn ctx(root: &TestDir) -> flux_sdk::tools::ToolContext {
    flux_sdk::tools::ToolContext::new(Arc::new(flux_system::System::new(
        flux_system::Workspace::new(root.path()).unwrap(),
    )))
}

/// A `FlowClient` with `board.*` registered exactly the way `flux app` registers a declared board
/// (`try_register_work_board`), plus whatever extra ops the journey needs.
fn client_with_board(root: &TestDir, board: Arc<dyn WorkBoard>) -> FlowClient {
    let mut client = FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(Arc::new(NeverProvider), root.path())
        .expect("build FlowClient");
    client
        .try_register_pack(|registry| {
            try_register_work_board(registry, "board", board.clone()).map(|_| ())
        })
        .expect("register the work board");
    client
}

async fn run(client: &FlowClient, source: &str) -> flux_sdk::ExecutionResult {
    let ast = flux_lang::parse::parse(source).expect("the journey parses");
    client.execute(&ast).await.expect("the journey runs")
}

// ── A loopback A2A worker (same shape as fleet_board_recovery.rs) ───────────

async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(end) = text.find("\r\n\r\n") {
            let len = text[..end]
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

async fn worker_stub(respond: impl Fn(&str) -> Value + Send + Sync + 'static) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let raw = read_request(&mut sock).await;
            let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
            let req: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let method = req
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let payload =
                json!({ "jsonrpc": "2.0", "id": 1, "result": respond(&method) }).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    format!("http://{addr}")
}

// ── The journeys ────────────────────────────────────────────────────────────

/// The headline acceptance: `each $item in board.query({...})` binds typed `id`/`runner`/`task_id`
/// and a `match` on `state` dispatches on it — the wave-selection shape a coordinator runs. Today
/// this fails at the first step: `board.query` is not a registered operation. (The `each` source
/// is bound first — Flux-Lang accepts only pure nodes as an `each` source, so a call is bound to a
/// symbol before iterating, the language's documented idiom.)
#[tokio::test]
async fn a_program_iterates_the_board_and_matches_on_state() {
    let root = TestDir::new("sweep");
    let board: Arc<dyn WorkBoard> = Arc::new(MemoryBoard::new());
    let ctx = ctx(&root);
    // One claimed + dispatched item (has runner/task_id), one still ready.
    let dispatched = board
        .create(
            &ctx,
            ItemDraft {
                title: "index the monorepo".into(),
                ..ItemDraft::default()
            },
        )
        .await
        .unwrap();
    board.claim(&ctx, &dispatched.id, "worker-1").await.unwrap();
    board
        .record_dispatch(
            &ctx,
            &dispatched.id,
            "https://worker-1.internal:8787",
            "t_1",
        )
        .await
        .unwrap();
    board
        .create(
            &ctx,
            ItemDraft {
                title: "not dispatched yet".into(),
                ..ItemDraft::default()
            },
        )
        .await
        .unwrap();

    let client = client_with_board(&root, board);
    let out = run(
        &client,
        r#"flow sweep
  $items = board.query({filters: {state: "claimed"}})
  each $item in $items -> $rows
    $state = $item.state
    $id = $item.id
    $runner = $item.runner
    $task_id = $item.task_id
    match $state
      case "claimed"
        $row = fmt("{id}|{runner}|{task_id}")
      default
        $row = "unexpected:{state}"
  return $rows"#,
    )
    .await;

    let rows: Vec<String> = out
        .parse()
        .expect("the sweep returns a JSON array of row strings");
    assert_eq!(
        rows,
        vec![format!(
            "{}|https://worker-1.internal:8787|t_1",
            dispatched.id
        )],
        "each bound the typed fields and match dispatched on state"
    );
}

/// "Ready and unblocked" is one call: `depends_on: "satisfied"` keeps exactly the items whose
/// every dependency is `done` — the filter a wave selector needs.
#[tokio::test]
async fn a_program_selects_ready_and_unblocked_items_in_one_query() {
    let root = TestDir::new("unblocked");
    let board: Arc<dyn WorkBoard> = Arc::new(MemoryBoard::new());
    let ctx = ctx(&root);
    let parent = board
        .create(
            &ctx,
            ItemDraft {
                title: "parent".into(),
                ..ItemDraft::default()
            },
        )
        .await
        .unwrap();
    board
        .create(
            &ctx,
            ItemDraft {
                title: "child".into(),
                depends_on: vec![parent.id.clone()],
                ..ItemDraft::default()
            },
        )
        .await
        .unwrap();

    let client = client_with_board(&root, board);
    let out = run(
        &client,
        r#"flow wave
  $rows = board.query({filters: {state: "ready", depends_on: "satisfied"}})
  return $rows"#,
    )
    .await;

    let rows: Vec<Value> = out.parse().expect("query rows are JSON");
    let titles: Vec<&str> = rows
        .iter()
        .map(|row| row["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec!["parent"],
        "only the dependency-free item is ready and unblocked"
    );
    assert_eq!(rows[0]["depends_on"], json!([]));
    assert_eq!(rows[0]["runner"], json!(null), "null, never a missing key");
}

/// The comment read-back: what `board.comment` wrote, `board.comments` returns — so a sweep can
/// see what a worker recorded.
#[tokio::test]
async fn a_program_reads_comments_back_off_the_board() {
    let root = TestDir::new("comments");
    let board: Arc<dyn WorkBoard> = Arc::new(MemoryBoard::new());
    let ctx = ctx(&root);
    let item = board
        .create(
            &ctx,
            ItemDraft {
                title: "noted".into(),
                ..ItemDraft::default()
            },
        )
        .await
        .unwrap();
    board
        .comment(&ctx, &item.id, "worker started")
        .await
        .unwrap();
    board
        .comment(&ctx, &item.id, "gate is green")
        .await
        .unwrap();

    let client = client_with_board(&root, board);
    let out = run(
        &client,
        &format!(
            r#"flow noted
  $notes = board.comments({{id: "{}"}})
  return $notes"#,
            item.id
        ),
    )
    .await;

    let notes: Vec<String> = out.parse().expect("comments are a JSON array");
    assert_eq!(notes, vec!["worker started", "gate is green"]);
}

/// C-235, end to end: extract a URL from board text with `regex_extract` and feed it to
/// `fleet.status`, an op that parses its `worker` argument as a URL. With the JSON-quoted result
/// this died with `invalid url: relative URL without a base` — the exact failure that stopped the
/// 0.36.0 fleet smoke test's last link.
#[tokio::test]
async fn an_extracted_url_feeds_an_op_that_parses_a_url() {
    let worker = worker_stub(|method| match method {
        "tasks/get" => json!({
            "kind": "task",
            "id": "t_1",
            "status": {
                "state": "working",
                "message": {
                    "kind": "message",
                    "messageId": "m_1",
                    "role": "agent",
                    "parts": [{ "kind": "text", "text": "still going" }],
                },
            },
        }),
        _ => Value::Null,
    })
    .await;

    let root = TestDir::new("chain");
    let board: Arc<dyn WorkBoard> = Arc::new(MemoryBoard::new());
    let mut client = client_with_board(&root, board);
    client
        .try_register_op(Arc::new(FleetStatusTool::new(PrivateNetAllow::Any, None)))
        .expect("register fleet.status");

    let source = format!(
        r#"flow chain
  $runner = regex_extract({{s: "[item item-1] claimed runner: {worker} task_id: t_1", pattern: "runner: (\\S+)", group: 1}})
  $status = fleet.status({{worker: $runner, task_id: "t_1"}})
  return $status"#
    );
    let out = run(&client, &source).await;

    let status: Value = out.parse().expect("fleet.status returns JSON");
    assert_eq!(
        status["state"], "working",
        "the extracted URL dialed the worker — unquoted, as C-235 requires"
    );
}
