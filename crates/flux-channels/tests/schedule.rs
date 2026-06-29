//! Schedule adapter: a fast cron delivers one event per tick; an `on:"startup"` channel fires once.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use flux_app::JourneyRun;
use flux_channels::{build_channels, Deliverer};
use flux_lang::program::ChannelDecl;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// A `Deliverer` that records every `(label, payload)` instead of touching an `App`.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl Deliverer for Recorder {
    async fn deliver(&self, label: &str, payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
        self.events.lock().await.push((label.to_string(), payload));
        Ok(vec![])
    }
}

fn schedule_channel(settings: Value) -> Box<dyn flux_channels::Channel> {
    let decl = ChannelDecl {
        name: "ticker".to_string(),
        kind: "schedule".to_string(),
        settings,
    };
    build_channels(std::slice::from_ref(&decl))
        .unwrap()
        .remove(0)
}

#[tokio::test]
async fn fast_cron_delivers_each_tick() {
    let ch = schedule_channel(json!({ "schedule": "* * * * * *" })); // every second
    let rec = Arc::new(Recorder::default());
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let d: Arc<dyn Deliverer> = rec.clone();
    let handle = tokio::spawn(async move { ch.start(d, c2).await });

    tokio::time::sleep(Duration::from_millis(2300)).await;
    cancel.cancel();
    let _ = handle.await;

    let events = rec.events.lock().await;
    assert!(!events.is_empty(), "expected >=1 tick");
    let (label, payload) = &events[0];
    assert_eq!(label, "ticker");
    assert!(
        payload.get("at").is_some(),
        "payload carries `at`: {payload}"
    );
    assert_eq!(payload.get("name").and_then(Value::as_str), Some("ticker"));
}

#[tokio::test]
async fn startup_channel_fires_once() {
    let ch = schedule_channel(json!({ "on": "startup" }));
    let rec = Arc::new(Recorder::default());
    // A startup channel fires immediately and returns, so `start` completes on its own.
    ch.start(rec.clone(), CancellationToken::new())
        .await
        .unwrap();

    let events = rec.events.lock().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "ticker");
}

#[test]
fn five_field_crontab_is_accepted() {
    // The `cron` crate needs a seconds field; a 5-field crontab is normalized, so this must build.
    let _ = schedule_channel(json!({ "schedule": "0 9 * * *" }));
}
