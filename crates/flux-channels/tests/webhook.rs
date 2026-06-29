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
