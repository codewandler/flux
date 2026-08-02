#[path = "../src/ari.rs"]
mod ari;

use host_kit::{MockHost, PluginBuilder, PluginHandler, WebSocketEvent};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const RECORDING_OPERATIONS: [&str; 12] = [
    "asterisk.ari.recordings.cancel",
    "asterisk.ari.recordings.copyStored",
    "asterisk.ari.recordings.deleteStored",
    "asterisk.ari.recordings.getLive",
    "asterisk.ari.recordings.getStored",
    "asterisk.ari.recordings.getStoredFile",
    "asterisk.ari.recordings.listStored",
    "asterisk.ari.recordings.mute",
    "asterisk.ari.recordings.pause",
    "asterisk.ari.recordings.stop",
    "asterisk.ari.recordings.unmute",
    "asterisk.ari.recordings.unpause",
];

fn plugin() -> host_kit::Plugin {
    ari::register_recordings_and_events(PluginBuilder::new("asterisk-test", "0.0.0"))
        .expect("register complete ARI contracts")
        .try_build()
        .expect("build complete ARI plugin")
}

fn http_payload(mock: &MockHost) -> Value {
    mock.calls
        .borrow()
        .iter()
        .find(|(command, _)| command == "http.do")
        .expect("HTTP delegation")
        .1
        .clone()
}

#[test]
fn all_twelve_recordings_are_generated_rest_operations_and_are_registered_once() {
    let manifest =
        ari::register_recordings_and_events(PluginBuilder::new("asterisk-test", "0.0.0"))
            .expect("register complete ARI contracts")
            .manifest();
    let registered = manifest
        .operations
        .iter()
        .filter(|operation| operation.name.starts_with("asterisk.ari.recordings."))
        .map(|operation| operation.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(registered, RECORDING_OPERATIONS.into_iter().collect());

    let sources = ari::source_operations().expect("generated source facts");
    assert_eq!(
        sources.len(),
        109,
        "the two-way source census must not move"
    );
    let source_recordings = sources
        .iter()
        .filter(|operation| operation["resource"] == "recordings")
        .map(|operation| {
            format!(
                "asterisk.ari.recordings.{}",
                operation["nickname"].as_str().unwrap()
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        source_recordings,
        RECORDING_OPERATIONS
            .map(str::to_string)
            .into_iter()
            .collect()
    );

    let controls = manifest
        .operations
        .iter()
        .filter(|operation| {
            operation.name == ari::EVENT_READ_CONTROL || operation.name == ari::EVENT_CLOSE_CONTROL
        })
        .collect::<Vec<_>>();
    assert_eq!(controls.len(), 2);
    assert!(controls.iter().all(|operation| operation
        .description
        .contains("not an Asterisk Swagger operation")));
}

#[test]
fn every_recording_rest_shape_delegates_method_path_auth_and_endpoint_reference() {
    let cases = [
        (
            "asterisk.ari.recordings.cancel",
            json!({"recordingName":"demo"}),
            "DELETE",
            "recordings/live/demo",
        ),
        (
            "asterisk.ari.recordings.copyStored",
            json!({"recordingName":"demo", "destinationRecordingName":"archive/demo"}),
            "POST",
            "recordings/stored/demo/copy?destinationRecordingName=archive%2Fdemo",
        ),
        (
            "asterisk.ari.recordings.deleteStored",
            json!({"recordingName":"demo"}),
            "DELETE",
            "recordings/stored/demo",
        ),
        (
            "asterisk.ari.recordings.getLive",
            json!({"recordingName":"demo"}),
            "GET",
            "recordings/live/demo",
        ),
        (
            "asterisk.ari.recordings.getStored",
            json!({"recordingName":"demo"}),
            "GET",
            "recordings/stored/demo",
        ),
        (
            "asterisk.ari.recordings.listStored",
            json!({}),
            "GET",
            "recordings/stored",
        ),
        (
            "asterisk.ari.recordings.mute",
            json!({"recordingName":"demo"}),
            "POST",
            "recordings/live/demo/mute",
        ),
        (
            "asterisk.ari.recordings.pause",
            json!({"recordingName":"demo"}),
            "POST",
            "recordings/live/demo/pause",
        ),
        (
            "asterisk.ari.recordings.stop",
            json!({"recordingName":"demo"}),
            "POST",
            "recordings/live/demo/stop",
        ),
        (
            "asterisk.ari.recordings.unmute",
            json!({"recordingName":"demo"}),
            "DELETE",
            "recordings/live/demo/mute",
        ),
        (
            "asterisk.ari.recordings.unpause",
            json!({"recordingName":"demo"}),
            "DELETE",
            "recordings/live/demo/pause",
        ),
    ];

    for (operation, input, method, path) in cases {
        let mut mock = MockHost::default()
            .with_endpoint_ref("asterisk.ari", "http://localhost:8088/ari/")
            .with_http(path, json!({"future_vendor_field": true}));
        plugin()
            .call(operation, input, &mut mock)
            .unwrap_or_else(|error| panic!("{operation}: {error}"));
        let payload = http_payload(&mock);
        assert_eq!(payload["method"], method, "{operation}");
        assert_eq!(payload["path"], path, "{operation}");
        assert_eq!(payload["endpoint_ref"], "asterisk.ari", "{operation}");
        assert_eq!(payload["auth_purpose"], "ari_basic", "{operation}");
        assert!(payload.get("url").is_none(), "{operation} learned a URL");
    }
}

#[test]
fn stored_file_download_stays_in_the_bounded_host_blob_path() {
    let raw = vec![0, 159, 255, 17];
    let mut mock = MockHost::default()
        .with_endpoint_ref("asterisk.ari", "http://localhost:8088/ari/")
        .with_http_bytes("recordings/stored/demo/file", raw.clone());
    let output = plugin()
        .call(
            "asterisk.ari.recordings.getStoredFile",
            json!({"recordingName":"demo"}),
            &mut mock,
        )
        .expect("stored recording blob receipt");
    assert_eq!(output["size"], raw.len());
    let blob_ref = output["blob_ref"].as_str().expect("opaque blob reference");
    assert_eq!(mock.blobs.borrow()[blob_ref].1, raw);

    let payload = http_payload(&mock);
    assert_eq!(payload["endpoint_ref"], "asterisk.ari");
    assert_eq!(payload["auth_purpose"], "ari_basic");
    assert_eq!(payload["response_blob"]["max_bytes"], 256 * 1024 * 1024);
    assert_eq!(payload["timeout_ms"], 30_000);
    assert!(payload.get("response_binary").is_none());
}

#[test]
fn event_websocket_delegates_ref_and_auth_and_exposes_only_bounded_typed_lifecycle() {
    let channel_event = json!({
        "type": "ChannelStateChange",
        "application": "support",
        "timestamp": "2026-08-02T10:30:00Z",
        "channel": {"id":"channel-1", "name":"PJSIP/100", "state":"Up"},
        "future_vendor_field": {"lossless": true}
    });
    let mut mock = MockHost::default()
        .with_endpoint_ref("asterisk.ari", "http://localhost:8088/ari/")
        .with_websocket_events([
            WebSocketEvent::Text(channel_event.to_string()),
            WebSocketEvent::Timeout,
            WebSocketEvent::Close {
                code: Some(1000),
                reason: "normal".into(),
            },
        ]);
    let opened = plugin()
        .call(
            "asterisk.ari.events.eventWebsocket",
            json!({"app":["support queue", "sales"], "subscribeAll":true}),
            &mut mock,
        )
        .expect("open guarded event WebSocket");
    let ws_id = opened["ws_id"].as_u64().expect("session-scoped id");

    let event = plugin()
        .call(
            ari::EVENT_READ_CONTROL,
            json!({"ws_id":ws_id, "timeout_ms":250}),
            &mut mock,
        )
        .expect("typed text event");
    assert_eq!(event, json!({"kind":"event", "event":channel_event}));
    let timeout = plugin()
        .call(
            ari::EVENT_READ_CONTROL,
            json!({"ws_id":ws_id, "timeout_ms":1}),
            &mut mock,
        )
        .expect("bounded timeout receipt");
    assert_eq!(timeout, json!({"kind":"timeout"}));
    let close = plugin()
        .call(
            ari::EVENT_READ_CONTROL,
            json!({"ws_id":ws_id, "timeout_ms":500}),
            &mut mock,
        )
        .expect("peer close receipt");
    assert_eq!(
        close,
        json!({"kind":"close", "code":1000, "reason":"normal"})
    );
    assert!(!mock.websockets.borrow().contains(&ws_id));

    let calls = mock.calls.borrow();
    let connect = calls
        .iter()
        .find(|(command, _)| command == "ws.connect")
        .expect("host websocket delegation");
    assert_eq!(connect.1["endpoint_ref"], "asterisk.ari");
    assert_eq!(connect.1["auth_purpose"], "ari_basic");
    assert_eq!(connect.1["timeout_ms"], 30_000);
    assert_eq!(
        connect.1["path"],
        "events?app=support%20queue&app=sales&subscribeAll=true"
    );
    assert!(connect.1.get("url").is_none());
    assert!(calls
        .iter()
        .all(|(command, _)| command != "ws.ping" && command != "ws.pong"));
    drop(calls);

    let manifest =
        ari::register_recordings_and_events(PluginBuilder::new("asterisk-test", "0.0.0"))
            .unwrap()
            .manifest();
    let read = manifest
        .operations
        .iter()
        .find(|operation| operation.name == ari::EVENT_READ_CONTROL)
        .expect("plugin event-read control");
    let output = read.output_schema.as_ref().expect("typed event output");
    assert_eq!(
        output["oneOf"][0]["properties"]["event"]["$ref"],
        "#/$defs/Message"
    );
    assert_eq!(
        output["$defs"]["Event"]["anyOf"].as_array().unwrap().len(),
        45
    );
}

#[test]
fn binary_event_frames_are_represented_by_the_host_then_refused_without_leaking_bytes() {
    let mut mock = MockHost::default()
        .with_endpoint_ref("asterisk.ari", "http://localhost:8088/ari/")
        .with_websocket_events([WebSocketEvent::Binary(vec![0, 159, 255])]);
    let opened = plugin()
        .call(
            "asterisk.ari.events.eventWebsocket",
            json!({"app":["support"]}),
            &mut mock,
        )
        .expect("open guarded event WebSocket");
    let ws_id = opened["ws_id"].as_u64().unwrap();
    let error = plugin()
        .call(
            ari::EVENT_READ_CONTROL,
            json!({"ws_id":ws_id, "timeout_ms":500}),
            &mut mock,
        )
        .expect_err("ARI accepts JSON text events only");
    assert!(error.contains("3-byte binary frame"), "{error}");
    assert!(error.contains("JSON text"), "{error}");
    assert!(!error.contains("AJ//"), "binary contents leaked: {error}");
    assert!(mock.websockets.borrow().contains(&ws_id));

    let closed = plugin()
        .call(
            ari::EVENT_CLOSE_CONTROL,
            json!({"ws_id":ws_id, "timeout_ms":500}),
            &mut mock,
        )
        .expect("explicit bounded cleanup");
    assert_eq!(closed, json!({"closed":true}));
    assert!(mock.websockets.borrow().is_empty());
}

#[test]
fn user_event_preserves_arbitrary_variables_as_body_data_not_authentication() {
    let variables = json!({
        "variables": {
            "api_key": "ordinary event value",
            "future": {"nested": true},
            "ticket": "42"
        }
    });
    let input = json!({
        "eventName": "ticket/updated",
        "application": "support app",
        "source": ["channel:one", "bridge:two"],
        "variables": variables
    });
    assert!(
        plugin()
            .validate_input("asterisk.ari.events.userEvent", &input)
            .problems
            .is_empty(),
        "arbitrary declared variables must remain open"
    );
    let mut mock = MockHost::default()
        .with_endpoint_ref("asterisk.ari", "http://localhost:8088/ari/")
        .with_http("events/user/ticket%2Fupdated", json!({}));
    plugin()
        .call("asterisk.ari.events.userEvent", input, &mut mock)
        .expect("publish user event");
    let payload = http_payload(&mock);
    assert_eq!(payload["auth_purpose"], "ari_basic");
    assert_eq!(
        payload["body_b64"],
        "eyJ2YXJpYWJsZXMiOnsiYXBpX2tleSI6Im9yZGluYXJ5IGV2ZW50IHZhbHVlIiwiZnV0dXJlIjp7Im5lc3RlZCI6dHJ1ZX0sInRpY2tldCI6IjQyIn19"
    );
    assert!(mock
        .calls
        .borrow()
        .iter()
        .all(|(command, _)| command != "secret"));
}
