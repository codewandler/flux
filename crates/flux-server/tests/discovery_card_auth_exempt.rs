//! C-41: the A2A discovery card is the one documented auth-exempt pair (besides `/health`) — it
//! must be reachable with no `Authorization` header even when a token is configured, while every
//! other route 401s without one.

mod support;

use axum::http::StatusCode;
use serde_json::Value;

const SEND_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{"parts":[{"kind":"text","text":"hi"}]}}}"#;

#[tokio::test]
async fn discovery_card_is_reachable_without_auth_while_other_routes_401() {
    let token = Some("s3cr3t".to_string());

    // The two discovery aliases: no Authorization header, still 200 — and the card is shaped as
    // expected (not just any 200).
    let (status, body) =
        support::get_raw(support::app(token.clone()), "/.well-known/agent.json", None).await;
    assert_eq!(status, StatusCode::OK);
    let card: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(card["name"], "flux");
    assert_eq!(card["capabilities"]["streaming"], true);
    assert!(
        card["url"].as_str().unwrap().ends_with("/a2a"),
        "card url: {card}"
    );

    let (status, _) = support::get_raw(
        support::app(token.clone()),
        "/.well-known/agent-card.json",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // /health is exempt too (liveness probe), for completeness.
    let (status, _) = support::get_raw(support::app(token.clone()), "/health", None).await;
    assert_eq!(status, StatusCode::OK);

    // Every other route 401s with no Authorization header presented...
    let (status, _) = support::post_raw(
        support::app(token.clone()),
        "/a2a",
        "application/json",
        SEND_BODY,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = support::get_raw(
        support::app(token.clone()),
        "/sessions/does-not-exist",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // ...and 401s just the same with a wrong bearer token.
    let (status, _) = support::post_raw_auth(
        support::app(token.clone()),
        "/a2a",
        "application/json",
        SEND_BODY,
        Some("Bearer nope"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // ...but the SAME protected route succeeds with the correct token — proving the 401s above are
    // the auth gate specifically, not some unrelated failure of the route.
    let (status, res) = support::post_json_auth(
        support::app(token.clone()),
        "/a2a",
        serde_json::from_str(SEND_BODY).unwrap(),
        Some("Bearer s3cr3t"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(res.get("error").is_none(), "unexpected error: {res}");

    let (status, _) = support::get_raw(
        support::app(token),
        "/sessions/does-not-exist",
        Some("Bearer s3cr3t"),
    )
    .await;
    // Correct token now passes the auth gate; the route itself 404s (no such session) rather than
    // 401ing — proving auth, not the handler, produced the earlier 401s.
    assert_eq!(status, StatusCode::NOT_FOUND);
}
