#[path = "../src/ari.rs"]
mod ari;

use host_kit::{Effect, Idempotency, MockHost, Plugin, PluginBuilder, PluginHandler, Risk};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

const RESOURCES: [(&str, usize); 3] = [("bridges", 18), ("playbacks", 3), ("sounds", 2)];

fn plugin() -> Plugin {
    ari::register(PluginBuilder::new("asterisk-resource-proof", "0.0.0"))
        .expect("generated registration")
        .try_build()
        .expect("valid generated plugin")
}

fn mock() -> MockHost {
    MockHost::default().with_endpoint_ref("asterisk.ari", "http://localhost:8088/ari/")
}

fn vendor_identities(resource: &str) -> BTreeSet<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("specs/ari-22.10.1/api-docs")
        .join(format!("{resource}.json"));
    let document: Value = serde_json::from_slice(&fs::read(path).expect("read resource Swagger"))
        .expect("valid resource Swagger");
    document["apis"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|api| api["operations"].as_array().unwrap())
        .map(|operation| {
            format!(
                "asterisk.ari.{resource}.{}",
                operation["nickname"].as_str().unwrap()
            )
        })
        .collect()
}

fn vendor_methods(resource: &str) -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("specs/ari-22.10.1/api-docs")
        .join(format!("{resource}.json"));
    let document: Value = serde_json::from_slice(&fs::read(path).expect("read resource Swagger"))
        .expect("valid resource Swagger");
    document["apis"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|api| api["operations"].as_array().unwrap())
        .map(|operation| {
            (
                format!(
                    "asterisk.ari.{resource}.{}",
                    operation["nickname"].as_str().unwrap()
                ),
                operation["httpMethod"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn every_bridge_playback_and_sound_operation_is_present_once() {
    let manifest = plugin().manifest();
    for (resource, expected_count) in RESOURCES {
        let prefix = format!("asterisk.ari.{resource}.");
        let generated: BTreeSet<_> = manifest
            .operations
            .iter()
            .filter(|operation| operation.name.starts_with(&prefix))
            .map(|operation| operation.name.clone())
            .collect();
        assert_eq!(
            generated.len(),
            expected_count,
            "{resource} operation count"
        );
        assert_eq!(
            generated,
            vendor_identities(resource),
            "{resource} inventory"
        );
    }
}

fn http_payload(mock: &MockHost) -> Value {
    mock.calls
        .borrow()
        .iter()
        .find(|(command, _)| command == "http.do")
        .expect("HTTP host call")
        .1
        .clone()
}

#[test]
fn bridge_and_playback_requests_encode_paths_repeated_queries_body_and_auth_exactly() {
    let plugin = plugin();

    let mut add_channel = mock().with_http("bridges/support%2F1/addChannel", json!({}));
    let receipt = plugin
        .call(
            "asterisk.ari.bridges.addChannel",
            json!({
                "bridgeId": "support/1",
                "channel": ["PJSIP/a", "PJSIP/b"],
                "role": "participant",
                "absorbDTMF": true,
                "mute": false
            }),
            &mut add_channel,
        )
        .expect("add channels");
    assert_eq!(receipt, json!({"status": 200}));
    let payload = http_payload(&add_channel);
    assert_eq!(payload["method"], "POST");
    assert_eq!(payload["endpoint_ref"], "asterisk.ari");
    assert_eq!(payload["auth_purpose"], "ari_basic");
    assert_eq!(
        payload["path"],
        "bridges/support%2F1/addChannel?channel=PJSIP%2Fa&channel=PJSIP%2Fb&role=participant&absorbDTMF=true&mute=false"
    );
    assert!(payload.get("body_b64").is_none());

    let mut variables = mock().with_http("bridges/b-1/variables", json!({}));
    plugin
        .call(
            "asterisk.ari.bridges.setBridgeVars",
            json!({"bridgeId":"b-1", "variables":{"variables":{"alpha":"1"}}}),
            &mut variables,
        )
        .expect("set bridge variables");
    let payload = http_payload(&variables);
    assert_eq!(payload["path"], "bridges/b-1/variables");
    assert_eq!(payload["headers"]["content-type"], "application/json");
    assert_eq!(payload["body_b64"], "eyJ2YXJpYWJsZXMiOnsiYWxwaGEiOiIxIn19");

    let mut control = mock().with_http("playbacks/p%2F1/control", json!({}));
    plugin
        .call(
            "asterisk.ari.playbacks.control",
            json!({"playbackId":"p/1", "operation":"forward"}),
            &mut control,
        )
        .expect("control playback");
    let payload = http_payload(&control);
    assert_eq!(payload["method"], "POST");
    assert_eq!(payload["path"], "playbacks/p%2F1/control?operation=forward");

    let mut sounds = mock().with_http("sounds?lang=en-US&format=wav", json!([]));
    plugin
        .call(
            "asterisk.ari.sounds.list",
            json!({"lang":"en-US", "format":"wav"}),
            &mut sounds,
        )
        .expect("list sounds");
    let payload = http_payload(&sounds);
    assert_eq!(payload["method"], "GET");
    assert_eq!(payload["path"], "sounds?lang=en-US&format=wav");
}

#[test]
fn every_live_media_mutation_has_its_reviewed_high_or_destructive_contract() {
    let manifest = plugin().manifest();
    let by_name: BTreeMap<_, _> = manifest
        .operations
        .iter()
        .map(|operation| (operation.name.as_str(), operation))
        .collect();

    for resource in ["bridges", "playbacks", "sounds"] {
        for (name, method) in vendor_methods(resource) {
            let operation = by_name
                .get(name.as_str())
                .unwrap_or_else(|| panic!("missing `{name}`"));
            match method.as_str() {
                "GET" => {
                    assert_eq!(operation.effects, [Effect::Read, Effect::Network], "{name}");
                    assert_eq!(operation.risk, Some(Risk::Low), "{name}");
                    assert_eq!(
                        operation.idempotency,
                        Some(Idempotency::Idempotent),
                        "{name}"
                    );
                }
                "POST" => {
                    assert_eq!(
                        operation.effects,
                        [Effect::Write, Effect::Network],
                        "{name}"
                    );
                    assert_eq!(operation.risk, Some(Risk::High), "{name}");
                    assert_eq!(
                        operation.idempotency,
                        Some(Idempotency::NonIdempotent),
                        "{name}"
                    );
                    assert!(
                        operation.semantic_effects.iter().any(|effect| {
                            serde_json::to_value(effect).expect("semantic effect") == "write_db"
                        }),
                        "{name} has no mutation semantic effect"
                    );
                }
                "DELETE" => {
                    assert_eq!(
                        operation.effects,
                        [Effect::Write, Effect::Network],
                        "{name}"
                    );
                    assert_eq!(operation.risk, Some(Risk::Destructive), "{name}");
                    assert_eq!(
                        operation.idempotency,
                        Some(Idempotency::Idempotent),
                        "{name}"
                    );
                    assert!(
                        operation.semantic_effects.iter().any(|effect| {
                            serde_json::to_value(effect).expect("semantic effect") == "delete"
                        }),
                        "{name} has no delete semantic effect"
                    );
                }
                other => panic!("unexpected `{other}` for `{name}`"),
            }
        }
    }
}

#[test]
fn model_list_and_void_fixtures_keep_the_resolved_contract_and_unknown_fields() {
    let plugin = plugin();
    let manifest = plugin.manifest();
    let operations: BTreeMap<_, _> = manifest
        .operations
        .iter()
        .map(|operation| (operation.name.as_str(), operation))
        .collect();

    let bridge_list = operations["asterisk.ari.bridges.list"]
        .output_schema
        .as_ref()
        .unwrap();
    assert_eq!(bridge_list["type"], "array");
    assert_eq!(bridge_list["items"]["$ref"], "#/$defs/Bridge");
    assert_eq!(bridge_list["$defs"].as_object().unwrap().len(), 85);
    let playback = operations["asterisk.ari.playbacks.get"]
        .output_schema
        .as_ref()
        .unwrap();
    assert_eq!(playback["$ref"], "#/$defs/Playback");
    let sound_list = operations["asterisk.ari.sounds.list"]
        .output_schema
        .as_ref()
        .unwrap();
    assert_eq!(sound_list["items"]["$ref"], "#/$defs/Sound");

    let bridges = json!([{
        "id":"b-1", "technology":"softmix", "bridge_type":"mixing",
        "bridge_class":"base", "creator":"Stasis", "name":"support",
        "channels":[], "creationtime":"2026-08-02T10:00:00Z",
        "future_vendor_field":{"codec":"opus"}
    }]);
    let mut bridge_host = mock().with_http("bridges", bridges.clone());
    assert_eq!(
        plugin
            .call("asterisk.ari.bridges.list", json!({}), &mut bridge_host)
            .expect("bridge list"),
        bridges
    );

    let playback_value = json!({
        "id":"p-1", "media_uri":"sound:hello-world", "target_uri":"bridge:b-1",
        "language":"en", "state":"playing", "future_vendor_field":true
    });
    let mut playback_host = mock().with_http("playbacks/p-1", playback_value.clone());
    assert_eq!(
        plugin
            .call(
                "asterisk.ari.playbacks.get",
                json!({"playbackId":"p-1"}),
                &mut playback_host,
            )
            .expect("playback model"),
        playback_value
    );

    let sounds = json!([{
        "id":"hello-world", "formats":[{"language":"en", "format":"wav"}],
        "future_vendor_field":["stereo"]
    }]);
    let mut sound_host = mock().with_http("sounds", sounds.clone());
    assert_eq!(
        plugin
            .call("asterisk.ari.sounds.list", json!({}), &mut sound_host)
            .expect("sound list"),
        sounds
    );

    let mut stop_host = mock().with_http("playbacks/p-1", json!({"ignored":"void"}));
    assert_eq!(
        plugin
            .call(
                "asterisk.ari.playbacks.stop",
                json!({"playbackId":"p-1"}),
                &mut stop_host,
            )
            .expect("void receipt"),
        json!({"status":200})
    );
}
