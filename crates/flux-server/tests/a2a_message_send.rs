//! C-41: `message/send` (the A2A JSON-RPC `tasks/send`-equivalent) driven through the real axum
//! `Router` end to end (in-process, `tower::ServiceExt::oneshot`) against a mock provider —
//! completes synchronously with a `completed` [`flux_a2a::Task`].

mod support;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn message_send_completes_with_completed_task_state() {
    let app = support::app(None);
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "message/send",
        "params": {
            "message": {
                "contextId": "ctx-1",
                "parts": [{ "kind": "text", "text": "hello" }],
            },
            // The A-54 spec default is non-blocking; this test asserts the synchronous shape.
            "configuration": { "blocking": true },
        }
    });
    let (status, res) = support::post_json(app, "/a2a", body).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "JSON-RPC-over-HTTP always answers 200"
    );
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["id"], 1);
    assert!(
        res.get("error").is_none(),
        "unexpected JSON-RPC error: {res}"
    );

    let task = &res["result"];
    assert_eq!(task["kind"], "task");
    assert_eq!(
        task["status"]["state"], "completed",
        "a synchronous message/send always returns the task in its terminal completed state"
    );
    assert_eq!(
        task["contextId"], "ctx-1",
        "the request's contextId is echoed back"
    );
    assert!(
        !task["id"].as_str().unwrap().is_empty(),
        "the task id is the minted session id"
    );
    assert_eq!(
        task["status"]["message"]["parts"][0]["text"], "ok",
        "the completed status carries the agent's final reply text"
    );
}

/// A request naming an unrecognized method still round-trips as a well-formed JSON-RPC error
/// (not a 500, not a panic) — the negative-space complement to the happy-path test above, and
/// shared coverage with the malformed-request suite's `-32601` case.
#[tokio::test]
async fn message_send_missing_params_is_a_json_rpc_error() {
    let app = support::app(None);
    let body = json!({ "jsonrpc": "2.0", "id": 2, "method": "message/send" });
    let (status, res) = support::post_json(app, "/a2a", body).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["error"]["code"], -32602);
    assert!(res.get("result").is_none());
}
