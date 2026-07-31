//! Webhook adapter: a POST becomes a delivery and returns the journeys' results; `async` → 202; a
//! non-loopback bind without a token is rejected.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use flux_app::JourneyRun;
use flux_channels::{Deliverer, WebhookChannel};
use flux_lang::program::ChannelDecl;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tower::ServiceExt; // for `oneshot`

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl Deliverer for Recorder {
    async fn deliver(&self, label: &str, payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
        self.events.lock().await.push((label.to_string(), payload));
        Ok(vec![JourneyRun {
            journey: "j".to_string(),
            result: "done".to_string(),
            steps: 1,
            usage: None,
            model: "mock".to_string(),
        }])
    }
}

fn channel(settings: Value) -> WebhookChannel {
    WebhookChannel::from_decl(&ChannelDecl {
        name: "hook".to_string(),
        kind: "webhook".to_string(),
        settings,
    })
    .unwrap()
}

#[tokio::test]
async fn post_becomes_delivery_and_returns_runs() {
    let rec = Arc::new(Recorder::default());
    let app = channel(json!({ "addr": "127.0.0.1:0", "path": "/hook" })).router(rec.clone());

    let resp = app
        .oneshot(
            Request::post("/hook")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "x": 1 }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let events = rec.events.lock().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "hook");
    assert_eq!(events[0].1, json!({ "x": 1 }));
}

#[tokio::test]
async fn async_mode_returns_202() {
    let rec = Arc::new(Recorder::default());
    let app = channel(json!({ "addr": "127.0.0.1:0", "path": "/hook", "async": true })).router(rec);

    let resp = app
        .oneshot(
            Request::post("/hook")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[test]
fn non_loopback_requires_token() {
    let err = WebhookChannel::from_decl(&ChannelDecl {
        name: "hook".to_string(),
        kind: "webhook".to_string(),
        settings: json!({ "addr": "0.0.0.0:8790", "path": "/hook" }),
    })
    .err()
    .expect("non-loopback bind without a token must be rejected");
    assert!(err.to_string().contains("token"), "got: {err}");
}

fn refusal(settings: Value) -> String {
    WebhookChannel::from_decl(&ChannelDecl {
        name: "hook".to_string(),
        kind: "webhook".to_string(),
        settings,
    })
    .err()
    .expect("an empty token must be refused")
    .to_string()
}

/// **An empty `token` is refused before a port is bound — on loopback too.**
///
/// It is not an absent token, it is a *worse* one: `token.is_none()` is what the non-loopback guard
/// tests, so `Some("")` sails through it and the public bind is permitted, while the handler then
/// compares the presented token (`""` when no `Authorization` header is sent) against the expected
/// one and finds them equal.
///
/// The refusal is asserted on **both** binds deliberately. The non-loopback case is the exposure; the
/// loopback case is the one that would otherwise ship an operator a channel they believe is
/// authenticated, one `addr` edit away from being public. Whitespace-only counts as empty — `" "` is
/// not a token anybody meant to configure.
#[test]
fn an_empty_token_is_refused_before_a_port_is_bound() {
    for token in ["", " ", "\t\n"] {
        for addr in ["127.0.0.1:0", "0.0.0.0:8790"] {
            let text = refusal(json!({ "addr": addr, "path": "/hook", "token": token }));
            assert!(
                text.contains("set but empty"),
                "the refusal must name the cause, got: {text}"
            );
            assert!(
                text.contains("no `Authorization` header at all"),
                "the refusal says exactly what an empty token would admit: {text}"
            );
        }
    }
}

/// The same value by the longer route an operator actually reaches by accident: `token secret "K"`
/// with `K` exported **set-and-empty**. Nobody types `""` on this path, which is what makes it the
/// dangerous spelling.
///
/// The premise is asserted rather than assumed: `flux_app::resolve_secrets` resolves a
/// `{"$secret":"K"}` marker through `std::env::var`, and `std::env::var` on a set-but-empty variable
/// returns `Ok("")` — **not** `Err(NotPresent)`. So the marker becomes the string `""` and the
/// settings deserialize to `Some("")`, landing on exactly the hole above with the operator believing
/// the channel is token-protected. `from_decl` is the same refusal either way, because by the time it
/// runs, a resolved secret and a literal are the same value.
#[test]
fn a_set_but_empty_secret_env_var_is_refused_too() {
    let key = "FLUX_TEST_C317_EMPTY_WEBHOOK_TOKEN";
    std::env::set_var(key, "");
    assert_eq!(
        std::env::var(key).as_deref(),
        Ok(""),
        "the premise of the whole defect: a set-but-empty env var resolves, it does not report \
         itself absent — so `secret \"{key}\"` becomes `Some(\"\")`, never `None`"
    );

    let resolved = std::env::var(key).expect("set above");
    let err = refusal(json!({ "addr": "0.0.0.0:8790", "path": "/hook", "token": resolved }));
    assert!(err.contains("set but empty"), "got: {err}");
    assert!(
        err.contains("secret"),
        "the refusal points at the `secret \"KEY\"` route that produced it: {err}"
    );

    std::env::remove_var(key);
}
