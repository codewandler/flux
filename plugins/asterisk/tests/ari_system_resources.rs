#[path = "../src/ari.rs"]
mod ari;

use host_kit::{Host, MockHost, PluginBuilder};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const ENDPOINT_REF: &str = "asterisk.ari";
const AUTH_PURPOSE: &str = "ari_basic";

struct Fixture {
    operation: &'static str,
    method: &'static str,
    input: Value,
    path: &'static str,
    body_b64: Option<&'static str>,
}

fn model_fixture(model: &str) -> Value {
    match model {
        "Application" => json!({
            "name": "support",
            "channel_ids": ["channel-1"],
            "bridge_ids": ["bridge-1"],
            "endpoint_ids": ["PJSIP/alice"],
            "device_names": ["PJSIP/alice"],
            "events_allowed": [{"type": "StasisStart"}],
            "events_disallowed": [],
            "fixture_extension": true
        }),
        "ConfigTuple" => json!({"attribute": "allow", "value": "yes"}),
        "AsteriskInfo" => json!({
            "system": {"entity_id": "pbx-1", "version": "22.10.1"}
        }),
        "AsteriskPing" => json!({
            "asterisk_id": "pbx-1",
            "ping": "pong",
            "timestamp": "2026-08-02T12:00:00Z"
        }),
        "Module" => json!({
            "name": "res_pjsip.so",
            "description": "PJSIP",
            "use_count": 2,
            "status": "Running",
            "support_level": "core"
        }),
        "LogChannel" => json!({
            "channel": "security",
            "type": "File",
            "status": "Enabled",
            "configuration": "notice,warning"
        }),
        "Variable" => json!({"value": "alice"}),
        "Endpoint" => json!({
            "technology": "PJSIP",
            "resource": "alice",
            "state": "online",
            "channel_ids": ["channel-1"]
        }),
        "DeviceState" => json!({"name": "PJSIP/alice", "state": "INUSE"}),
        "Mailbox" => json!({"name": "1000@default", "old_messages": 7, "new_messages": 2}),
        other => panic!("no representative response fixture for `{other}`"),
    }
}

fn response_fixture(response_class: &str) -> Value {
    if response_class == "void" {
        return json!({"ignored": "void response body"});
    }
    if let Some(model) = response_class
        .strip_prefix("List[")
        .and_then(|value| value.strip_suffix(']'))
    {
        return json!([model_fixture(model)]);
    }
    model_fixture(response_class)
}

fn run_fixture(fixture: Fixture) {
    let matches: Vec<_> = ari::contracts()
        .expect("generated contracts")
        .into_iter()
        .filter(|contract| contract.name == fixture.operation)
        .collect();
    assert_eq!(matches.len(), 1, "{} contract count", fixture.operation);
    let contract = &matches[0];
    assert_eq!(
        contract.method, fixture.method,
        "{} method",
        fixture.operation
    );

    let response = response_fixture(&contract.response_class);
    let mut mock = MockHost::default()
        .with_endpoint_ref(ENDPOINT_REF, "http://localhost:8088/ari/")
        .with_http(fixture.path, response.clone());
    let output = ari::execute(contract, fixture.input, &mut Host::new(&mut mock))
        .unwrap_or_else(|error| panic!("{} fixture failed: {error}", fixture.operation));
    let expected_output = if contract.response_kind == "void" {
        json!({"status": 200})
    } else {
        response
    };
    assert_eq!(output, expected_output, "{} response", fixture.operation);

    let calls = mock.calls.borrow();
    assert_eq!(calls.len(), 1, "{} host call count", fixture.operation);
    let (command, payload) = &calls[0];
    assert_eq!(command, "http.do", "{} host command", fixture.operation);
    assert_eq!(
        payload["endpoint_ref"], ENDPOINT_REF,
        "{}",
        fixture.operation
    );
    assert_eq!(
        payload["auth_purpose"], AUTH_PURPOSE,
        "{}",
        fixture.operation
    );
    assert_eq!(payload["method"], fixture.method, "{}", fixture.operation);
    assert_eq!(payload["path"], fixture.path, "{}", fixture.operation);
    match fixture.body_b64 {
        Some(body_b64) => {
            assert_eq!(payload["body_b64"], body_b64, "{} body", fixture.operation);
            assert_eq!(
                payload["headers"]["content-type"], "application/json",
                "{} content type",
                fixture.operation
            );
        }
        None => {
            assert!(
                payload.get("body_b64").is_none(),
                "{} body",
                fixture.operation
            );
            assert!(
                payload.get("headers").is_none(),
                "{} headers",
                fixture.operation
            );
        }
    }
}

macro_rules! ari_fixtures {
    ($(
        $test:ident => {
            operation: $operation:literal,
            method: $method:literal,
            input: $input:expr,
            path: $path:literal,
            body_b64: $body_b64:expr
        }
    ),+ $(,)?) => {
        fn fixtures() -> Vec<Fixture> {
            vec![$(
                Fixture {
                    operation: $operation,
                    method: $method,
                    input: $input,
                    path: $path,
                    body_b64: $body_b64,
                }
            ),+]
        }

        $(
            #[test]
            fn $test() {
                run_fixture(Fixture {
                    operation: $operation,
                    method: $method,
                    input: $input,
                    path: $path,
                    body_b64: $body_b64,
                });
            }
        )+
    };
}

ari_fixtures! {
    applications_list => {
        operation: "asterisk.ari.applications.list",
        method: "GET",
        input: json!({}),
        path: "applications",
        body_b64: None
    },
    applications_get => {
        operation: "asterisk.ari.applications.get",
        method: "GET",
        input: json!({"applicationName": "support/app main"}),
        path: "applications/support%2Fapp%20main",
        body_b64: None
    },
    applications_subscribe => {
        operation: "asterisk.ari.applications.subscribe",
        method: "POST",
        input: json!({
            "applicationName": "support/app main",
            "eventSource": ["channel:one", "bridge/two"]
        }),
        path: "applications/support%2Fapp%20main/subscription?eventSource=channel%3Aone&eventSource=bridge%2Ftwo",
        body_b64: None
    },
    applications_unsubscribe => {
        operation: "asterisk.ari.applications.unsubscribe",
        method: "DELETE",
        input: json!({
            "applicationName": "support/app main",
            "eventSource": ["channel:one", "bridge/two"]
        }),
        path: "applications/support%2Fapp%20main/subscription?eventSource=channel%3Aone&eventSource=bridge%2Ftwo",
        body_b64: None
    },
    applications_filter => {
        operation: "asterisk.ari.applications.filter",
        method: "PUT",
        input: json!({
            "applicationName": "support/app main",
            "filter": {"allowed": [{"type": "StasisStart"}]}
        }),
        path: "applications/support%2Fapp%20main/eventFilter",
        body_b64: Some("eyJhbGxvd2VkIjpbeyJ0eXBlIjoiU3Rhc2lzU3RhcnQifV19")
    },
    asterisk_get_object => {
        operation: "asterisk.ari.asterisk.getObject",
        method: "GET",
        input: json!({
            "configClass": "res_pjsip/config",
            "objectType": "endpoint type",
            "id": "alice/100"
        }),
        path: "asterisk/config/dynamic/res_pjsip%2Fconfig/endpoint%20type/alice%2F100",
        body_b64: None
    },
    asterisk_update_object => {
        operation: "asterisk.ari.asterisk.updateObject",
        method: "PUT",
        input: json!({
            "configClass": "res_pjsip/config",
            "objectType": "endpoint type",
            "id": "alice/100",
            "fields": {"attribute": "value & more"}
        }),
        path: "asterisk/config/dynamic/res_pjsip%2Fconfig/endpoint%20type/alice%2F100",
        body_b64: Some("eyJhdHRyaWJ1dGUiOiJ2YWx1ZSAmIG1vcmUifQ==")
    },
    asterisk_delete_object => {
        operation: "asterisk.ari.asterisk.deleteObject",
        method: "DELETE",
        input: json!({
            "configClass": "res_pjsip/config",
            "objectType": "endpoint type",
            "id": "alice/100"
        }),
        path: "asterisk/config/dynamic/res_pjsip%2Fconfig/endpoint%20type/alice%2F100",
        body_b64: None
    },
    asterisk_get_info => {
        operation: "asterisk.ari.asterisk.getInfo",
        method: "GET",
        input: json!({"only": ["build", "status info"]}),
        path: "asterisk/info?only=build&only=status%20info",
        body_b64: None
    },
    asterisk_ping => {
        operation: "asterisk.ari.asterisk.ping",
        method: "GET",
        input: json!({}),
        path: "asterisk/ping",
        body_b64: None
    },
    asterisk_list_modules => {
        operation: "asterisk.ari.asterisk.listModules",
        method: "GET",
        input: json!({}),
        path: "asterisk/modules",
        body_b64: None
    },
    asterisk_get_module => {
        operation: "asterisk.ari.asterisk.getModule",
        method: "GET",
        input: json!({"moduleName": "res_pjsip.so/main"}),
        path: "asterisk/modules/res_pjsip.so%2Fmain",
        body_b64: None
    },
    asterisk_load_module => {
        operation: "asterisk.ari.asterisk.loadModule",
        method: "POST",
        input: json!({"moduleName": "res_pjsip.so/main"}),
        path: "asterisk/modules/res_pjsip.so%2Fmain",
        body_b64: None
    },
    asterisk_unload_module => {
        operation: "asterisk.ari.asterisk.unloadModule",
        method: "DELETE",
        input: json!({"moduleName": "res_pjsip.so/main"}),
        path: "asterisk/modules/res_pjsip.so%2Fmain",
        body_b64: None
    },
    asterisk_reload_module => {
        operation: "asterisk.ari.asterisk.reloadModule",
        method: "PUT",
        input: json!({"moduleName": "res_pjsip.so/main"}),
        path: "asterisk/modules/res_pjsip.so%2Fmain",
        body_b64: None
    },
    asterisk_list_log_channels => {
        operation: "asterisk.ari.asterisk.listLogChannels",
        method: "GET",
        input: json!({}),
        path: "asterisk/logging",
        body_b64: None
    },
    asterisk_add_log => {
        operation: "asterisk.ari.asterisk.addLog",
        method: "POST",
        input: json!({
            "logChannelName": "security/main log",
            "configuration": "notice,warning &error"
        }),
        path: "asterisk/logging/security%2Fmain%20log?configuration=notice%2Cwarning%20%26error",
        body_b64: None
    },
    asterisk_delete_log => {
        operation: "asterisk.ari.asterisk.deleteLog",
        method: "DELETE",
        input: json!({"logChannelName": "security/main log"}),
        path: "asterisk/logging/security%2Fmain%20log",
        body_b64: None
    },
    asterisk_rotate_log => {
        operation: "asterisk.ari.asterisk.rotateLog",
        method: "PUT",
        input: json!({"logChannelName": "security/main log"}),
        path: "asterisk/logging/security%2Fmain%20log/rotate",
        body_b64: None
    },
    asterisk_get_global_var => {
        operation: "asterisk.ari.asterisk.getGlobalVar",
        method: "GET",
        input: json!({"variable": "CHANNEL(peer/name)"}),
        path: "asterisk/variable?variable=CHANNEL%28peer%2Fname%29",
        body_b64: None
    },
    asterisk_set_global_var => {
        operation: "asterisk.ari.asterisk.setGlobalVar",
        method: "POST",
        input: json!({"variable": "CHANNEL(peer/name)", "value": "a b&c=d"}),
        path: "asterisk/variable?variable=CHANNEL%28peer%2Fname%29&value=a%20b%26c%3Dd",
        body_b64: None
    },
    endpoints_list => {
        operation: "asterisk.ari.endpoints.list",
        method: "GET",
        input: json!({}),
        path: "endpoints",
        body_b64: None
    },
    endpoints_send_message => {
        operation: "asterisk.ari.endpoints.sendMessage",
        method: "PUT",
        input: json!({
            "to": "pjsip:alice@example.com/desk",
            "from": "pjsip:bob@example.com",
            "body": "Hello & goodbye=soon",
            "variables": {"variables": {"ticket": "42"}}
        }),
        path: "endpoints/sendMessage?to=pjsip%3Aalice%40example.com%2Fdesk&from=pjsip%3Abob%40example.com&body=Hello%20%26%20goodbye%3Dsoon",
        body_b64: Some("eyJ2YXJpYWJsZXMiOnsidGlja2V0IjoiNDIifX0=")
    },
    endpoints_refer => {
        operation: "asterisk.ari.endpoints.refer",
        method: "POST",
        input: json!({
            "to": "pjsip:alice@example.com/desk",
            "from": "pjsip:bob@example.com",
            "refer_to": "pjsip:carol@example.com/desk 2",
            "to_self": true,
            "variables": {"variables": {"route": "desk/2"}}
        }),
        path: "endpoints/refer?to=pjsip%3Aalice%40example.com%2Fdesk&from=pjsip%3Abob%40example.com&refer_to=pjsip%3Acarol%40example.com%2Fdesk%202&to_self=true",
        body_b64: Some("eyJ2YXJpYWJsZXMiOnsicm91dGUiOiJkZXNrLzIifX0=")
    },
    endpoints_list_by_tech => {
        operation: "asterisk.ari.endpoints.listByTech",
        method: "GET",
        input: json!({"tech": "PJSIP/main"}),
        path: "endpoints/PJSIP%2Fmain",
        body_b64: None
    },
    endpoints_get => {
        operation: "asterisk.ari.endpoints.get",
        method: "GET",
        input: json!({"tech": "PJSIP", "resource": "alice/desk 1"}),
        path: "endpoints/PJSIP/alice%2Fdesk%201",
        body_b64: None
    },
    endpoints_send_message_to_endpoint => {
        operation: "asterisk.ari.endpoints.sendMessageToEndpoint",
        method: "PUT",
        input: json!({
            "tech": "PJSIP",
            "resource": "alice/desk 1",
            "from": "pjsip:bob@example.com",
            "body": "Hello & goodbye=soon",
            "variables": {"variables": {"ticket": "42"}}
        }),
        path: "endpoints/PJSIP/alice%2Fdesk%201/sendMessage?from=pjsip%3Abob%40example.com&body=Hello%20%26%20goodbye%3Dsoon",
        body_b64: Some("eyJ2YXJpYWJsZXMiOnsidGlja2V0IjoiNDIifX0=")
    },
    endpoints_refer_to_endpoint => {
        operation: "asterisk.ari.endpoints.referToEndpoint",
        method: "POST",
        input: json!({
            "tech": "PJSIP",
            "resource": "alice/desk 1",
            "from": "pjsip:bob@example.com",
            "refer_to": "pjsip:carol@example.com/desk 2",
            "to_self": true,
            "variables": {"variables": {"route": "desk/2"}}
        }),
        path: "endpoints/PJSIP/alice%2Fdesk%201/refer?from=pjsip%3Abob%40example.com&refer_to=pjsip%3Acarol%40example.com%2Fdesk%202&to_self=true",
        body_b64: Some("eyJ2YXJpYWJsZXMiOnsicm91dGUiOiJkZXNrLzIifX0=")
    },
    device_states_list => {
        operation: "asterisk.ari.device_states.list",
        method: "GET",
        input: json!({}),
        path: "deviceStates",
        body_b64: None
    },
    device_states_get => {
        operation: "asterisk.ari.device_states.get",
        method: "GET",
        input: json!({"deviceName": "PJSIP/alice 1"}),
        path: "deviceStates/PJSIP%2Falice%201",
        body_b64: None
    },
    device_states_update => {
        operation: "asterisk.ari.device_states.update",
        method: "PUT",
        input: json!({"deviceName": "PJSIP/alice 1", "deviceState": "INUSE"}),
        path: "deviceStates/PJSIP%2Falice%201?deviceState=INUSE",
        body_b64: None
    },
    device_states_delete => {
        operation: "asterisk.ari.device_states.delete",
        method: "DELETE",
        input: json!({"deviceName": "PJSIP/alice 1"}),
        path: "deviceStates/PJSIP%2Falice%201",
        body_b64: None
    },
    mailboxes_list => {
        operation: "asterisk.ari.mailboxes.list",
        method: "GET",
        input: json!({}),
        path: "mailboxes",
        body_b64: None
    },
    mailboxes_get => {
        operation: "asterisk.ari.mailboxes.get",
        method: "GET",
        input: json!({"mailboxName": "1000@default/main"}),
        path: "mailboxes/1000%40default%2Fmain",
        body_b64: None
    },
    mailboxes_update => {
        operation: "asterisk.ari.mailboxes.update",
        method: "PUT",
        input: json!({
            "mailboxName": "1000@default/main",
            "oldMessages": 7,
            "newMessages": 2
        }),
        path: "mailboxes/1000%40default%2Fmain?oldMessages=7&newMessages=2",
        body_b64: None
    },
    mailboxes_delete => {
        operation: "asterisk.ari.mailboxes.delete",
        method: "DELETE",
        input: json!({"mailboxName": "1000@default/main"}),
        path: "mailboxes/1000%40default%2Fmain",
        body_b64: None
    },
}

fn target_manifest_name(name: &str) -> bool {
    [
        "asterisk.ari.applications.",
        "asterisk.ari.asterisk.",
        "asterisk.ari.endpoints.",
        "asterisk.ari.device_states.",
        "asterisk.ari.mailboxes.",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

#[test]
fn every_system_resource_operation_has_a_hermetic_fixture() {
    let fixtures = fixtures();
    assert_eq!(fixtures.len(), 36);
    let fixture_names: BTreeSet<_> = fixtures.iter().map(|fixture| fixture.operation).collect();
    assert_eq!(fixture_names.len(), 36, "duplicate fixture operation");

    let mut source_counts = BTreeMap::new();
    let mut source_names = BTreeSet::new();
    for operation in ari::source_operations().expect("generated source operations") {
        let resource = operation["resource"].as_str().expect("resource");
        let (source_key, normalized) = match resource {
            "applications" => ("applications", "applications"),
            "asterisk" => ("asterisk", "asterisk"),
            "endpoints" => ("endpoints", "endpoints"),
            "mailboxes" => ("mailboxes", "mailboxes"),
            "deviceStates" => ("deviceStates", "device_states"),
            _ => continue,
        };
        *source_counts.entry(source_key).or_insert(0usize) += 1;
        source_names.insert(format!(
            "asterisk.ari.{normalized}.{}",
            operation["nickname"].as_str().expect("nickname")
        ));
    }
    assert_eq!(
        source_counts,
        BTreeMap::from([
            ("applications", 5),
            ("asterisk", 16),
            ("deviceStates", 4),
            ("endpoints", 7),
            ("mailboxes", 4),
        ])
    );
    assert_eq!(
        source_names,
        fixture_names
            .iter()
            .map(|name| (*name).to_string())
            .collect()
    );

    let manifest = ari::register(PluginBuilder::new("asterisk-test", "0.0.0"))
        .expect("register generated operations")
        .manifest();
    let mut manifest_counts = BTreeMap::new();
    for operation in manifest
        .operations
        .into_iter()
        .filter(|operation| target_manifest_name(&operation.name))
    {
        *manifest_counts.entry(operation.name).or_insert(0usize) += 1;
    }
    assert_eq!(manifest_counts.len(), 36);
    assert_eq!(
        manifest_counts.keys().cloned().collect::<BTreeSet<_>>(),
        fixture_names
            .iter()
            .map(|name| (*name).to_string())
            .collect()
    );
    assert!(manifest_counts.values().all(|count| *count == 1));
}

#[test]
fn non_success_preserves_status_and_body_after_encoding_the_request() {
    let contract = ari::contracts()
        .expect("generated contracts")
        .into_iter()
        .find(|contract| contract.name == "asterisk.ari.mailboxes.update")
        .expect("mailbox update contract");
    let expected_path = "mailboxes/1000%40default%2Fmain?oldMessages=7&newMessages=2";
    let mut mock = MockHost::default()
        .with_endpoint_ref(ENDPOINT_REF, "http://localhost:8088/ari/")
        .with_http_status_body(expected_path, 422, "mailbox state rejected");
    let error = ari::execute(
        &contract,
        json!({
            "mailboxName": "1000@default/main",
            "oldMessages": 7,
            "newMessages": 2
        }),
        &mut Host::new(&mut mock),
    )
    .expect_err("non-2xx response must fail");
    assert_eq!(
        error,
        format!("PUT {expected_path} returned 422: mailbox state rejected")
    );
    let calls = mock.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1["path"], expected_path);
}
