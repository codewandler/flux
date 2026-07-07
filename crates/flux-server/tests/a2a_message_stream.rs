//! C-41: `message/stream` (the A2A JSON-RPC `tasks/sendSubscribe`-equivalent) — SSE framing driven
//! through the real axum `Router` end to end. Asserts the actual event/data structure (task/context
//! id continuity, a non-final `working` ack, a `working` frame carrying the turn's answer, then a
//! single terminal `completed` frame with `final: true`), not just substring presence in the raw
//! body.
//!
//! Note on granularity: the flux-flow engine compiles a whole turn (plan-or-prose) before handing
//! the sink anything — `FlowEngine::run_turn_cancellable` calls `sink.text_delta` exactly once, with
//! the turn's final answer, not once per raw provider chunk (see `engine.rs`'s
//! `sink.text_delta(&answer)`). So even a provider that streams multiple `TextDelta` chunks (here,
//! `MultiDeltaProvider`'s two chunks) surfaces as a single "working" delta frame carrying the
//! concatenated text — that's the real, current shape of the stream, not an artifact of the mock.

mod support;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn message_stream_emits_a_working_frame_then_a_final_completed_frame() {
    let engine = support::test_engine(Arc::new(support::MultiDeltaProvider));
    let app = flux_server::router(engine, None, flux_server::CardInfo::flux_coding());

    let body = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "message/stream",
        "params": {
            "message": {
                "contextId": "ctx-stream",
                "parts": [{ "kind": "text", "text": "hi" }],
            }
        }
    });
    let (status, raw) = support::post_raw(app, "/a2a", "application/json", &body.to_string()).await;
    assert_eq!(status, StatusCode::OK);

    let frames = support::parse_sse_json(&raw);
    // [working-ack (no message yet), working (turn's answer), completed (final, no message)].
    assert_eq!(
        frames.len(),
        3,
        "expected [working-ack, working-answer, completed]; got {frames:#?}"
    );

    // Every frame is a well-formed JSON-RPC response echoing the request id.
    for f in &frames {
        assert_eq!(f["jsonrpc"], "2.0");
        assert_eq!(f["id"], 7);
        assert!(f.get("error").is_none());
    }

    // First frame: the initial "working" update with no message yet (task just started).
    let first = &frames[0];
    assert_eq!(first["result"]["kind"], "status-update");
    assert_eq!(first["result"]["status"]["state"], "working");
    assert_eq!(first["result"]["final"], false);
    assert!(first["result"]["status"]["message"].is_null());

    // Second frame: the turn's answer, streamed as a non-final "working" update.
    let mid = &frames[1];
    assert_eq!(mid["result"]["status"]["state"], "working");
    assert_eq!(mid["result"]["final"], false);
    assert_eq!(
        mid["result"]["status"]["message"]["parts"][0]["text"], "hello world",
        "the provider's two TextDelta chunks are concatenated into the turn's one answer"
    );

    // Last frame: the terminal "completed" frame — final: true, no message (deltas already
    // streamed are authoritative, per `StreamSink`/`subscribe`'s doc comments).
    let last = &frames[2];
    assert_eq!(last["result"]["status"]["state"], "completed");
    assert_eq!(last["result"]["final"], true);
    assert!(last["result"]["status"]["message"].is_null());

    // task/context ids are stable across every frame in the stream.
    let task_id = frames[0]["result"]["taskId"].as_str().unwrap();
    let context_id = frames[0]["result"]["contextId"].as_str().unwrap();
    assert_eq!(context_id, "ctx-stream");
    for f in &frames {
        assert_eq!(f["result"]["taskId"], task_id);
        assert_eq!(f["result"]["contextId"], context_id);
    }
}
