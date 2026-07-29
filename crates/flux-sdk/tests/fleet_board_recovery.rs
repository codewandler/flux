//! Crash recovery over the board alone — the headline claim of the fleet design (A-130).
//!
//! `docs/designs/fleet-coordinator.md` §5 says there is no second store: `fleet.dispatch` writes
//! the worker's address and the task id back onto the board `Item`, so **the board is the run
//! registry** and crash recovery is "restart, sweep, re-derive". That claim is only true if a
//! process holding *nothing but the board* can find every run that was dispatched and reach the
//! worker that owns it.
//!
//! This test lives in `flux-sdk` because it is the only crate that may legally see both halves:
//! `fleet.dispatch` is L3 (`flux-orchestrate`) and the `WorkBoard` port is L5
//! (`flux-capabilities`), so neither may depend on the other and the join has to happen at a
//! surface. The seam between them is `flux_runtime::DispatchLedger` (L2), which both already see.
//!
//! It runs offline: the "worker" is a loopback TCP socket speaking A2A JSON-RPC, and the board is
//! `MemoryBoard`.

use std::sync::{Arc, Mutex};

use flux_capabilities::{BoardLedger, MemoryBoard, WorkBoard};
use flux_datasource::board::{ItemDraft, State};
use flux_datasource::live::{Filters, PageRequest};
use flux_orchestrate::{FleetDispatchTool, FleetStatusTool};
use flux_runtime::{Tool, ToolContext};
use flux_system::net::PrivateNetAllow;
use flux_system::{System, Workspace};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ── A loopback A2A worker ───────────────────────────────────────────────────
//
// Deliberately the same shape as `flux-orchestrate`'s own stub: tokio + std only, no new
// dependency, and no network beyond 127.0.0.1.

/// Every `(method, params)` the stub worker was asked for, in order.
type Seen = Arc<Mutex<Vec<(String, Value)>>>;

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

async fn worker_stub(respond: impl Fn(&str, &Value) -> Value + Send + Sync + 'static) -> (String, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
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
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            recorder.lock().unwrap().push((method.clone(), params.clone()));
            let payload =
                json!({ "jsonrpc": "2.0", "id": 1, "result": respond(&method, &params) }).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    (format!("http://{addr}"), seen)
}

fn task_json(id: &str, state: &str, text: &str) -> Value {
    json!({
        "kind": "task",
        "id": id,
        "status": {
            "state": state,
            "message": {
                "kind": "message",
                "messageId": "m_1",
                "role": "agent",
                "parts": [{ "kind": "text", "text": text }],
            },
        },
    })
}

fn ctx() -> ToolContext {
    let root = std::env::temp_dir().join(format!(
        "flux-fleet-recovery-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&root).unwrap();
    ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap())))
}

// ── The sweep, which is handed NOTHING but the board ────────────────────────

/// One in-flight run as a restarted coordinator re-derives it.
#[derive(Debug, PartialEq, Eq)]
struct Recovered {
    item: String,
    runner: String,
    task_id: String,
    remote_state: String,
}

/// The reconciliation sweep. Its whole argument list is the board and a context — no dispatch
/// bookkeeping, no client, no task id carried over from whoever dispatched. Everything it needs to
/// reach a worker it reads off the `Item`, which is exactly the property §5 asserts.
async fn sweep(board: Arc<dyn WorkBoard>, ctx: &ToolContext) -> Vec<Recovered> {
    let status = FleetStatusTool::new(PrivateNetAllow::Any, None);
    let page = board
        .list(
            ctx,
            &Filters::new(),
            PageRequest {
                cursor: None,
                limit: 100,
            },
        )
        .await
        .unwrap();

    let mut recovered = Vec::new();
    for item in page.rows {
        // The sweep inspects exactly the in-flight states the design names.
        if !matches!(item.state, State::Claimed | State::InProgress) {
            continue;
        }
        let (Some(runner), Some(task_id)) = (item.runner.clone(), item.task_id.clone()) else {
            continue;
        };
        let out = status
            .execute(ctx, json!({ "worker": runner, "task_id": task_id }))
            .await
            .unwrap();
        assert!(!out.is_error, "fleet.status failed: {}", out.content);
        let body: Value = serde_json::from_str(&out.content).unwrap();
        recovered.push(Recovered {
            item: item.id,
            runner,
            task_id,
            remote_state: body["state"].as_str().unwrap().to_string(),
        });
    }
    recovered
}

// ── The test ────────────────────────────────────────────────────────────────

/// A-130's headline acceptance: after `fleet.dispatch`, a **new process** over the same board
/// re-derives every in-flight item and its worker, and the sweep resumes.
///
/// The "restart" is modelled by dropping every value the dispatching half held — the op, the
/// ledger, the returned task id, and even the worker's address — and rebuilding the reconciliation
/// half from `Arc<dyn WorkBoard>` alone. Before A-130 the board had `runner`/`task_id` as fields
/// that no operation could ever set, so this recovers nothing.
#[tokio::test]
async fn a_restarted_coordinator_rederives_every_dispatch_from_the_board_alone() {
    let (worker, seen) = worker_stub(|method, _p| match method {
        "message/send" => task_json("t_recover", "submitted", ""),
        "tasks/get" => task_json("t_recover", "working", "still going"),
        _ => Value::Null,
    })
    .await;
    let ctx = ctx();

    // The board is the only thing that outlives the restart — it stands in for the durable store a
    // `MarkdownBoard` or `JiraBoard` would be.
    let board: Arc<dyn WorkBoard> = Arc::new(MemoryBoard::new());

    // ── process 1: claim and dispatch ───────────────────────────────────────
    let dispatched_item = {
        let item = board
            .create(
                &ctx,
                ItemDraft {
                    title: "index the monorepo".into(),
                    ..ItemDraft::default()
                },
            )
            .await
            .unwrap();
        board.claim(&ctx, &item.id, "worker-1").await.unwrap();
        // An item that is claimed but never dispatched must not be mistaken for an in-flight run.
        let idle = board
            .create(
                &ctx,
                ItemDraft {
                    title: "not dispatched yet".into(),
                    ..ItemDraft::default()
                },
            )
            .await
            .unwrap();
        board.claim(&ctx, &idle.id, "worker-2").await.unwrap();

        let ledger = Arc::new(BoardLedger::new("board", board.clone()));
        let dispatch = FleetDispatchTool::new(PrivateNetAllow::Any, None).with_ledger(ledger);
        let out = dispatch
            .execute(
                &ctx,
                json!({ "worker": worker.clone(), "task": "index it", "item": item.id }),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "dispatch failed: {}", out.content);
        let body: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(body["task_id"], "t_recover");
        assert_eq!(body["recorded"], json!(true));
        item.id
    };
    // Everything the dispatching process held is gone here: the op, the ledger, the client, and the
    // task id. Only `board` survives.
    drop(worker);

    // ── process 2: restart, sweep, re-derive ────────────────────────────────
    let recovered = sweep(board.clone(), &ctx).await;
    assert_eq!(
        recovered.len(),
        1,
        "exactly the dispatched item is in flight: {recovered:?}"
    );
    assert_eq!(recovered[0].item, dispatched_item);
    assert_eq!(recovered[0].task_id, "t_recover");
    assert_eq!(recovered[0].remote_state, "working");
    // The address came off the board, and it worked — the sweep really reached the worker.
    let calls = seen.lock().unwrap();
    assert!(
        calls.iter().any(|(m, p)| m == "tasks/get" && p["id"] == "t_recover"),
        "the sweep never reached the worker: {calls:?}"
    );
}
