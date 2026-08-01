//! D-69: per-request principal auth — realm-scoped sessions, realm-keyed A-48 continuity,
//! per-turn envelope identity, constant-shape auth failures, and the A2A card advertisement.
//!
//! Drives the REAL router in `ServerAuth::Principal` mode with a deterministic test
//! authenticator (alice→acme, bob→globex), so every assertion exercises the production
//! middleware/guard/handler path.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use flux_auth::request::{AuthContext, AuthError, RequestAuthenticator};
use flux_flow::engine::FlowEngine;
use flux_policy::{Caller, CallerKind, Principal, Trust, TrustKind, TrustLevel};
use flux_server::{CardInfo, PrincipalAuth, ServerAuth};

use support::{post_json_auth, test_engine, ProseProvider};

/// Deterministic principals: `tok-alice` → alice@acme, `tok-bob` → bob@globex, `tok-carol` →
/// carol with NO account claim (principal-derived realm), `tok-down` → backend failure,
/// anything else → invalid token.
struct TestAuthenticator;

#[async_trait::async_trait]
impl RequestAuthenticator for TestAuthenticator {
    async fn authenticate(&self, bearer: &str) -> Result<AuthContext, AuthError> {
        let (id, account) = match bearer {
            "tok-alice" => ("alice", Some("acme")),
            "tok-bob" => ("bob", Some("globex")),
            "tok-carol" => ("carol", None),
            // An attacker whose IdP emits an account claim shaped like the principal-derived realm
            // of account-less "carol" — must NOT collide with her (finding #1 regression).
            "tok-collide" => ("attacker", Some("user:carol")),
            "tok-down" => return Err(AuthError::Unavailable("secret-internal-url".into())),
            _ => return Err(AuthError::Unauthorized),
        };
        Ok(AuthContext {
            account: account.map(str::to_string),
            caller: Caller {
                principal: Principal {
                    id: id.to_string(),
                    name: id.to_string(),
                    kind: CallerKind::User,
                },
                groups: Vec::new(),
                source: "test".to_string(),
            },
            trust: Trust {
                kind: TrustKind::Invocation,
                level: TrustLevel::Verified,
                scopes: Vec::new(),
            },
        })
    }
}

const EXTERNAL: &str = "https://agents.example.test";

fn principal_app() -> (Router, Arc<FlowEngine>) {
    let engine = test_engine(Arc::new(ProseProvider));
    let auth = ServerAuth::Principal(PrincipalAuth::new(Arc::new(TestAuthenticator), EXTERNAL));
    let app = flux_server::router_in(
        engine.clone(),
        auth,
        CardInfo::flux_coding(),
        "127.0.0.1:0".parse().unwrap(),
        &support::pinned_env(),
    )
    .unwrap();
    (app, engine)
}

fn send_params(text: &str, context_id: Option<&str>) -> Value {
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

/// Full-response GET (status + headers + body) — the constant-shape assertions need headers.
async fn get_full(
    app: Router,
    path: &str,
    auth: Option<&str>,
    extra: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut rb = HttpRequest::get(path);
    if let Some(a) = auth {
        rb = rb.header("authorization", a);
    }
    for (k, v) in extra {
        rb = rb.header(*k, *v);
    }
    let res = app.oneshot(rb.body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// Missing token, invalid token, and duplicate `Authorization` headers all yield the SAME
/// constant 401 — status, `WWW-Authenticate` challenge, and body byte-identical (no oracle).
#[tokio::test]
async fn principal_401_is_constant_with_challenge() {
    let (app, _) = principal_app();

    let (s1, h1, b1) = get_full(app.clone(), "/usage", None, &[]).await;
    let (s2, h2, b2) = get_full(app.clone(), "/usage", Some("Bearer nope"), &[]).await;
    for (s, h, b) in [(&s1, &h1, &b1), (&s2, &h2, &b2)] {
        assert_eq!(*s, StatusCode::UNAUTHORIZED);
        assert_eq!(
            h.get("www-authenticate").unwrap().to_str().unwrap(),
            "Bearer error=\"invalid_token\"",
        );
        assert_eq!(*b, "unauthorized");
    }

    // Two Authorization headers — even both valid — are rejected before extraction: a front
    // proxy honoring the other copy than we do is a smuggling-style divergence.
    let req = HttpRequest::get("/usage")
        .header("authorization", "Bearer tok-alice")
        .header("authorization", "Bearer tok-alice")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// A failing auth backend is a constant 503 — fail closed, distinguishable from 401, and the
/// backend's error detail (which can name internal endpoints) never reaches the wire.
#[tokio::test]
async fn unavailable_backend_is_constant_503() {
    let (app, _) = principal_app();
    let (status, _, body) = get_full(app, "/usage", Some("Bearer tok-down"), &[]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, "authentication backend unavailable");
    assert!(!body.contains("secret-internal-url"));
}

/// Sessions minted in principal mode carry the caller's realm, and EVERY `/sessions/:id/*` route
/// — including the write path `POST …/messages` — answers a cross-realm probe with a 404 that is
/// byte-identical to a nonexistent id (A2A §13.1: never reveal existence).
#[tokio::test]
async fn sessions_are_realm_tagged_and_cross_realm_probes_are_constant_404() {
    let (app, engine) = principal_app();

    let (status, body) = post_json_auth(
        app.clone(),
        "/sessions",
        json!({}),
        Some("Bearer tok-alice"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = body["id"].as_str().unwrap().to_string();
    assert_eq!(
        engine.events.info(&sid).unwrap().context.account.as_deref(),
        Some("acct:acme"),
        "principal-mode mints carry the caller's realm (acct:-namespaced) on the D-02 envelope"
    );

    // The owner reads it fine.
    let (status, _, _) = get_full(
        app.clone(),
        &format!("/sessions/{sid}"),
        Some("Bearer tok-alice"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Cross-realm probes: all four session routes, plus a nonexistent id — one constant shape.
    let mut shapes = Vec::new();
    for path in [
        format!("/sessions/{sid}"),
        format!("/sessions/{sid}/usage"),
        format!("/sessions/{sid}/stream?input=hi"),
        "/sessions/s_424242".to_string(), // does not exist at all
    ] {
        let (s, _, b) = get_full(app.clone(), &path, Some("Bearer tok-bob"), &[]).await;
        shapes.push((s, b));
    }
    // The WRITE path — leaving this one un-guarded would be cross-tenant read+write.
    let (s, b) = support::post_raw_auth(
        app.clone(),
        &format!("/sessions/{sid}/messages"),
        "application/json",
        &json!({ "input": "hi" }).to_string(),
        Some("Bearer tok-bob"),
    )
    .await;
    shapes.push((s, b));

    for (s, b) in &shapes {
        assert_eq!(*s, StatusCode::NOT_FOUND);
        assert_eq!(b, "not found", "constant body — no existence oracle");
    }
}

/// A-48 continuity is realm-keyed: the same `contextId` under two realms yields two isolated
/// sessions (the spec says contextId is a grouping key, NOT a security boundary), while the same
/// realm's repeat send still reuses its own session.
#[tokio::test]
async fn contextid_continuity_is_realm_keyed() {
    let (app, engine) = principal_app();

    for tok in ["Bearer tok-alice", "Bearer tok-bob", "Bearer tok-alice"] {
        let (status, body) = post_json_auth(
            app.clone(),
            "/a2a",
            send_params("hi", Some("ctx-shared")),
            Some(tok),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "send failed: {body}");
        assert!(body["error"].is_null(), "rpc error: {body}");
    }

    let a2a_sessions: Vec<_> = engine
        .events
        .list(10)
        .unwrap()
        .into_iter()
        .filter(|s| s.context.correlation_id.as_deref() == Some("ctx-shared"))
        .collect();
    assert_eq!(
        a2a_sessions.len(),
        2,
        "same contextId, two realms → two sessions; alice's second send reused hers"
    );
    let mut accounts: Vec<_> = a2a_sessions
        .iter()
        .map(|s| s.context.account.clone().unwrap())
        .collect();
    accounts.sort();
    assert_eq!(accounts, ["acct:acme", "acct:globex"]);
}

/// The envelope identity follows the request principal without retargeting the shared executor.
/// Each turn's durable audit row carries its lexical caller while the assembly-time fallback stays
/// unchanged.
#[tokio::test]
async fn turns_run_under_the_request_principal() {
    let (app, engine) = principal_app();

    let (status, alice) = post_json_auth(
        app.clone(),
        "/a2a",
        send_params("hi", None),
        Some("Bearer tok-alice"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let alice_session = alice["result"]["id"].as_str().unwrap();
    let alice_callers: Vec<_> = engine
        .events
        .observations(alice_session)
        .unwrap()
        .into_iter()
        .filter(|observation| observation.kind == "turn.identity")
        .filter_map(|observation| observation.data["caller"].as_str().map(str::to_string))
        .collect();
    assert_eq!(alice_callers, ["alice"]);

    let (status, bob) = post_json_auth(
        app.clone(),
        "/a2a",
        send_params("hi", None),
        Some("Bearer tok-bob"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let bob_session = bob["result"]["id"].as_str().unwrap();
    let bob_callers: Vec<_> = engine
        .events
        .observations(bob_session)
        .unwrap()
        .into_iter()
        .filter(|observation| observation.kind == "turn.identity")
        .filter_map(|observation| observation.data["caller"].as_str().map(str::to_string))
        .collect();
    assert_eq!(bob_callers, ["bob"]);
    assert_eq!(
        engine.executor.identity().get().0.principal.id,
        "local",
        "request identities must never mutate the executor fallback"
    );
}

/// A principal without an account claim gets a principal-derived realm (`user:<id>`), not a
/// shared "no account" pool: carol's session is invisible to alice and vice versa.
#[tokio::test]
async fn accountless_principals_do_not_share_a_realm() {
    let (app, engine) = principal_app();
    let (status, body) = post_json_auth(
        app.clone(),
        "/sessions",
        json!({}),
        Some("Bearer tok-carol"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = body["id"].as_str().unwrap().to_string();
    assert_eq!(
        engine.events.info(&sid).unwrap().context.account.as_deref(),
        Some("user:carol"),
    );
    let (s, _, b) = get_full(
        app.clone(),
        &format!("/sessions/{sid}"),
        Some("Bearer tok-alice"),
        &[],
    )
    .await;
    assert_eq!((s, b.as_str()), (StatusCode::NOT_FOUND, "not found"));
}

/// Finding #1 regression: an account claim valued exactly like an account-less principal's
/// derived realm (`user:carol`) lands in the disjoint `acct:` namespace, so the attacker cannot
/// reach carol's sessions and vice versa.
#[tokio::test]
async fn account_claim_cannot_collide_with_a_user_realm() {
    let (app, engine) = principal_app();

    // Carol (no account claim) mints a session → realm `user:carol`.
    let (_, carol) = post_json_auth(
        app.clone(),
        "/sessions",
        json!({}),
        Some("Bearer tok-carol"),
    )
    .await;
    let carol_sid = carol["id"].as_str().unwrap().to_string();

    // The attacker (account claim "user:carol") mints a session → realm `acct:user:carol`.
    let (_, atk) = post_json_auth(
        app.clone(),
        "/sessions",
        json!({}),
        Some("Bearer tok-collide"),
    )
    .await;
    let atk_sid = atk["id"].as_str().unwrap().to_string();
    assert_eq!(
        engine
            .events
            .info(&atk_sid)
            .unwrap()
            .context
            .account
            .as_deref(),
        Some("acct:user:carol"),
        "the account realm is acct:-namespaced, disjoint from user: realms"
    );

    // Neither can read the other's session (constant 404 both ways).
    let (s1, _, _) = get_full(
        app.clone(),
        &format!("/sessions/{carol_sid}"),
        Some("Bearer tok-collide"),
        &[],
    )
    .await;
    let (s2, _, _) = get_full(
        app.clone(),
        &format!("/sessions/{atk_sid}"),
        Some("Bearer tok-carol"),
        &[],
    )
    .await;
    assert_eq!(
        s1,
        StatusCode::NOT_FOUND,
        "attacker cannot read carol's session"
    );
    assert_eq!(
        s2,
        StatusCode::NOT_FOUND,
        "carol cannot read attacker's session"
    );
}

/// Finding #8 regression: a request with two `Authorization` headers is rejected in shared-secret
/// mode (not just principal mode).
#[tokio::test]
async fn shared_secret_rejects_duplicate_authorization_headers() {
    let app = support::app(Some("s3cr3t".into()));
    // One correct header → OK.
    let ok = HttpRequest::get("/usage")
        .header("authorization", "Bearer s3cr3t")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(ok).await.unwrap().status(),
        StatusCode::OK
    );
    // Two headers, both correct → rejected (smuggling divergence).
    let dup = HttpRequest::get("/usage")
        .header("authorization", "Bearer s3cr3t")
        .header("authorization", "Bearer s3cr3t")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.oneshot(dup).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

/// Finding #3 regression: shared-secret mode with a configured external base advertises it on the
/// card (not the request Host header), closing the secret-phishing vector.
#[tokio::test]
async fn shared_secret_card_uses_configured_external_url() {
    let engine = test_engine(Arc::new(ProseProvider));
    let auth = ServerAuth::shared_secret(
        Some("s3cr3t".into()),
        Some("https://trusted.example".into()),
    );
    let app = flux_server::router_in(
        engine,
        auth,
        CardInfo::flux_coding(),
        "127.0.0.1:0".parse().unwrap(),
        &support::pinned_env(),
    )
    .unwrap();
    let (status, _, body) = get_full(
        app,
        "/.well-known/agent-card.json",
        None,
        &[("host", "evil.example.com")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let card: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(card["url"], "https://trusted.example/a2a");
}

/// The card declares the bearer scheme whenever auth is enabled (the spec has clients pick from
/// the card's declared schemes) — and in principal mode its `url` comes from the configured
/// external base, never the request's Host header (the card is public and tells clients where to
/// send tokens: Host-derivation would be a phishing primitive).
#[tokio::test]
async fn card_declares_scheme_and_uses_configured_external_url() {
    // Principal mode: scheme declared, url is config-derived even under a hostile Host header.
    let (app, _) = principal_app();
    let (status, _, body) = get_full(
        app,
        "/.well-known/agent-card.json",
        None,
        &[("host", "evil.example.com")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let card: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(card["url"], format!("{EXTERNAL}/a2a"));
    assert_eq!(
        card["securitySchemes"]["bearer"],
        json!({ "type": "http", "scheme": "bearer" })
    );
    assert_eq!(card["security"], json!([{ "bearer": [] }]));

    // Shared-secret mode: scheme declared too (auth is enabled), url keeps host derivation.
    let app = support::app(Some("s3cr3t".into()));
    let (_, _, body) = get_full(app, "/.well-known/agent-card.json", None, &[]).await;
    let card: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        card["securitySchemes"]["bearer"],
        json!({ "type": "http", "scheme": "bearer" })
    );

    // Open mode: nothing declared (and the key is absent, not null — byte-stable legacy cards).
    let app = support::app(None);
    let (_, _, body) = get_full(app, "/.well-known/agent-card.json", None, &[]).await;
    assert!(!body.contains("securitySchemes"));
}

/// `GET /usage` in principal mode returns only the caller's realm's spend (§13.1 again — another
/// tenant's usage is as secret as their transcripts).
#[tokio::test]
async fn usage_is_realm_scoped() {
    let (app, engine) = principal_app();

    let mint = |tok: &'static str| {
        let app = app.clone();
        async move {
            let (_, body) = post_json_auth(app, "/sessions", json!({}), Some(tok)).await;
            body["id"].as_str().unwrap().to_string()
        }
    };
    let sid_a = mint("Bearer tok-alice").await;
    let sid_b = mint("Bearer tok-bob").await;

    for (sid, model) in [(&sid_a, "model-alice"), (&sid_b, "model-bob")] {
        let turn = engine.events.begin_turn(sid, "hi", model).unwrap();
        engine
            .events
            .record_call_usage(
                sid,
                turn,
                model,
                flux_core::Usage {
                    input_tokens: 1000,
                    output_tokens: 100,
                    ..Default::default()
                },
            )
            .unwrap();
        engine
            .events
            .end_turn(sid, turn, "accepted", 1, "done", None)
            .unwrap();
    }

    let (status, _, body) = get_full(app, "/usage", Some("Bearer tok-alice"), &[]).await;
    assert_eq!(status, StatusCode::OK);
    let usage: Value = serde_json::from_str(&body).unwrap();
    let models: Vec<_> = usage["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["model"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(models, ["model-alice"], "only the caller's realm's rows");
}

/// A-54: the stateful task surface is realm-scoped — a task minted by alice resolves for alice,
/// while bob's probe of the same id answers the SAME constant `-32001` an unknown id gets, across
/// every task method (get, cancel, resubscribe, push-config). Cross-realm existence is never
/// distinguishable.
#[tokio::test]
async fn task_surface_is_realm_scoped_with_constant_not_found() {
    let (app, _) = principal_app();

    // alice mints + completes a task (blocking send); task id = her session id.
    let (_, minted) = post_json_auth(
        app.clone(),
        "/a2a",
        send_params("hi", Some("ctx-realm-task")),
        Some("Bearer tok-alice"),
    )
    .await;
    let task_id = minted["result"]["id"].as_str().unwrap().to_string();

    // alice reads her task back through `tasks/get`.
    let task_rpc = |method: &str, id: &str| {
        json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": { "id": id },
        })
    };
    let (_, mine) = post_json_auth(
        app.clone(),
        "/a2a",
        task_rpc("tasks/get", &task_id),
        Some("Bearer tok-alice"),
    )
    .await;
    assert_eq!(
        mine["result"]["status"]["state"], "completed",
        "own-realm get resolves: {mine}"
    );

    // bob's probes of alice's task: one constant -32001 across the whole surface…
    for method in [
        "tasks/get",
        "tasks/cancel",
        "tasks/resubscribe",
        "tasks/pushNotificationConfig/list",
    ] {
        let (_, res) = post_json_auth(
            app.clone(),
            "/a2a",
            task_rpc(method, &task_id),
            Some("Bearer tok-bob"),
        )
        .await;
        assert_eq!(
            res["error"]["code"], -32001,
            "{method} cross-realm probe is constant not-found: {res}"
        );
    }
    // …indistinguishable from a genuinely unknown id.
    let (_, unknown) = post_json_auth(
        app.clone(),
        "/a2a",
        task_rpc("tasks/get", "s_424242"),
        Some("Bearer tok-bob"),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32001);
}
