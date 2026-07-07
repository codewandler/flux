//! C-41: `/a2a` never 500s or panics on bad input — a syntactically-garbage body and a
//! well-formed-but-unrecognized-method request are both handled cleanly. One `#[ignore]`d test
//! documents a real (minor) gap: garbage bodies don't get a JSON-RPC-shaped parse error today.

mod support;

use axum::http::StatusCode;
use serde_json::json;

/// A well-formed JSON-RPC request naming an unknown method yields the documented JSON-RPC error
/// shape (`error.code == -32601`), HTTP 200 (JSON-RPC errors are a payload, not a transport-level
/// failure) — not a 500, not a panic.
#[tokio::test]
async fn unrecognized_method_yields_a_json_rpc_method_not_found_error() {
    let app = support::app(None);
    let body = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tasks/bogus",
        "params": {},
    });
    let (status, res) = support::post_json(app, "/a2a", body).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(res["id"], 9);
    assert_eq!(res["error"]["code"], -32601);
    assert!(
        res["error"]["message"]
            .as_str()
            .unwrap()
            .contains("tasks/bogus"),
        "error message names the offending method: {res}"
    );
    assert!(res.get("result").is_none());
}

/// A request with the wrong `jsonrpc` version is also a clean JSON-RPC error (`-32600`), not a
/// 500/panic — same family of "malformed JSON-RPC" input as the unknown-method case.
#[tokio::test]
async fn wrong_jsonrpc_version_yields_a_json_rpc_error() {
    let app = support::app(None);
    let body = json!({ "jsonrpc": "1.0", "id": 1, "method": "message/send" });
    let (status, res) = support::post_json(app, "/a2a", body).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["error"]["code"], -32600);
}

/// A syntactically-invalid body (not JSON at all) must never 500 or panic. Today this never even
/// reaches `a2a_handler` — axum's `Json<JsonRpcRequest>` extractor rejects it first — so the
/// response is axum's generic `400 Bad Request` rather than a JSON-RPC-shaped `{"error": {...}}`
/// payload. That is a real, if minor, gap against a strict JSON-RPC 2.0 server contract (a
/// conforming server parses the transport body itself and answers a `-32700 Parse error` envelope),
/// but reshaping it requires a custom extractor/rejection handler — more than the "small, obvious"
/// fix bar for this coverage story. Recorded as a follow-up candidate in the C-41 story Notes
/// (`crates/flux-server/src/a2a.rs:207` `a2a_handler`'s `Json<JsonRpcRequest>` parameter — see the
/// `#[ignore]`d `garbage_body_yields_a_json_rpc_parse_error_envelope` below for the failing-first
/// test that pins the *ideal* shape). This test pins today's actual (safe, non-JSON-RPC-shaped)
/// behavior: never a 500, never a panic.
#[tokio::test]
async fn garbage_body_never_500s_or_panics() {
    let app = support::app(None);
    let (status, body) =
        support::post_raw(app, "/a2a", "application/json", "{ this is not json at all").await;

    // Pin the actual current behavior precisely (axum's `Json` extractor rejection: 400, plain
    // text) rather than a loose "any 4xx" so a future change here — either a real fix reshaping
    // this into a JSON-RPC `-32700` envelope, or an accidental regression — shows up as a failing
    // assertion instead of silently drifting.
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(
        !body.trim_start().starts_with('{'),
        "today's rejection body is axum's plain-text message, not a JSON-RPC error object: {body}"
    );
}

/// The *ideal*, spec-conforming counterpart to the pin above: a strict JSON-RPC 2.0 server parses
/// the transport body itself and answers a `-32700 Parse error` envelope (HTTP 200 — JSON-RPC
/// errors are a payload, the same convention every other error path in `a2a_handler` already
/// follows), instead of leaking axum's generic extractor-rejection text. Ignored because closing
/// this gap needs a custom extractor or a rejection-mapping layer around `a2a_handler`'s
/// `Json<JsonRpcRequest>` parameter (`crates/flux-server/src/a2a.rs:207`) — more than the "small,
/// obvious" fix bar for a coverage-only story (C-41). Un-ignore once that lands; until then this is
/// the failing-first test a follow-up story can pick up directly.
#[ignore = "known gap, not fixed by C-41: garbage /a2a bodies get axum's generic 400 today, not a \
            JSON-RPC -32700 envelope. See crates/flux-server/src/a2a.rs:207 \
            (a2a_handler's Json<JsonRpcRequest> extractor) and the C-41 story Notes."]
#[tokio::test]
async fn garbage_body_yields_a_json_rpc_parse_error_envelope() {
    let app = support::app(None);
    let (status, body) =
        support::post_raw(app, "/a2a", "application/json", "{ this is not json at all").await;

    assert_eq!(
        status,
        StatusCode::OK,
        "JSON-RPC errors are a payload, not a transport-level failure"
    );
    let res: serde_json::Value = serde_json::from_str(&body)
        .expect("a JSON-RPC error envelope, not axum's plain-text rejection");
    assert_eq!(res["jsonrpc"], "2.0");
    assert_eq!(
        res["error"]["code"], -32700,
        "the standard JSON-RPC \"Parse error\" code"
    );
}
