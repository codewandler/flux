//! End-to-end: a fast cron channel wakes a real `App`, whose trigger runs a journey that reads the
//! seeded event payload. Proves timer → deliver → bus → trigger → journey → result, with no provider.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use flux_app::{App, JourneyRun};
use flux_channels::{build_channels, AppDeliverer, Deliverer};
use flux_lang::program::Module;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Wraps the real `AppDeliverer`, recording every journey run so the test can observe what the cron
/// tick actually produced.
struct Tee {
    inner: AppDeliverer,
    recorded: Mutex<Vec<JourneyRun>>,
}

#[async_trait]
impl Deliverer for Tee {
    async fn deliver(&self, label: &str, payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
        let runs = self.inner.deliver(label, payload).await?;
        self.recorded.lock().await.extend(runs.clone());
        Ok(runs)
    }
}

#[tokio::test]
async fn cron_tick_runs_journey_via_app() {
    // A `ticker` schedule channel → trigger → a journey that formats the seeded `{name}` payload field.
    let src = serde_json::to_string(&json!({
        "channels": [{ "name": "ticker", "kind": "schedule", "settings": { "schedule": "* * * * * *" } }],
        "triggers": [{ "name": "t", "on": "ticker", "run": "tick" }],
        "journeys": [{
            "name": "tick",
            "flow": { "name": "tick", "body": [
                { "kind": "bind", "name": "r", "value": { "kind": "fmt", "template": "ran for {name}" } },
                { "kind": "return", "value": { "kind": "var", "name": "r" } }
            ] }
        }]
    }))
    .unwrap();
    let program = match Module::parse_str(&src).unwrap() {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    };
    let channel_decls = program.channels.clone();
    let app = Arc::new(App::with_options(program, None, "mock", true));
    let tee = Arc::new(Tee {
        inner: AppDeliverer::new(app),
        recorded: Mutex::new(Vec::new()),
    });

    let ch = build_channels(&channel_decls).unwrap().remove(0);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let d: Arc<dyn Deliverer> = tee.clone();
    let handle = tokio::spawn(async move { ch.start(d, c2).await });

    tokio::time::sleep(Duration::from_millis(1500)).await;
    cancel.cancel();
    let _ = handle.await;

    let recorded = tee.recorded.lock().await;
    assert!(
        recorded
            .iter()
            .any(|r| r.journey == "tick" && r.result.trim() == "ran for ticker"),
        "expected the tick journey to run with the seeded payload; got {recorded:?}"
    );
}
