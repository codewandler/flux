//! A2A conformance quick-wins over the real router (A-49 card shape, A-50 error codes).
//!
//! These exercise the `flux-server` HTTP dispatch sites — the single-agent `a2a_handler` and the
//! resolver-keyed `a2a_handler_multi` — through the production router, so the shared
//! `flux_a2a::server` classifiers (`is_unsupported_a2a_method` / `no_text_error_code`) are proven
//! wired at every dispatch site, not just unit-tested in `flux-a2a`. The two sites share those
//! classifiers precisely so the emitted codes cannot drift between them.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use flux_a2a::AgentProvider;
use flux_server::{router_multi, CardInfo, ServerAuth, StaticResolver};

use support::{app, post_json, test_engine, ProseProvider};

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

/// A-50 (single-agent `a2a_handler`): a defined-but-unsupported method → `-32004`; a
/// genuinely-unknown method → `-32601`; a message with a part but no text → `-32005`.
#[tokio::test]
async fn error_codes_on_the_single_agent_dispatcher() {
    let (_, cancel) = post_json(app(None), "/a2a", rpc("tasks/cancel", json!({}))).await;
    assert_eq!(
        cancel["error"]["code"], -32004,
        "tasks/cancel → -32004: {cancel}"
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

/// A-50 (resolver-keyed `a2a_handler_multi`): the same classifiers govern the multi-agent mount, so
/// an unsupported method under `/:agent_id/a2a` also returns `-32004` (the shared helper is why the
/// two dispatch sites cannot drift).
#[tokio::test]
async fn unsupported_method_on_the_multi_agent_dispatcher() {
    let engine = test_engine(Arc::new(ProseProvider));
    let resolver =
        StaticResolver::new().with_agent("support", engine, CardInfo::for_agent("support", None));
    let app = router_multi(Arc::new(resolver), ServerAuth::Open);

    let res = app
        .oneshot(
            HttpRequest::post("/support/a2a")
                .header("content-type", "application/json")
                .body(Body::from(rpc("tasks/resubscribe", json!({})).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["error"]["code"], -32004,
        "multi-mount tasks/resubscribe → -32004: {body}"
    );
}
