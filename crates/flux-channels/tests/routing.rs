//! Routing: a delivered event runs the matching trigger's journey; the gate serializes deliveries.

use std::sync::Arc;

use flux_app::App;
use flux_channels::{AppDeliverer, Deliverer};
use flux_lang::program::Module;
use serde_json::json;

/// A tiny program: a `tick` trigger → a pure-op journey that returns the literal `"ok"`. No provider.
fn tick_app() -> Arc<App> {
    let src = serde_json::to_string(&json!({
        "triggers": [{ "name": "t", "on": "tick", "run": "tick" }],
        "journeys": [{
            "name": "tick",
            "flow": { "name": "tick", "body": [
                { "kind": "return", "value": { "kind": "lit", "value": "ok" } }
            ] }
        }]
    }))
    .unwrap();
    let program = match Module::parse_str(&src).unwrap() {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    };
    Arc::new(App::with_options(program, None, "mock", true))
}

#[tokio::test]
async fn delivered_event_runs_matching_journey() {
    let d = AppDeliverer::new(tick_app());
    let runs = d.deliver("tick", json!({})).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].journey, "tick");
    assert_eq!(runs[0].result.trim(), "ok");
}

#[tokio::test]
async fn unmatched_label_runs_nothing() {
    let d = AppDeliverer::new(tick_app());
    let runs = d.deliver("nope", json!({})).await.unwrap();
    assert!(runs.is_empty());
}

/// Concurrent deliveries are serialized by the gate (no panic / corruption / cross-talk): every caller
/// still gets exactly its own journey result.
#[tokio::test]
async fn concurrent_deliveries_are_serialized() {
    let d = Arc::new(AppDeliverer::new(tick_app()));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let d = d.clone();
        handles.push(tokio::spawn(
            async move { d.deliver("tick", json!({})).await },
        ));
    }
    for h in handles {
        let runs = h.await.unwrap().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].result.trim(), "ok");
    }
}
