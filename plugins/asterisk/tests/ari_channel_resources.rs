#[path = "../src/ari.rs"]
mod ari;

use host_kit::{Host, MockHost, PluginBuilder, PluginHandler, Risk};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn plugin() -> host_kit::Plugin {
    ari::register(PluginBuilder::new("asterisk-test", "0.0.0"))
        .expect("register generated ARI contracts")
        .try_build()
        .expect("build generated ARI plugin")
}

fn contract(name: &str) -> ari::AriContract {
    ari::contracts()
        .expect("generated ARI contracts")
        .into_iter()
        .find(|contract| contract.name == name)
        .unwrap_or_else(|| panic!("missing `{name}`"))
}

#[test]
fn all_thirty_five_channel_operations_match_the_vendor_contract_exactly_once() {
    let document: Value = serde_json::from_slice(
        &fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("specs/ari-22.10.1/api-docs/channels.json"),
        )
        .expect("read channels Swagger"),
    )
    .expect("valid channels Swagger");
    let expected = document["apis"]
        .as_array()
        .expect("apis")
        .iter()
        .flat_map(|api| {
            api["operations"]
                .as_array()
                .expect("operations")
                .iter()
                .map(move |operation| {
                    let parameters = operation
                        .get("parameters")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                        .iter()
                        .map(|parameter| {
                            (
                                parameter["name"].as_str().unwrap().to_string(),
                                parameter["paramType"].as_str().unwrap().to_string(),
                                parameter
                                    .get("required")
                                    .cloned()
                                    .unwrap_or(Value::Null)
                                    .to_string(),
                                parameter
                                    .get("allowMultiple")
                                    .cloned()
                                    .unwrap_or(Value::Null)
                                    .to_string(),
                                parameter["dataType"].as_str().unwrap().to_string(),
                                parameter
                                    .pointer("/allowableValues/values")
                                    .cloned()
                                    .unwrap_or_else(|| json!([]))
                                    .to_string(),
                            )
                        })
                        .collect::<Vec<_>>();
                    (
                        operation["nickname"].as_str().unwrap().to_string(),
                        operation["httpMethod"].as_str().unwrap().to_string(),
                        api["path"].as_str().unwrap().to_string(),
                        parameters,
                    )
                })
        })
        .collect::<BTreeSet<_>>();
    let actual = ari::source_operations()
        .expect("generated source facts")
        .into_iter()
        .filter(|operation| operation["resource"] == "channels")
        .map(|operation| {
            let parameters = operation["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .map(|parameter| {
                    (
                        parameter["name"].as_str().unwrap().to_string(),
                        parameter["placement"].as_str().unwrap().to_string(),
                        parameter["required"].to_string(),
                        parameter["allow_multiple"].to_string(),
                        parameter["data_type"].as_str().unwrap().to_string(),
                        parameter["enum_values"].to_string(),
                    )
                })
                .collect::<Vec<_>>();
            (
                operation["nickname"].as_str().unwrap().to_string(),
                operation["method"].as_str().unwrap().to_string(),
                operation["path"].as_str().unwrap().to_string(),
                parameters,
            )
        })
        .collect::<BTreeSet<_>>();

    assert_eq!(expected.len(), 35);
    assert_eq!(actual, expected);
    assert_eq!(
        ari::contracts()
            .unwrap()
            .iter()
            .filter(|contract| contract.name.starts_with("asterisk.ari.channels."))
            .count(),
        35
    );
}

#[test]
fn conditional_channel_rules_are_identical_in_preflight_and_dispatch() {
    let plugin = plugin();
    let cases = [
        (
            "asterisk.ari.channels.originate",
            json!({"endpoint":"PJSIP/100", "app":"support", "extension":"200"}),
            "mutually exclusive",
        ),
        (
            "asterisk.ari.channels.externalMedia",
            json!({"app":"support", "format":"ulaw", "transport":"websocket", "encapsulation":"rtp"}),
            "encapsulation",
        ),
        (
            "asterisk.ari.channels.snoopChannel",
            json!({"channelId":"live-1", "app":"support", "spy":"none", "whisper":"none"}),
            "spy or whisper",
        ),
    ];

    for (operation, input, expected) in cases {
        let report = plugin.validate_input(operation, &input);
        assert!(
            report
                .problems
                .iter()
                .any(|problem| problem.contains(expected)),
            "{operation} preflight did not report `{expected}`: {:?}",
            report.problems
        );

        let mut mock = MockHost::default();
        let error = plugin
            .call(operation, input, &mut mock)
            .expect_err("dispatch must enforce the same conditional rule");
        assert!(error.contains(expected), "{operation}: {error}");
        assert!(
            mock.calls.borrow().is_empty(),
            "{operation} reached host IO after a failed preflight"
        );
    }
}

#[test]
fn valid_originate_external_media_and_snoop_shapes_pass_preflight_and_dispatch() {
    let cases = [
        (
            "asterisk.ari.channels.originate",
            json!({"endpoint":"PJSIP/100", "app":"support", "appArgs":"ticket-42"}),
            "channels?endpoint=PJSIP%2F100",
        ),
        (
            "asterisk.ari.channels.externalMedia",
            json!({
                "app":"support",
                "format":"ulaw",
                "transport":"websocket",
                "encapsulation":"none",
                "connection_type":"server"
            }),
            "channels/externalMedia",
        ),
        (
            "asterisk.ari.channels.snoopChannel",
            json!({"channelId":"live-1", "app":"support", "spy":"in"}),
            "channels/live-1/snoop",
        ),
    ];

    for (operation, input, path) in cases {
        let plugin = plugin();
        assert!(
            plugin.validate_input(operation, &input).problems.is_empty(),
            "{operation} valid input was refused"
        );
        let mut mock = MockHost::default()
            .with_endpoint_ref("asterisk.ari", "http://localhost:8088/ari/")
            .with_http(path, json!({"id":"channel-1", "future":true}));
        let output = plugin
            .call(operation, input, &mut mock)
            .unwrap_or_else(|error| panic!("{operation}: {error}"));
        assert_eq!(output["future"], true, "{operation}");
    }
}

#[test]
fn originate_encodes_the_widest_query_body_and_preserves_the_channel_response() {
    let operation = "asterisk.ari.channels.originate";
    let mut mock = MockHost::default()
        .with_endpoint_ref("asterisk.ari", "http://localhost:8088/ari/")
        .with_http(
            "channels?endpoint=PJSIP%2Falice",
            json!({"id":"new-channel", "name":"PJSIP/alice", "future_vendor_field":42}),
        );
    let output = plugin()
        .call(
            operation,
            json!({
                "endpoint":"PJSIP/alice",
                "extension":"200",
                "context":"support queue",
                "priority":2,
                "label":"answer/next",
                "callerId":"Alice <100>",
                "timeout":45,
                "variables":{"variables":{"TICKET":"42"}},
                "channelId":"primary/id",
                "otherChannelId":"secondary/id",
                "formats":"ulaw,slin16"
            }),
            &mut mock,
        )
        .expect("originate request");
    assert_eq!(output["future_vendor_field"], 42);

    let calls = mock.calls.borrow();
    let payload = &calls
        .iter()
        .find(|(command, _)| command == "http.do")
        .expect("HTTP call")
        .1;
    let path = payload["path"].as_str().unwrap();
    for encoded in [
        "context=support%20queue",
        "label=answer%2Fnext",
        "callerId=Alice%20%3C100%3E",
        "timeout=45",
        "channelId=primary%2Fid",
        "otherChannelId=secondary%2Fid",
        "formats=ulaw%2Cslin16",
    ] {
        assert!(path.contains(encoded), "missing `{encoded}` in `{path}`");
    }
    assert_eq!(
        payload["body_b64"],
        "eyJ2YXJpYWJsZXMiOnsiVElDS0VUIjoiNDIifX0="
    );
}

#[test]
fn channel_path_body_void_and_documented_error_statuses_are_truthful() {
    let mut body_mock = MockHost::default()
        .with_endpoint_ref("asterisk.ari", "http://localhost:8088/ari/")
        .with_http("channels/live%2Fone/variables", json!({}));
    let set = contract("asterisk.ari.channels.setChannelVars");
    let receipt = ari::execute(
        &set,
        json!({"channelId":"live/one", "variables":{"variables":{"state":"ready"}}}),
        &mut Host::new(&mut body_mock),
    )
    .expect("set variables");
    assert_eq!(receipt, json!({"status":200}));
    let calls = body_mock.calls.borrow();
    let payload = &calls.iter().find(|(name, _)| name == "http.do").unwrap().1;
    assert_eq!(payload["method"], "POST");
    assert_eq!(payload["path"], "channels/live%2Fone/variables");
    assert!(payload.get("body_b64").is_some());
    drop(calls);

    for (operation, input, status, body) in [
        (
            "asterisk.ari.channels.originate",
            json!({"endpoint":"missing"}),
            400,
            "invalid originate parameters",
        ),
        (
            "asterisk.ari.channels.get",
            json!({"channelId":"missing"}),
            404,
            "channel not found",
        ),
        (
            "asterisk.ari.channels.originateWithId",
            json!({"channelId":"already", "endpoint":"PJSIP/1"}),
            409,
            "channel already exists",
        ),
    ] {
        let contract = contract(operation);
        let mut mock = MockHost::default()
            .with_endpoint_ref("asterisk.ari", "http://localhost:8088/ari/")
            .with_http_status_body("channels", status, body);
        let error = ari::execute(&contract, input, &mut Host::new(&mut mock))
            .expect_err("documented non-success response");
        assert!(error.contains(&format!("{status}: {body}")), "{error}");
    }
}

#[test]
fn live_call_mutations_are_high_and_hangup_is_explicitly_destructive() {
    let manifest = ari::register(PluginBuilder::new("asterisk-test", "0.0.0"))
        .unwrap()
        .manifest();
    for name in [
        "asterisk.ari.channels.originate",
        "asterisk.ari.channels.originateWithId",
        "asterisk.ari.channels.create",
        "asterisk.ari.channels.externalMedia",
        "asterisk.ari.channels.snoopChannel",
        "asterisk.ari.channels.snoopChannelWithId",
        "asterisk.ari.channels.play",
        "asterisk.ari.channels.record",
    ] {
        let operation = manifest
            .operations
            .iter()
            .find(|operation| operation.name == name)
            .unwrap_or_else(|| panic!("missing `{name}`"));
        assert_eq!(operation.risk, Some(Risk::High), "{name}");
    }

    let hangup = manifest
        .operations
        .iter()
        .find(|operation| operation.name == "asterisk.ari.channels.hangup")
        .expect("hangup operation");
    assert_eq!(hangup.risk, Some(Risk::Destructive));
    assert!(serde_json::to_value(&hangup.semantic_effects)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .any(|effect| effect == "delete"));
}
