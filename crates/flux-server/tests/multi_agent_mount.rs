//! D-63: resolver-keyed multi-agent A2A mount — N agents under `/:agent_id/`, each with flux's
//! own A2A session machinery (TTL, `contextId` continuity, SSE), one shared auth layer.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use flux_auth::request::{AuthContext, AuthError, RequestAuthenticator};
use flux_server::{
    router_multi_in, CardInfo, PrincipalAuth, ResolvedAgent, ServerAuth, StaticResolver,
};

use support::{test_engine, ProseProvider};

/// Two agents ("support" @ acme-ish, "sales") behind a static resolver, open auth.
fn two_agent_app() -> Router {
    let support = test_engine(Arc::new(ProseProvider));
    let sales = test_engine(Arc::new(ProseProvider));
    let resolver = StaticResolver::new()
        .with_agent("support", support, CardInfo::for_agent("support", None))
        .with_agent("sales", sales, CardInfo::for_agent("sales", None));
    router_multi_in(
        Arc::new(resolver),
        ServerAuth::Open,
        "127.0.0.1:0".parse().unwrap(),
        &crate::support::pinned_env(),
    )
    .unwrap()
}

fn send_body(text: &str, context_id: Option<&str>) -> Value {
    let mut message = json!({ "parts": [{ "kind": "text", "text": text }] });
    if let Some(cid) = context_id {
        message["contextId"] = json!(cid);
    }
    // Blocking send: these tests assert the synchronous completed-Task shape (A-54 makes
    // non-blocking the default).
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "message/send",
        "params": { "message": message, "configuration": { "blocking": true } },
    })
}

async fn get_json(app: Router, path: &str) -> (StatusCode, Value) {
    let res = app
        .oneshot(HttpRequest::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

async fn post_a2a(app: Router, agent: &str, body: Value) -> (StatusCode, Value) {
    let res = app
        .oneshot(
            HttpRequest::post(format!("/{agent}/a2a"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Each agent's card is served at its own path and advertises its own `/:agent_id/a2a` endpoint.
#[tokio::test]
async fn each_agent_card_advertises_its_own_endpoint() {
    let (s1, support) = get_json(two_agent_app(), "/support/.well-known/agent-card.json").await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(support["name"], "support");
    assert!(
        support["url"].as_str().unwrap().ends_with("/support/a2a"),
        "card url must point at this agent's mounted endpoint: {}",
        support["url"]
    );

    let (_, sales) = get_json(two_agent_app(), "/sales/.well-known/agent.json").await;
    assert_eq!(sales["name"], "sales");
    assert!(sales["url"].as_str().unwrap().ends_with("/sales/a2a"));
}

/// An unknown agent id is a constant 404 on both the card and the RPC endpoint (§13.1).
#[tokio::test]
async fn unknown_agent_is_constant_404() {
    let (s1, _) = get_json(two_agent_app(), "/ghost/.well-known/agent-card.json").await;
    assert_eq!(s1, StatusCode::NOT_FOUND);
    let (s2, _) = post_a2a(two_agent_app(), "ghost", send_body("hi", None)).await;
    assert_eq!(s2, StatusCode::NOT_FOUND);
}

/// Each agent runs turns on its OWN engine/store — a task sent to `support` lands only in
/// support's sessions, never sales'.
#[tokio::test]
async fn agents_are_isolated_by_path() {
    let support = test_engine(Arc::new(ProseProvider));
    let sales = test_engine(Arc::new(ProseProvider));
    let s_events = support.events.clone();
    let sales_events = sales.events.clone();
    let resolver = StaticResolver::new()
        .with_agent("support", support, CardInfo::for_agent("support", None))
        .with_agent("sales", sales, CardInfo::for_agent("sales", None));
    let app = router_multi_in(
        Arc::new(resolver),
        ServerAuth::Open,
        "127.0.0.1:0".parse().unwrap(),
        &crate::support::pinned_env(),
    )
    .unwrap();

    let (status, body) = post_a2a(app, "support", send_body("hi", Some("ctx-1"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["error"].is_null(), "{body}");

    assert_eq!(
        s_events.list(10).unwrap().len(),
        1,
        "support ran one session"
    );
    assert_eq!(
        sales_events.list(10).unwrap().len(),
        0,
        "sales saw nothing — agents are isolated by path"
    );
}

/// `contextId` continuity works per agent: two sends with one `contextId` reuse the agent's
/// session.
#[tokio::test]
async fn contextid_continuity_within_an_agent() {
    let support = test_engine(Arc::new(ProseProvider));
    let events = support.events.clone();
    let resolver =
        StaticResolver::new().with_agent("support", support, CardInfo::for_agent("support", None));
    let app = router_multi_in(
        Arc::new(resolver),
        ServerAuth::Open,
        "127.0.0.1:0".parse().unwrap(),
        &crate::support::pinned_env(),
    )
    .unwrap();

    for _ in 0..2 {
        let (status, _) = post_a2a(app.clone(), "support", send_body("hi", Some("ctx-42"))).await;
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(
        events.list(10).unwrap().len(),
        1,
        "same contextId reused one session"
    );
}

/// A deterministic authenticator: `tok-x` → principal x@acme; anything else invalid.
struct OneUser;
#[async_trait::async_trait]
impl RequestAuthenticator for OneUser {
    async fn authenticate(&self, bearer: &str) -> Result<AuthContext, AuthError> {
        use flux_policy::{Caller, CallerKind, Principal, Trust, TrustKind, TrustLevel};
        if bearer != "tok-x" {
            return Err(AuthError::Unauthorized);
        }
        Ok(AuthContext {
            account: Some("acme".into()),
            caller: Caller {
                principal: Principal {
                    id: "x".into(),
                    name: "x".into(),
                    kind: CallerKind::User,
                },
                groups: Vec::new(),
                source: "test".into(),
            },
            trust: Trust {
                kind: TrustKind::Invocation,
                level: TrustLevel::Verified,
                scopes: Vec::new(),
            },
        })
    }
}

/// Auth composes as one outer layer: the RPC endpoint is 401 without a valid token, and the card
/// (public) declares the bearer scheme.
#[tokio::test]
async fn auth_is_one_layer_over_the_mount() {
    let support = test_engine(Arc::new(ProseProvider));
    let resolver =
        StaticResolver::new().with_agent("support", support, CardInfo::for_agent("support", None));
    let auth = ServerAuth::Principal(PrincipalAuth::new(Arc::new(OneUser), "https://x.example"));
    let app = router_multi_in(
        Arc::new(resolver),
        auth,
        "127.0.0.1:0".parse().unwrap(),
        &crate::support::pinned_env(),
    )
    .unwrap();

    // No token → 401 on the RPC endpoint.
    let (status, _) = post_a2a(app.clone(), "support", send_body("hi", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The public card declares the scheme and points at the configured external base.
    let (cs, card) = get_json(app, "/support/.well-known/agent-card.json").await;
    assert_eq!(cs, StatusCode::OK);
    assert_eq!(card["url"], "https://x.example/support/a2a");
    assert_eq!(
        card["securitySchemes"]["bearer"],
        json!({ "type": "http", "scheme": "bearer" })
    );
}

/// A resolved-agent bundle is Clone-able state (compile-time contract used by dynamic resolvers).
#[test]
fn resolved_agent_is_constructible() {
    let engine = test_engine(Arc::new(ProseProvider));
    let _r = ResolvedAgent {
        engine,
        card: Arc::new(CardInfo::for_agent("x", None)),
    };
}
