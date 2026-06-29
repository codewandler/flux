//! Host lifecycle: a channel that finishes on its own (a one-shot `startup` schedule) must not tear
//! down the long-running channels; the host runs until cancel.

use std::sync::Arc;
use std::time::Duration;

use flux_app::App;
use flux_channels::{build_channels, serve};
use flux_lang::program::Module;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn one_shot_channel_does_not_stop_the_host() {
    // Two channels: a one-shot `startup` schedule (finishes immediately) and a per-second cron (runs
    // until cancel). The host must keep running after the one-shot ends.
    let src = serde_json::to_string(&json!({
        "channels": [
            { "name": "boot", "kind": "schedule", "settings": { "on": "startup" } },
            { "name": "tick", "kind": "schedule", "settings": { "schedule": "* * * * * *" } }
        ],
        "triggers": [{ "name": "t", "on": "tick", "run": "noop" }],
        "journeys": [{
            "name": "noop",
            "flow": { "name": "noop", "body": [
                { "kind": "return", "value": { "kind": "lit", "value": "" } }
            ] }
        }]
    }))
    .unwrap();
    let program = match Module::parse_str(&src).unwrap() {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    };
    let decls = program.channels.clone();
    let app = Arc::new(App::with_options(program, None, "mock", true));
    let channels = build_channels(&decls).unwrap();

    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let handle = tokio::spawn(async move { serve(app, channels, false, c2).await });

    // The `boot` one-shot finishes within milliseconds; the host must still be up for `tick`.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        !handle.is_finished(),
        "serve exited early after the one-shot `startup` channel finished"
    );

    // A cancel shuts it down cleanly.
    cancel.cancel();
    let res = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("serve did not shut down within 2s of cancel")
        .expect("serve task panicked");
    assert!(res.is_ok(), "serve returned an error: {res:?}");
}
