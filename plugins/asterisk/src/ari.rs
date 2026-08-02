//! Generated Asterisk REST Interface contracts and their single guarded executor.
#![cfg_attr(test, allow(dead_code))]

use host_kit::{Effect, Host, Idempotency, OperationSpec, PluginBuilder, Risk, WebSocketEvent};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

mod generated {
    include!("ari_generated.rs");
}

const ENDPOINT_REF: &str = "asterisk.ari";
const AUTH_PURPOSE: &str = "ari_basic";
const BINARY_MAX_BYTES: usize = 256 * 1024 * 1024;
const BINARY_TIMEOUT_MS: u64 = 30_000;
const WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 30_000;
const WEBSOCKET_MAX_TIMEOUT_MS: u64 = 300_000;
pub(crate) const EVENT_READ_CONTROL: &str = "asterisk.ari.control.events.read";
pub(crate) const EVENT_CLOSE_CONTROL: &str = "asterisk.ari.control.events.close";

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct AriParameter {
    pub(crate) name: String,
    pub(crate) placement: String,
    #[serde(default)]
    pub(crate) required: Option<bool>,
    #[serde(default)]
    pub(crate) allow_multiple: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct AriContract {
    pub(crate) name: String,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) websocket: bool,
    pub(crate) description: String,
    pub(crate) response_class: String,
    pub(crate) response_kind: String,
    pub(crate) parameters: Vec<AriParameter>,
    pub(crate) input_schema: Value,
    #[serde(default)]
    output_schema: Option<Value>,
    pub(crate) effects: Vec<String>,
    pub(crate) risk: String,
    pub(crate) idempotency: String,
    pub(crate) semantic_effects: Vec<String>,
}

pub(crate) fn contracts() -> Result<Vec<AriContract>, String> {
    serde_json::from_str(generated::ARI_OPERATIONS_JSON)
        .map_err(|error| format!("generated ARI operation contracts are invalid: {error}"))
}

#[cfg(test)]
pub(crate) fn source_operations() -> Result<Vec<Value>, String> {
    serde_json::from_str(generated::ARI_SOURCE_OPERATIONS_JSON)
        .map_err(|error| format!("generated ARI source facts are invalid: {error}"))
}

pub(crate) fn model_schemas() -> Result<BTreeMap<String, Value>, String> {
    serde_json::from_str(generated::ARI_MODEL_SCHEMAS_JSON)
        .map_err(|error| format!("generated ARI model schemas are invalid: {error}"))
}

fn effect(value: &str, operation: &str) -> Result<Effect, String> {
    match value {
        "read" => Ok(Effect::Read),
        "write" => Ok(Effect::Write),
        "network" => Ok(Effect::Network),
        other => Err(format!(
            "generated ARI operation `{operation}` has unknown effect `{other}`"
        )),
    }
}

fn risk(value: &str, operation: &str) -> Result<Risk, String> {
    match value {
        "low" => Ok(Risk::Low),
        "medium" => Ok(Risk::Medium),
        "high" => Ok(Risk::High),
        "destructive" => Ok(Risk::Destructive),
        other => Err(format!(
            "generated ARI operation `{operation}` has unknown risk `{other}`"
        )),
    }
}

fn idempotency(value: &str, operation: &str) -> Result<Idempotency, String> {
    match value {
        "idempotent" => Ok(Idempotency::Idempotent),
        "non_idempotent" => Ok(Idempotency::NonIdempotent),
        "conditional" => Ok(Idempotency::Conditional),
        other => Err(format!(
            "generated ARI operation `{operation}` has unknown idempotency `{other}`"
        )),
    }
}

fn schema_for_type(
    type_name: &str,
    definitions: &BTreeMap<String, Value>,
) -> Result<Value, String> {
    let mut schema = if let Some(inner) = type_name
        .strip_prefix("List[")
        .and_then(|value| value.strip_suffix(']'))
    {
        json!({"type": "array", "items": schema_for_type(inner, definitions)?})
    } else if definitions.contains_key(type_name) {
        json!({"$ref": format!("#/$defs/{type_name}")})
    } else {
        return Err(format!(
            "generated ARI response references unknown model `{type_name}`"
        ));
    };
    schema["$defs"] = serde_json::to_value(definitions)
        .map_err(|error| format!("cannot encode generated ARI model schemas: {error}"))?;
    Ok(schema)
}

fn operation_spec(
    contract: &AriContract,
    definitions: &BTreeMap<String, Value>,
) -> Result<OperationSpec, String> {
    let output_schema = match contract.response_kind.as_str() {
        "json" => schema_for_type(&contract.response_class, definitions)?,
        "void" | "binary" => contract.output_schema.clone().ok_or_else(|| {
            format!(
                "generated ARI operation `{}` has no output schema",
                contract.name
            )
        })?,
        other => {
            return Err(format!(
                "generated ARI operation `{}` has unknown response kind `{other}`",
                contract.name
            ))
        }
    };
    let semantic_effects =
        serde_json::from_value(json!(contract.semantic_effects)).map_err(|error| {
            format!(
                "generated ARI operation `{}` has invalid semantic effects: {error}",
                contract.name
            )
        })?;
    Ok(OperationSpec {
        name: contract.name.clone(),
        description: contract.description.clone(),
        input_schema: contract.input_schema.clone(),
        output_schema: Some(output_schema),
        effects: contract
            .effects
            .iter()
            .map(|value| effect(value, &contract.name))
            .collect::<Result<_, _>>()?,
        risk: Some(risk(&contract.risk, &contract.name)?),
        idempotency: Some(idempotency(&contract.idempotency, &contract.name)?),
        secret_purposes: vec![AUTH_PURPOSE.to_string()],
        semantic_effects,
        ..OperationSpec::default()
    })
}

pub(crate) fn register(mut builder: PluginBuilder) -> Result<PluginBuilder, String> {
    let definitions = model_schemas()?;
    for contract in contracts()? {
        if contract.websocket {
            continue;
        }
        let spec = operation_spec(&contract, &definitions)?;
        let operation = contract.name.clone();
        builder =
            builder.operation_flexible(spec, move |input, host| execute(&contract, input, host));
        if has_channel_conditional_rules(&operation) {
            let preflight_operation = operation.clone();
            builder = builder.preflight(operation, move |input| {
                channel_conditional_problems(&preflight_operation, input)
            });
        }
    }
    Ok(builder)
}

/// Add the one official WebSocket route plus plugin-owned lifecycle controls after the generated
/// REST registrar. Keeping this separate preserves the factual 109-source/108-REST census: the
/// read and close operations are Flux plugin controls, not invented Asterisk Swagger operations.
pub(crate) fn register_recordings_and_events(
    builder: PluginBuilder,
) -> Result<PluginBuilder, String> {
    let mut builder = register(builder)?;
    let definitions = model_schemas()?;
    let mut websocket_contracts = contracts()?
        .into_iter()
        .filter(|contract| contract.websocket);
    let contract = websocket_contracts
        .next()
        .ok_or_else(|| "generated ARI contracts contain no WebSocket operation".to_string())?;
    if let Some(extra) = websocket_contracts.next() {
        return Err(format!(
            "generated ARI contracts contain an unexpected second WebSocket operation `{}`",
            extra.name
        ));
    }

    let mut connect_spec = operation_spec(&contract, &definitions)?;
    connect_spec.description = format!(
        "{} Opens a host-owned guarded WebSocket and returns its session-scoped id.",
        contract.description
    );
    connect_spec.output_schema = Some(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "ws_id": {"type": "integer", "minimum": 1}
        },
        "required": ["ws_id"]
    }));
    builder = builder.operation_flexible(connect_spec, move |input, host| {
        open_event_websocket(&contract, input, host)
    });

    let event_output = json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": {"const": "event"},
                    "event": {"$ref": "#/$defs/Message"}
                },
                "required": ["kind", "event"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {"kind": {"const": "timeout"}},
                "required": ["kind"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": {"const": "close"},
                    "code": {"type": ["integer", "null"], "minimum": 0, "maximum": 65535},
                    "reason": {"type": "string"}
                },
                "required": ["kind", "code", "reason"]
            }
        ],
        "$defs": definitions
    });
    builder = builder.operation_flexible(
        OperationSpec {
            name: EVENT_READ_CONTROL.into(),
            description: "Flux plugin lifecycle control (not an Asterisk Swagger operation): read one bounded event from a host-owned ARI WebSocket. Binary frames are refused because ARI events are JSON text.".into(),
            input_schema: websocket_control_input_schema(),
            output_schema: Some(event_output),
            effects: vec![Effect::Read, Effect::Network],
            risk: Some(Risk::Low),
            idempotency: Some(Idempotency::NonIdempotent),
            ..OperationSpec::default()
        },
        read_event_websocket,
    );
    builder = builder.operation_flexible(
        OperationSpec {
            name: EVENT_CLOSE_CONTROL.into(),
            description: "Flux plugin lifecycle control (not an Asterisk Swagger operation): gracefully close one host-owned ARI WebSocket.".into(),
            input_schema: websocket_control_input_schema(),
            output_schema: Some(json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {"closed": {"type": "boolean"}},
                "required": ["closed"]
            })),
            effects: vec![Effect::Write, Effect::Network],
            risk: Some(Risk::Low),
            idempotency: Some(Idempotency::Idempotent),
            ..OperationSpec::default()
        },
        close_event_websocket,
    );
    Ok(builder)
}

fn websocket_control_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "ws_id": {"type": "integer", "minimum": 1},
            "timeout_ms": {
                "type": "integer",
                "minimum": 1,
                "maximum": WEBSOCKET_MAX_TIMEOUT_MS
            }
        },
        "required": ["ws_id", "timeout_ms"]
    })
}

fn websocket_control_values(input: &Value, operation: &str) -> Result<(u64, u64), String> {
    let object = input
        .as_object()
        .ok_or_else(|| format!("{operation} input must be an object"))?;
    let ws_id = object
        .get("ws_id")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{operation} requires a positive `ws_id`"))?;
    let timeout_ms = object
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .filter(|value| (1..=WEBSOCKET_MAX_TIMEOUT_MS).contains(value))
        .ok_or_else(|| {
            format!("{operation} requires `timeout_ms` between 1 and {WEBSOCKET_MAX_TIMEOUT_MS}")
        })?;
    Ok((ws_id, timeout_ms))
}

fn open_event_websocket(
    contract: &AriContract,
    input: Value,
    host: &mut Host,
) -> Result<Value, String> {
    if !contract.websocket {
        return Err(format!("{} is not a WebSocket route", contract.name));
    }
    let (path, body) = request_parts(contract, input)?;
    if body.is_some() {
        return Err(format!(
            "{} unexpectedly declared a WebSocket request body",
            contract.name
        ));
    }
    let ws_id = host.ws_connect(
        ENDPOINT_REF,
        &path,
        Some(AUTH_PURPOSE),
        WEBSOCKET_CONNECT_TIMEOUT_MS,
    )?;
    Ok(json!({"ws_id": ws_id}))
}

fn read_event_websocket(input: Value, host: &mut Host) -> Result<Value, String> {
    let (ws_id, timeout_ms) = websocket_control_values(&input, EVENT_READ_CONTROL)?;
    match host.ws_read(ws_id, timeout_ms)? {
        WebSocketEvent::Text(text) => {
            let event: Value = serde_json::from_str(&text).map_err(|error| {
                format!("ARI event WebSocket returned invalid JSON text: {error}")
            })?;
            if !event.is_object()
                || event
                    .get("type")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err("ARI event WebSocket text is not a typed Message object".into());
            }
            Ok(json!({"kind": "event", "event": event}))
        }
        WebSocketEvent::Binary(bytes) => Err(format!(
            "ARI event WebSocket refused a {}-byte binary frame; ARI events must be JSON text",
            bytes.len()
        )),
        WebSocketEvent::Timeout => Ok(json!({"kind": "timeout"})),
        WebSocketEvent::Close { code, reason } => {
            Ok(json!({"kind": "close", "code": code, "reason": reason}))
        }
    }
}

fn close_event_websocket(input: Value, host: &mut Host) -> Result<Value, String> {
    let (ws_id, timeout_ms) = websocket_control_values(&input, EVENT_CLOSE_CONTROL)?;
    Ok(json!({"closed": host.ws_close(ws_id, timeout_ms)?}))
}

fn has_channel_conditional_rules(operation: &str) -> bool {
    matches!(
        operation,
        "asterisk.ari.channels.originate"
            | "asterisk.ari.channels.originateWithId"
            | "asterisk.ari.channels.externalMedia"
            | "asterisk.ari.channels.snoopChannel"
            | "asterisk.ari.channels.snoopChannelWithId"
    )
}

fn channel_conditional_problems(operation: &str, input: &Value) -> Vec<String> {
    let Some(input) = input.as_object() else {
        return Vec::new();
    };
    match operation {
        "asterisk.ari.channels.originate" | "asterisk.ari.channels.originateWithId" => {
            originate_problems(input)
        }
        "asterisk.ari.channels.externalMedia" => external_media_problems(input),
        "asterisk.ari.channels.snoopChannel" | "asterisk.ari.channels.snoopChannelWithId" => {
            snoop_problems(input)
        }
        _ => Vec::new(),
    }
}

fn present(input: &Map<String, Value>, name: &str) -> bool {
    input.get(name).is_some_and(|value| !value.is_null())
}

fn string_or<'a>(input: &'a Map<String, Value>, name: &str, fallback: &'a str) -> &'a str {
    input.get(name).and_then(Value::as_str).unwrap_or(fallback)
}

fn originate_problems(input: &Map<String, Value>) -> Vec<String> {
    let mut problems = Vec::new();
    let app = present(input, "app");
    let dialplan = ["extension", "context", "priority", "label"]
        .into_iter()
        .filter(|name| present(input, name))
        .collect::<Vec<_>>();
    if app && !dialplan.is_empty() {
        problems.push(format!(
            "`app` and dialplan fields ({}) are mutually exclusive",
            dialplan.join(", ")
        ));
    }
    if present(input, "appArgs") && !app {
        problems.push("`appArgs` requires `app`".into());
    }
    if present(input, "originator") && present(input, "formats") {
        problems.push("`originator` and `formats` are mutually exclusive".into());
    }
    problems
}

fn external_media_problems(input: &Map<String, Value>) -> Vec<String> {
    let transport = string_or(input, "transport", "udp");
    let encapsulation = string_or(input, "encapsulation", "rtp");
    let connection_type = string_or(input, "connection_type", "client");
    let mut problems = Vec::new();

    let supported_pair = matches!(
        (transport, encapsulation),
        ("udp", "rtp") | ("tcp", "audiosocket") | ("websocket", "none")
    );
    if !supported_pair {
        problems.push(format!(
            "transport `{transport}` requires its matching encapsulation: udp/rtp, tcp/audiosocket, or websocket/none"
        ));
    }
    if connection_type == "server" && transport != "websocket" {
        problems.push("`connection_type=server` is valid only with websocket transport".into());
    }
    if !(transport == "websocket" && connection_type == "server")
        && !present(input, "external_host")
    {
        problems.push(
            "`external_host` is required unless websocket transport uses server connection type"
                .into(),
        );
    }
    problems
}

fn snoop_problems(input: &Map<String, Value>) -> Vec<String> {
    let spy = string_or(input, "spy", "none");
    let whisper = string_or(input, "whisper", "none");
    if spy == "none" && whisper == "none" {
        vec!["at least one of `spy or whisper` must select an audio direction".into()]
    } else {
        Vec::new()
    }
}

fn scalar(value: &Value, name: &str) -> Result<String, String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        _ => Err(format!(
            "ARI parameter `{name}` must be a string, number, or boolean"
        )),
    }
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn request_parts(
    contract: &AriContract,
    input: Value,
) -> Result<(String, Option<Vec<u8>>), String> {
    let object = input
        .as_object()
        .ok_or_else(|| format!("{} input must be an object", contract.name))?;
    let mut path = contract.path.clone();
    let mut query = Vec::new();
    let mut body = None;
    for parameter in &contract.parameters {
        let value = object.get(&parameter.name);
        if value.is_none() {
            if parameter.required.unwrap_or(false) {
                return Err(format!("{} requires `{}`", contract.name, parameter.name));
            }
            continue;
        }
        let value = value.expect("checked above");
        match parameter.placement.as_str() {
            "path" => {
                let placeholder = format!("{{{}}}", parameter.name);
                if !path.contains(&placeholder) {
                    return Err(format!(
                        "{} path has no `{placeholder}` placeholder",
                        contract.name
                    ));
                }
                path = path.replace(
                    &placeholder,
                    &percent_encode(&scalar(value, &parameter.name)?),
                );
            }
            "query" => {
                if parameter.allow_multiple.unwrap_or(false) {
                    let values = value.as_array().ok_or_else(|| {
                        format!("ARI parameter `{}` must be an array", parameter.name)
                    })?;
                    for item in values {
                        query.push(format!(
                            "{}={}",
                            percent_encode(&parameter.name),
                            percent_encode(&scalar(item, &parameter.name)?)
                        ));
                    }
                } else {
                    query.push(format!(
                        "{}={}",
                        percent_encode(&parameter.name),
                        percent_encode(&scalar(value, &parameter.name)?)
                    ));
                }
            }
            "body" => {
                if body.is_some() {
                    return Err(format!("{} declares more than one body", contract.name));
                }
                body = Some(serde_json::to_vec(value).map_err(|error| {
                    format!("cannot encode `{}` body: {error}", parameter.name)
                })?);
            }
            other => {
                return Err(format!(
                    "{} has unsupported parameter placement `{other}`",
                    contract.name
                ))
            }
        }
    }
    if path.contains('{') || path.contains('}') {
        return Err(format!(
            "{} left an unresolved path parameter in `{path}`",
            contract.name
        ));
    }
    let mut path = path.trim_start_matches('/').to_string();
    if !query.is_empty() {
        path.push('?');
        path.push_str(&query.join("&"));
    }
    Ok((path, body))
}

pub(crate) fn execute(
    contract: &AriContract,
    input: Value,
    host: &mut Host,
) -> Result<Value, String> {
    if contract.websocket {
        return Err(format!(
            "{} is a WebSocket route and cannot be executed as REST",
            contract.name
        ));
    }
    let (path, body) = request_parts(contract, input)?;
    let headers = if body.is_some() {
        vec![("content-type", "application/json")]
    } else {
        Vec::new()
    };
    if contract.response_kind == "binary" {
        let response = host.http_blob_ref(
            ENDPOINT_REF,
            &contract.method,
            &path,
            Some(AUTH_PURPOSE),
            &headers,
            body.as_deref(),
            "asterisk-ari-download",
            BINARY_MAX_BYTES,
            BINARY_TIMEOUT_MS,
        )?;
        return Ok(json!({
            "blob_ref": response.blob_ref,
            "size": response.size,
            "sha256": response.sha256,
        }));
    }

    let response = host.http_ref(
        ENDPOINT_REF,
        &contract.method,
        &path,
        Some(AUTH_PURPOSE),
        &headers,
        body.as_deref(),
    )?;
    if !response.is_success() {
        return Err(format!(
            "{} {} returned {}: {}",
            contract.method, path, response.status, response.body
        ));
    }
    match contract.response_kind.as_str() {
        "void" => Ok(json!({"status": response.status})),
        "json" => response.json(),
        other => Err(format!(
            "{} has unsupported response kind `{other}`",
            contract.name
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_kit::MockHost;

    fn contract(name: &str) -> AriContract {
        contracts()
            .expect("generated contracts")
            .into_iter()
            .find(|contract| contract.name == name)
            .unwrap_or_else(|| panic!("missing `{name}`"))
    }

    #[test]
    fn executor_encodes_only_declared_path_query_and_body_inputs() {
        let contract = contract("asterisk.ari.events.userEvent");
        let mut mock = MockHost::default()
            .with_endpoint_ref(ENDPOINT_REF, "http://localhost:8088/ari/")
            .with_http("events/user/a%2Fb", json!({}));
        let mut host = Host::new(&mut mock);
        let output = execute(
            &contract,
            json!({
                "eventName": "a/b",
                "application": "support app",
                "source": ["channel:one", "bridge:two"],
                "variables": {"variables": {"ticket": "42"}},
                "ignored": "never encoded"
            }),
            &mut host,
        )
        .expect("ARI call");
        assert_eq!(output, json!({"status": 200}));
        let calls = mock.calls.borrow();
        let payload = &calls
            .iter()
            .find(|(command, _)| command == "http.do")
            .unwrap()
            .1;
        assert_eq!(payload["endpoint_ref"], ENDPOINT_REF);
        assert_eq!(payload["auth_purpose"], AUTH_PURPOSE);
        assert!(payload["path"]
            .as_str()
            .unwrap()
            .contains("application=support%20app"));
        assert!(payload["path"]
            .as_str()
            .unwrap()
            .contains("source=channel%3Aone"));
        assert!(!payload["path"].as_str().unwrap().contains("ignored"));
        assert!(payload.get("body_b64").is_some());
    }

    #[test]
    fn executor_preserves_unknown_json_response_fields_and_reports_non_success() {
        let contract = contract("asterisk.ari.device_states.get");
        let mut success = MockHost::default()
            .with_endpoint_ref(ENDPOINT_REF, "http://localhost:8088/ari/")
            .with_http(
                "deviceStates/PJSIP%2F7",
                json!({"name":"PJSIP/7","future":true}),
            );
        let result = execute(
            &contract,
            json!({"deviceName":"PJSIP/7"}),
            &mut Host::new(&mut success),
        )
        .expect("JSON response");
        assert_eq!(result["future"], true);

        let mut failure = MockHost::default()
            .with_endpoint_ref(ENDPOINT_REF, "http://localhost:8088/ari/")
            .with_http_status_body("deviceStates/missing", 404, "no such device");
        let error = execute(
            &contract,
            json!({"deviceName":"missing"}),
            &mut Host::new(&mut failure),
        )
        .expect_err("non-2xx must fail");
        assert!(error.contains("404: no such device"), "{error}");
    }

    #[test]
    fn executor_rejects_websocket_without_host_io() {
        let contract = contract("asterisk.ari.events.eventWebsocket");
        let mut mock = MockHost::default();
        let error = execute(
            &contract,
            json!({"app":["support"]}),
            &mut Host::new(&mut mock),
        )
        .expect_err("WebSocket is not REST");
        assert!(error.contains("cannot be executed as REST"));
        assert!(mock.calls.borrow().is_empty());
    }

    #[test]
    fn binary_response_stays_in_host_blob_store() {
        let contract = contract("asterisk.ari.recordings.getStoredFile");
        let raw = b"not UTF-8: \xff".to_vec();
        let mut mock = MockHost::default()
            .with_endpoint_ref(ENDPOINT_REF, "http://localhost:8088/ari/")
            .with_http_bytes("recordings/stored/demo/file", raw);
        let output = execute(
            &contract,
            json!({"recordingName":"demo"}),
            &mut Host::new(&mut mock),
        )
        .expect("blob response");
        assert!(output["blob_ref"]
            .as_str()
            .unwrap()
            .starts_with("mockblob-"));
        assert_eq!(output["size"], 12);
        assert!(mock
            .blobs
            .borrow()
            .contains_key(output["blob_ref"].as_str().unwrap()));
        let calls = mock.calls.borrow();
        let payload = &calls
            .iter()
            .find(|(command, _)| command == "http.do")
            .unwrap()
            .1;
        assert_eq!(payload["response_blob"]["max_bytes"], BINARY_MAX_BYTES);
        assert_eq!(payload["timeout_ms"], BINARY_TIMEOUT_MS);
    }
}
