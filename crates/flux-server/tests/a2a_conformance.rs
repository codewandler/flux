//! A2A conformance over the real router: card shape (A-49), error codes (A-50), inbound part
//! handling (A-51), outbound `Task` fidelity — history + artifacts (A-52) — and the stateful task
//! surface (A-54 non-blocking send + `tasks/get`, A-55 `tasks/cancel`, A-56 `tasks/resubscribe`,
//! A-57 push-notification configs + webhook delivery).
//!
//! These exercise the `flux-server` HTTP dispatch sites — the single-agent `a2a_handler` and the
//! resolver-keyed `a2a_handler_multi` — through the production router, so the shared
//! `flux_a2a::server` boundary (`is_unsupported_a2a_method` for method classification, `extract_input`
//! for the accept/refuse decision on inbound parts) is proven wired at every dispatch site, not just
//! unit-tested in `flux-a2a`. The sites share those helpers precisely so behavior cannot drift
//! between them.

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use flux_a2a::AgentProvider;
use flux_server::{router_multi, CardInfo, ServerAuth, StaticResolver};

use support::{app, post_json, post_raw, test_engine, ProseProvider};

/// A provider whose turn stays in flight for ~800ms — long enough for an out-of-band
/// `tasks/cancel` or `tasks/resubscribe` to observably land mid-run.
struct SlowProvider;

#[async_trait::async_trait]
impl flux_provider::Provider for SlowProvider {
    fn name(&self) -> &str {
        "mock-slow"
    }
    async fn stream(
        &self,
        _req: flux_provider::Request,
    ) -> flux_core::Result<flux_provider::ChunkStream> {
        Ok(Box::pin(async_stream::stream! {
            yield Ok(flux_core::Chunk::TextDelta("thinking ".into()));
            tokio::time::sleep(Duration::from_millis(800)).await;
            yield Ok(flux_core::Chunk::TextDelta("done".into()));
            yield Ok(flux_core::Chunk::Done {
                stop_reason: Some(flux_core::StopReason::EndTurn),
            });
        }))
    }
}

const TRANSPORT_JSONRPC: &str = "JSONRPC";

/// GET a discovery card and parse it.
async fn get_card(app: Router, path: &str) -> Value {
    let res = app
        .oneshot(HttpRequest::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// A-49: the served card carries the spec-required `protocolVersion`, a populated `interfaces`
/// whose JSON-RPC entry url equals the card `url`, a matching `preferredTransport`, and an honest
/// `supportsAuthenticatedExtendedCard: false`.
#[tokio::test]
async fn served_card_is_conformant() {
    let card = get_card(app(None), "/.well-known/agent-card.json").await;

    assert!(
        card["protocolVersion"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "protocolVersion is spec-required: {card}"
    );
    assert_eq!(card["preferredTransport"], TRANSPORT_JSONRPC);
    assert_eq!(card["supportsAuthenticatedExtendedCard"], false);

    let url = card["url"].as_str().expect("card has a url");
    let interfaces = card["interfaces"].as_array().expect("interfaces array");
    assert_eq!(interfaces.len(), 1, "one declared transport: {card}");
    assert_eq!(interfaces[0]["transport"], TRANSPORT_JSONRPC);
    assert_eq!(
        interfaces[0]["url"].as_str(),
        Some(url),
        "the JSON-RPC interface url equals the card url"
    );

    // The legacy `…/agent.json` alias serves the identical (conformant) card.
    let alias = get_card(app(None), "/.well-known/agent.json").await;
    assert_eq!(alias["protocolVersion"], card["protocolVersion"]);
    assert_eq!(alias["interfaces"], card["interfaces"]);
}

/// A-49: optional discovery metadata (`provider`/`documentationUrl`/`iconUrl`) is emitted only when
/// the served agent's `CardInfo` carries it — and a card that sets none omits the keys entirely.
#[tokio::test]
async fn optional_card_metadata_is_emitted_when_set() {
    let engine = test_engine(Arc::new(ProseProvider));
    let card_info = CardInfo::flux_coding()
        .with_provider(AgentProvider {
            organization: "Acme".to_string(),
            url: "https://acme.example".to_string(),
        })
        .with_documentation_url("https://docs.example")
        .with_icon_url("https://icon.example/i.png");
    let with_meta = flux_server::router(engine, ServerAuth::Open, card_info);

    let card = get_card(with_meta, "/.well-known/agent-card.json").await;
    assert_eq!(card["provider"]["organization"], "Acme");
    assert_eq!(card["provider"]["url"], "https://acme.example");
    assert_eq!(card["documentationUrl"], "https://docs.example");
    assert_eq!(card["iconUrl"], "https://icon.example/i.png");

    // The default card (no metadata set) omits the keys entirely.
    let bare = get_card(app(None), "/.well-known/agent-card.json").await;
    assert!(bare.get("provider").is_none(), "provider omitted: {bare}");
    assert!(bare.get("documentationUrl").is_none());
    assert!(bare.get("iconUrl").is_none());
}

fn rpc(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
}

/// A2A **blocking** `message/send` params over `contextId`, optionally capping `historyLength`.
/// (The A-54 spec default is non-blocking, so tests that assert the synchronous completed-Task
/// shape opt into `blocking: true` explicitly.)
fn send_over_context(text: &str, context_id: &str, history_length: Option<u64>) -> Value {
    let mut msg = json!({
        "message": { "contextId": context_id, "parts": [{ "kind": "text", "text": text }] },
        "configuration": { "blocking": true },
    });
    if let Some(n) = history_length {
        msg["configuration"]["historyLength"] = json!(n);
    }
    rpc("message/send", msg)
}

/// A **non-blocking** `message/send` (no `configuration.blocking` — the spec default).
fn send_nonblocking(text: &str, context_id: &str) -> Value {
    rpc(
        "message/send",
        json!({
            "message": { "contextId": context_id, "parts": [{ "kind": "text", "text": text }] },
        }),
    )
}

/// `tasks/get` for `task_id`, returning the parsed JSON-RPC response.
async fn get_task(app: Router, task_id: &str) -> Value {
    post_json(app, "/a2a", rpc("tasks/get", json!({ "id": task_id })))
        .await
        .1
}

/// Poll `tasks/get` until the task reaches `want` (or panic after ~3s) — the A-54 client shape.
async fn await_task_state(app: &Router, task_id: &str, want: &str) -> Value {
    for _ in 0..150 {
        let r = get_task(app.clone(), task_id).await;
        if r["result"]["status"]["state"] == want {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("task {task_id} never reached state {want:?}");
}

/// A-52: a blocking `message/send` returns `Task.history` from the engine's conversation
/// projection, it accumulates across turns of the same `contextId`, and `configuration.historyLength`
/// caps it to the most-recent messages. (One router reused across turns so all three requests hit
/// the same engine/event store.)
#[tokio::test]
async fn task_history_is_populated_and_bounded() {
    let engine = test_engine(Arc::new(ProseProvider));
    let app = flux_server::router(engine, ServerAuth::Open, CardInfo::flux_coding());

    // Turn 1 over a fresh context: history holds this turn's user + agent messages.
    let (_, r1) = post_json(
        app.clone(),
        "/a2a",
        send_over_context("first", "ctx-hist", None),
    )
    .await;
    let h1 = r1["result"]["history"].as_array().expect("history array");
    assert!(!h1.is_empty(), "turn-1 history is populated: {r1}");

    // Turn 2 over the same context accumulates — history grows beyond turn 1.
    let (_, r2) = post_json(
        app.clone(),
        "/a2a",
        send_over_context("second", "ctx-hist", None),
    )
    .await;
    let full = r2["result"]["history"].as_array().unwrap().len();
    assert!(full > h1.len(), "same-context history accumulates: {r2}");

    // Turn 3 caps to the two most-recent messages (this turn's user + agent).
    let (_, r3) = post_json(
        app.clone(),
        "/a2a",
        send_over_context("third", "ctx-hist", Some(2)),
    )
    .await;
    let capped = r3["result"]["history"].as_array().unwrap();
    assert_eq!(capped.len(), 2, "historyLength=2 caps to 2 messages: {r3}");
    assert_eq!(capped[0]["role"], "user", "the just-sent user turn is kept");
    assert_eq!(capped[0]["parts"][0]["text"], "third");
    assert_eq!(capped[1]["role"], "agent", "followed by the agent reply");
}

/// A-51: a `message/send` whose only part is a `data` part runs a real turn — the structured
/// payload is surfaced into the input, so the task completes normally (not an empty-input turn),
/// while a `file` part is refused with `-32005` rather than silently dropped.
#[tokio::test]
async fn inbound_data_part_is_surfaced_and_file_part_is_refused() {
    let data_only = rpc(
        "message/send",
        json!({
            "message": { "parts": [{ "kind": "data", "data": { "ticket": 42 } }] },
            "configuration": { "blocking": true },
        }),
    );
    let (_, ok) = post_json(app(None), "/a2a", data_only).await;
    assert_eq!(
        ok["result"]["status"]["state"], "completed",
        "a data-only message runs a real turn: {ok}"
    );

    let with_file = rpc(
        "message/send",
        json!({ "message": { "parts": [
            { "kind": "text", "text": "handle this" },
            { "kind": "file", "file": { "uri": "http://x/y.pdf" } },
        ] } }),
    );
    let (_, refused) = post_json(app(None), "/a2a", with_file).await;
    assert_eq!(
        refused["error"]["code"], -32005,
        "a file part is refused, not silently dropped: {refused}"
    );
}

/// A-50 (single-agent `a2a_handler`): a defined-but-unsupported method → `-32004`; a
/// genuinely-unknown method → `-32601`; a message with a part but no text → `-32005`. (Since
/// A-54/55/56/57 the `tasks/*` methods are implemented, so the remaining `-32004` exemplar is the
/// extended card; `tasks/cancel` without a task id is now a plain `-32602`.)
#[tokio::test]
async fn error_codes_on_the_single_agent_dispatcher() {
    let (_, extended) = post_json(
        app(None),
        "/a2a",
        rpc("agent/getAuthenticatedExtendedCard", json!({})),
    )
    .await;
    assert_eq!(
        extended["error"]["code"], -32004,
        "extended card → -32004: {extended}"
    );

    let (_, cancel) = post_json(app(None), "/a2a", rpc("tasks/cancel", json!({}))).await;
    assert_eq!(
        cancel["error"]["code"], -32602,
        "tasks/cancel without a task id → -32602: {cancel}"
    );

    let (_, unknown) = post_json(app(None), "/a2a", rpc("foo/bar", json!({}))).await;
    assert_eq!(
        unknown["error"]["code"], -32601,
        "unknown method → -32601: {unknown}"
    );

    let file_only = rpc(
        "message/send",
        json!({ "message": { "parts": [{ "kind": "file", "file": {} }] } }),
    );
    let (_, no_text) = post_json(app(None), "/a2a", file_only).await;
    assert_eq!(
        no_text["error"]["code"], -32005,
        "no usable text part → -32005: {no_text}"
    );
}

/// A-50/A-54 (resolver-keyed `a2a_handler_multi`): the shared dispatcher governs the multi-agent
/// mount, so an unsupported method under `/:agent_id/a2a` returns `-32004` and the stateful task
/// surface is wired there too (an unknown task id → `-32001`).
#[tokio::test]
async fn unsupported_method_on_the_multi_agent_dispatcher() {
    let engine = test_engine(Arc::new(ProseProvider));
    let resolver =
        StaticResolver::new().with_agent("support", engine, CardInfo::for_agent("support", None));
    let app = router_multi(Arc::new(resolver), ServerAuth::Open);

    let post = |body: Value| {
        HttpRequest::post("/support/a2a")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let res = app
        .clone()
        .oneshot(post(rpc("agent/getAuthenticatedExtendedCard", json!({}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["error"]["code"], -32004,
        "multi-mount extended card → -32004: {body}"
    );

    // The stateful task surface is wired on the multi mount too (A-54).
    let res = app
        .oneshot(post(rpc("tasks/get", json!({ "id": "s_424242" }))))
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["error"]["code"], -32001,
        "multi-mount tasks/get of an unknown id → -32001: {body}"
    );
}

// ── Tier 3: the stateful task surface (A-54..A-57) ─────────────────────────────

/// A-54: a `message/send` WITHOUT `blocking: true` (the spec default) returns a non-terminal task
/// immediately; `tasks/get` then observes it reach `completed`, with the answer in
/// `status.message` and the conversation in `history` — while a blocking send is unchanged.
#[tokio::test]
async fn non_blocking_send_returns_submitted_then_get_observes_completed() {
    let engine = test_engine(Arc::new(ProseProvider));
    let app = flux_server::router(engine, ServerAuth::Open, CardInfo::flux_coding());

    let (_, sub) = post_json(app.clone(), "/a2a", send_nonblocking("hi", "ctx-nb")).await;
    let state = sub["result"]["status"]["state"].as_str().unwrap();
    assert!(
        state == "submitted" || state == "working",
        "non-blocking send answers a non-terminal task immediately: {sub}"
    );
    let task_id = sub["result"]["id"].as_str().expect("task id").to_string();

    let done = await_task_state(&app, &task_id, "completed").await;
    assert_eq!(
        done["result"]["status"]["message"]["parts"][0]["text"], "ok",
        "the projected task carries the recorded answer: {done}"
    );
    assert!(
        !done["result"]["history"].as_array().unwrap().is_empty(),
        "the projected task carries history: {done}"
    );
    assert_eq!(done["result"]["contextId"], "ctx-nb");

    // The blocking fast path is unchanged: a completed task in one round trip.
    let (_, blocking) = post_json(
        app.clone(),
        "/a2a",
        send_over_context("again", "ctx-b", None),
    )
    .await;
    assert_eq!(blocking["result"]["status"]["state"], "completed");
}

/// A-54: `tasks/get` on an unknown id — and on a real session that was NOT minted by the A2A
/// surface (a guessable CLI session id) — is a constant `-32001 TaskNotFound`.
#[tokio::test]
async fn tasks_get_unknown_and_non_a2a_ids_are_not_found() {
    let engine = test_engine(Arc::new(ProseProvider));
    // A real, live session — but a CLI one (no `a2a` tag): unreachable through the task surface.
    let cli_session = engine.events.create_session("m").unwrap();
    let app = flux_server::router(engine, ServerAuth::Open, CardInfo::flux_coding());

    let unknown = get_task(app.clone(), "s_424242").await;
    assert_eq!(unknown["error"]["code"], -32001, "unknown id: {unknown}");

    let cli = get_task(app.clone(), &cli_session).await;
    assert_eq!(
        cli["error"]["code"], -32001,
        "a non-A2A session is not addressable as a task: {cli}"
    );
}

/// A-55: `tasks/cancel` of a live run stops it (the task projects `canceled`, durably); cancel of
/// a completed task → `-32002 TaskNotCancelable`; cancel of an unknown id → `-32001`.
#[tokio::test]
async fn tasks_cancel_stops_a_live_run_and_rejects_terminal_or_unknown() {
    let engine = test_engine(Arc::new(SlowProvider));
    let app = flux_server::router(engine, ServerAuth::Open, CardInfo::flux_coding());

    // Start a slow non-blocking run and let it get in flight.
    let (_, sub) = post_json(app.clone(), "/a2a", send_nonblocking("go", "ctx-cancel")).await;
    let task_id = sub["result"]["id"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let (_, cancel) = post_json(
        app.clone(),
        "/a2a",
        rpc("tasks/cancel", json!({ "id": task_id })),
    )
    .await;
    assert_eq!(
        cancel["result"]["status"]["state"], "canceled",
        "cancel answers the canceled task: {cancel}"
    );
    // The run observes the token and records the durable cancelled outcome.
    let done = await_task_state(&app, &task_id, "canceled").await;
    assert_eq!(done["result"]["status"]["state"], "canceled");

    // A terminal task is not cancelable.
    let (_, terminal) = post_json(
        app.clone(),
        "/a2a",
        rpc("tasks/cancel", json!({ "id": task_id })),
    )
    .await;
    assert_eq!(
        terminal["error"]["code"], -32002,
        "terminal → -32002: {terminal}"
    );

    let (_, unknown) = post_json(
        app.clone(),
        "/a2a",
        rpc("tasks/cancel", json!({ "id": "s_424242" })),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32001, "unknown: {unknown}");
}

/// Split the SSE body into its parsed `data:` JSON frames.
fn sse_frames(body: &str) -> Vec<Value> {
    body.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|d| serde_json::from_str(d).expect("SSE data frame is JSON"))
        .collect()
}

/// A-56: `tasks/resubscribe` on a RUNNING task replays its current state and follows the live run
/// to the terminal frame; on a FINISHED task it yields the terminal frame and closes; on an
/// unknown id it answers `-32001` before any SSE is established.
#[tokio::test]
async fn tasks_resubscribe_follows_live_and_replays_terminal() {
    let engine = test_engine(Arc::new(SlowProvider));
    let app = flux_server::router(engine, ServerAuth::Open, CardInfo::flux_coding());

    let (_, sub) = post_json(app.clone(), "/a2a", send_nonblocking("go", "ctx-resub")).await;
    let task_id = sub["result"]["id"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Live: ≥1 `working` frame, then a final frame closes the stream.
    let (status, body) = post_raw(
        app.clone(),
        "/a2a",
        "application/json",
        &rpc("tasks/resubscribe", json!({ "id": task_id })).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let frames = sse_frames(&body);
    assert!(
        frames
            .iter()
            .any(|f| f["result"]["status"]["state"] == "working"),
        "a live resubscribe observes a working frame: {body}"
    );
    let last = frames.last().expect("at least one frame");
    assert_eq!(
        last["result"]["final"], true,
        "stream ends terminal: {body}"
    );

    // Finished: one terminal frame, then close.
    let (_, replay) = post_raw(
        app.clone(),
        "/a2a",
        "application/json",
        &rpc("tasks/resubscribe", json!({ "id": task_id })).to_string(),
    )
    .await;
    let frames = sse_frames(&replay);
    assert_eq!(frames.len(), 1, "terminal task replays once: {replay}");
    assert_eq!(frames[0]["result"]["final"], true);
    assert_eq!(frames[0]["result"]["status"]["state"], "completed");

    // Unknown: a pre-SSE JSON-RPC error.
    let (_, unknown) = post_json(
        app.clone(),
        "/a2a",
        rpc("tasks/resubscribe", json!({ "id": "s_424242" })),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32001, "unknown: {unknown}");
}

/// A-57: the card advertises push notifications; `pushNotificationConfig/{set,get,list,delete}`
/// manage a per-task webhook; a subsequent run on that task DELIVERS its terminal transition to
/// the webhook; delete stops delivery. A non-public URL is refused with `-32003`.
#[tokio::test]
async fn push_notification_config_and_delivery() {
    // The SSRF policy refuses loopback in production; tests opt out explicitly.
    std::env::set_var("FLUX_A2A_PUSH_ALLOW_LOCAL", "1");

    // The card flips the capability (A-57).
    let card = get_card(app(None), "/.well-known/agent-card.json").await;
    assert_eq!(card["capabilities"]["pushNotifications"], true);

    // A local webhook receiver collecting every delivered frame.
    let received: Arc<tokio::sync::Mutex<Vec<Value>>> = Arc::default();
    let sink = received.clone();
    let hook = Router::new().route(
        "/hook",
        axum::routing::post(move |axum::Json(v): axum::Json<Value>| {
            let sink = sink.clone();
            async move {
                sink.lock().await.push(v);
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hook_url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, hook).await.unwrap();
    });

    let engine = test_engine(Arc::new(ProseProvider));
    let app = flux_server::router(engine, ServerAuth::Open, CardInfo::flux_coding());

    // Mint the task (task id = session id, stable across the context's turns).
    let (_, first) = post_json(
        app.clone(),
        "/a2a",
        send_over_context("one", "ctx-push", None),
    )
    .await;
    let task_id = first["result"]["id"].as_str().unwrap().to_string();

    // set → echo; list → 1 entry; a non-public URL is refused.
    let (_, set) = post_json(
        app.clone(),
        "/a2a",
        rpc(
            "tasks/pushNotificationConfig/set",
            json!({ "taskId": task_id, "pushNotificationConfig": { "url": hook_url, "token": "t-1" } }),
        ),
    )
    .await;
    assert_eq!(
        set["result"]["pushNotificationConfig"]["url"],
        hook_url.as_str()
    );
    let (_, listed) = post_json(
        app.clone(),
        "/a2a",
        rpc(
            "tasks/pushNotificationConfig/list",
            json!({ "id": task_id }),
        ),
    )
    .await;
    assert_eq!(listed["result"].as_array().unwrap().len(), 1, "{listed}");
    std::env::remove_var("FLUX_A2A_PUSH_ALLOW_LOCAL");
    let (_, refused) = post_json(
        app.clone(),
        "/a2a",
        rpc(
            "tasks/pushNotificationConfig/set",
            json!({ "taskId": task_id, "pushNotificationConfig": { "url": "http://127.0.0.1:9/x" } }),
        ),
    )
    .await;
    assert_eq!(refused["error"]["code"], -32003, "{refused}");
    std::env::set_var("FLUX_A2A_PUSH_ALLOW_LOCAL", "1");

    // A second turn on the same context (same task id) delivers its terminal transition.
    let (_, second) = post_json(
        app.clone(),
        "/a2a",
        send_over_context("two", "ctx-push", None),
    )
    .await;
    assert_eq!(second["result"]["id"].as_str(), Some(task_id.as_str()));
    let mut delivered = 0usize;
    for _ in 0..150 {
        delivered = received.lock().await.len();
        if delivered > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(delivered > 0, "the webhook received a transition frame");
    {
        let frames = received.lock().await;
        assert_eq!(frames[0]["taskId"].as_str(), Some(task_id.as_str()));
        assert_eq!(frames[0]["final"], true, "{:?}", frames[0]);
    }

    // delete → a third turn delivers nothing new.
    let (_, _deleted) = post_json(
        app.clone(),
        "/a2a",
        rpc(
            "tasks/pushNotificationConfig/delete",
            json!({ "id": task_id }),
        ),
    )
    .await;
    let baseline = received.lock().await.len();
    let (_, _third) = post_json(
        app.clone(),
        "/a2a",
        send_over_context("three", "ctx-push", None),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        received.lock().await.len(),
        baseline,
        "deleted config stops delivery"
    );
}
