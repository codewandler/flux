//! Environment-gated live proof for the generated Asterisk ARI event channel.
//!
//! Set `FLUX_TEST_ASTERISK_ARI_WS_URL` to the complete `/ari/events` WebSocket URL and supply
//! `FLUX_TEST_ASTERISK_ARI_APP`, `FLUX_TEST_ASTERISK_ARI_USER`, and
//! `FLUX_TEST_ASTERISK_ARI_PASSWORD`. While the test is running, create and destroy one channel in
//! that Stasis application. With no URL configured the test is deliberately inert, so the ordinary
//! workspace gate still compiles the live seam without reaching a network.

use std::collections::BTreeSet;
use std::time::Duration;

use base64::Engine as _;
use flux_system::port::GuardedNetwork as _;
use flux_system::websocket::{WebSocketConnect, WebSocketEvent};

#[tokio::test]
async fn live_asterisk_observes_a_channel_lifecycle_and_cancellation_closes_the_socket() {
    let Ok(endpoint) = std::env::var("FLUX_TEST_ASTERISK_ARI_WS_URL") else {
        return;
    };
    let app = required("FLUX_TEST_ASTERISK_ARI_APP");
    let user = required("FLUX_TEST_ASTERISK_ARI_USER");
    let password = required("FLUX_TEST_ASTERISK_ARI_PASSWORD");

    let mut url = url::Url::parse(&endpoint).expect("FLUX_TEST_ASTERISK_ARI_WS_URL is a URL");
    assert!(
        matches!(url.scheme(), "ws" | "wss"),
        "FLUX_TEST_ASTERISK_ARI_WS_URL must use ws or wss"
    );
    assert!(
        url.path().ends_with("/events"),
        "FLUX_TEST_ASTERISK_ARI_WS_URL must name the ARI /events endpoint"
    );
    url.query_pairs_mut()
        .append_pair("app", &app)
        .append_pair("subscribeAll", "false");

    let authorization =
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    let mut connect = WebSocketConnect::new(url.to_string());
    connect
        .headers
        .push(("Authorization".into(), format!("Basic {authorization}")));

    let host = url.host_str().expect("ARI URL has a host").to_owned();
    let allow = flux_system::net::PrivateNetAllow::from_hosts([host]);
    let workspace =
        flux_system::Workspace::new(std::env::current_dir().expect("live test current directory"))
            .expect("live test workspace");
    let system = flux_system::System::new(workspace);
    let mut socket = system
        .open_websocket_scoped(&connect, &allow)
        .await
        .expect("guarded ARI WebSocket handshake");

    let timeout = std::env::var("FLUX_TEST_ASTERISK_ARI_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(Duration::from_secs(60), Duration::from_secs);
    tokio::time::timeout(timeout, async {
        let mut observed = BTreeSet::new();
        while observed.len() != 2 {
            let event = socket
                .read()
                .await
                .expect("read live ARI event")
                .expect("ARI socket stayed open for the lifecycle");
            let WebSocketEvent::Text(text) = event else {
                continue;
            };
            let body: serde_json::Value = serde_json::from_str(&text).expect("ARI event is JSON");
            if let Some(kind @ ("ChannelCreated" | "ChannelDestroyed")) =
                body.get("type").and_then(serde_json::Value::as_str)
            {
                observed.insert(kind.to_owned());
            }
        }
    })
    .await
    .expect("create and destroy a Stasis channel before the live-test timeout");

    // Exercise the same cancellation-to-bounded-close edge as the connector driver. A successful
    // return proves cancellation does not strand the vendor socket.
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    tokio::select! {
        _ = cancellation.cancelled() => socket.close().await.expect("bounded ARI cancellation close"),
    }
}

fn required(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} must accompany FLUX_TEST_ASTERISK_ARI_WS_URL"))
}
