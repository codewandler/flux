#[path = "../src/ari.rs"]
mod ari;

use host_kit::PluginManifest;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::process::{Command, Stdio};

const OFFICIAL_EVENT_OPEN: &str = "asterisk.ari.events.eventWebsocket";

const AMI_SCHEMA_PINS: [(&str, &str); 8] = [
    (
        "asterisk.ami.ping",
        r#"{"properties":{"timeout":{"description":"AMI connection timeout, e.g. `5s` or `1m`. Defaults to 10s if omitted.","type":["string","null"]}},"type":"object"}"#,
    ),
    (
        "asterisk.channel.list",
        r#"{"properties":{"limit":{"description":"Maximum channels to return.","format":"int64","type":["integer","null"]},"timeout":{"description":"AMI connection timeout, e.g. `5s` or `1m`. Defaults to 10s if omitted.","type":["string","null"]}},"type":"object"}"#,
    ),
    (
        "asterisk.peer.list",
        r#"{"properties":{"limit":{"description":"Maximum peers to return.","format":"int64","type":["integer","null"]},"technology":{"description":"Channel technology: `pjsip`, `sip`, or `iax` (default `pjsip`).","type":["string","null"]},"timeout":{"description":"AMI connection timeout, e.g. `5s` or `1m`. Defaults to 10s if omitted.","type":["string","null"]}},"type":"object"}"#,
    ),
    (
        "asterisk.queue.status",
        r#"{"properties":{"queue":{"description":"Limit status to this queue.","type":["string","null"]},"timeout":{"description":"AMI connection timeout, e.g. `5s` or `1m`. Defaults to 10s if omitted.","type":["string","null"]}},"type":"object"}"#,
    ),
    (
        "asterisk.devicestate.list",
        r#"{"properties":{"device":{"description":"Substring filter on the device name (e.g. `PJSIP/agent-7`).","type":["string","null"]},"limit":{"description":"Maximum device states to return.","format":"int64","type":["integer","null"]},"timeout":{"description":"AMI connection timeout, e.g. `5s` or `1m`. Defaults to 10s if omitted.","type":["string","null"]}},"type":"object"}"#,
    ),
    (
        "asterisk.channel.hangup",
        r#"{"properties":{"cause":{"description":"ISDN hangup cause code (e.g. 16 normal clearing).","format":"int64","type":["integer","null"]},"channel":{"description":"Exact channel name to hang up.","type":"string"},"timeout":{"description":"AMI connection timeout, e.g. `5s` or `1m`. Defaults to 10s if omitted.","type":["string","null"]}},"required":["channel"],"type":"object"}"#,
    ),
    (
        "asterisk.call.originate",
        r#"{"properties":{"account_code":{"description":"Account code for the call.","type":["string","null"]},"application":{"description":"Application to run on answer (mutually exclusive with `exten`).","type":["string","null"]},"async":{"description":"Originate asynchronously (default true).","type":["boolean","null"]},"caller_id":{"description":"Caller ID for the originated call.","type":["string","null"]},"channel":{"description":"Channel to call first.","type":"string"},"channel_id":{"description":"Explicit unique id for the first channel.","type":["string","null"]},"context":{"description":"Dialplan context for `exten`.","type":["string","null"]},"data":{"description":"Application argument data.","type":["string","null"]},"early_media":{"description":"Connect on early media instead of answer.","type":["boolean","null"]},"exten":{"description":"Extension to connect to (requires `context`; mutually exclusive with `application`).","type":["string","null"]},"other_channel_id":{"description":"Explicit unique id for the second channel.","type":["string","null"]},"priority":{"description":"Dialplan priority (default 1).","format":"int64","type":["integer","null"]},"timeout":{"description":"AMI connection timeout, e.g. `5s` or `1m`. Defaults to 10s if omitted.","type":["string","null"]},"timeout_ms":{"description":"Answer timeout in milliseconds (default 30000).","format":"int64","type":["integer","null"]},"variables":{"additionalProperties":{"type":"string"},"description":"Channel variables to set on the originated channel.","type":["object","null"]}},"required":["channel"],"type":"object"}"#,
    ),
    (
        "asterisk.command",
        r#"{"properties":{"command":{"description":"Asterisk CLI command to run.","type":"string"},"timeout":{"description":"AMI connection timeout, e.g. `5s` or `1m`. Defaults to 10s if omitted.","type":["string","null"]}},"required":["command"],"type":"object"}"#,
    ),
];

fn production_manifest() -> PluginManifest {
    let mut child = Command::new(env!("CARGO_BIN_EXE_flux-plugin-asterisk"))
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn production Asterisk plugin");
    let request = json!({
        "protocol": "flux.plugin.v1",
        "id": "completion-manifest",
        "type": "request",
        "command": "manifest",
        "payload": null
    });
    writeln!(
        child.stdin.as_mut().expect("plugin stdin"),
        "{}",
        serde_json::to_string(&request).expect("encode manifest request")
    )
    .expect("write manifest request");
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for plugin manifest");
    assert!(
        output.status.success(),
        "plugin manifest process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut responses = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty());
    let response: Value = serde_json::from_slice(responses.next().expect("manifest response"))
        .expect("manifest response JSON");
    assert!(responses.next().is_none(), "unexpected second plugin frame");
    assert_eq!(response["id"], "completion-manifest");
    assert_eq!(response["ok"], true, "manifest response: {response}");
    serde_json::from_value(response["result"].clone()).expect("production plugin manifest")
}

fn official_name(operation: &Value) -> String {
    let resource = match operation["resource"].as_str().expect("official resource") {
        "deviceStates" => "device_states",
        resource => resource,
    };
    format!(
        "asterisk.ari.{resource}.{}",
        operation["nickname"].as_str().expect("official nickname")
    )
}

#[test]
fn final_manifest_accounts_for_every_official_ari_fact_in_both_directions() {
    let source = ari::source_operations().expect("generated official facts");
    assert_eq!(source.len(), 109);
    let source_counts = source
        .iter()
        .fold(BTreeMap::new(), |mut counts, operation| {
            *counts.entry(official_name(operation)).or_insert(0usize) += 1;
            counts
        });
    assert!(source_counts.values().all(|count| *count == 1));
    let official_names: BTreeSet<_> = source_counts.keys().cloned().collect();
    assert_eq!(official_names.len(), 109);
    assert!(official_names.contains(OFFICIAL_EVENT_OPEN));

    let rest_names: BTreeSet<_> = ari::contracts()
        .expect("generated contracts")
        .into_iter()
        .filter(|contract| !contract.websocket)
        .map(|contract| contract.name)
        .collect();
    assert_eq!(rest_names.len(), 108);
    assert_eq!(
        official_names
            .difference(&BTreeSet::from([OFFICIAL_EVENT_OPEN.to_string()]))
            .cloned()
            .collect::<BTreeSet<_>>(),
        rest_names
    );

    let manifest = production_manifest();
    let ari_counts = manifest
        .operations
        .iter()
        .filter(|operation| operation.name.starts_with("asterisk.ari."))
        .fold(BTreeMap::new(), |mut counts, operation| {
            *counts.entry(operation.name.clone()).or_insert(0usize) += 1;
            counts
        });
    assert!(ari_counts.values().all(|count| *count == 1));
    let manifest_names: BTreeSet<_> = ari_counts.keys().cloned().collect();
    let controls = BTreeSet::from([
        ari::EVENT_READ_CONTROL.to_string(),
        ari::EVENT_CLOSE_CONTROL.to_string(),
    ]);
    assert!(controls.is_disjoint(&official_names));
    let expected_manifest: BTreeSet<_> = official_names.union(&controls).cloned().collect();
    assert_eq!(manifest_names, expected_manifest);
}

#[test]
fn existing_ami_operation_identities_and_schemas_are_byte_identical() {
    let manifest = production_manifest();
    let expected_names: BTreeSet<_> = AMI_SCHEMA_PINS
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();
    let visible_non_ari: Vec<_> = manifest
        .operations
        .iter()
        .filter(|operation| !operation.internal && !operation.name.starts_with("asterisk.ari."))
        .collect();
    assert_eq!(visible_non_ari.len(), 8);
    assert_eq!(
        visible_non_ari
            .iter()
            .map(|operation| operation.name.clone())
            .collect::<BTreeSet<_>>(),
        expected_names
    );

    for (name, expected_schema) in AMI_SCHEMA_PINS {
        let matches: Vec<_> = visible_non_ari
            .iter()
            .filter(|operation| operation.name == name)
            .collect();
        assert_eq!(matches.len(), 1, "{name} manifest count");
        let operation = matches[0];
        let actual = serde_json::to_string(&operation.input_schema).expect("input schema bytes");
        assert_eq!(
            actual.as_bytes(),
            expected_schema.as_bytes(),
            "{name} input schema changed"
        );
        assert!(
            operation.output_schema.is_none(),
            "{name} gained an output schema and changed its published contract"
        );
    }
}
