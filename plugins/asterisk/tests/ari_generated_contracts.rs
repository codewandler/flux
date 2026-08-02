#[path = "../src/ari.rs"]
mod ari;

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn spec_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("specs/ari-22.10.1/api-docs")
}

fn vendor_contracts() -> (Vec<Value>, BTreeSet<String>) {
    let mut operations = Vec::new();
    let mut models = BTreeSet::new();
    let mut documents: Vec<_> = fs::read_dir(spec_dir())
        .expect("read ARI api-docs")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect();
    documents.sort();

    for path in documents {
        let document: Value = serde_json::from_slice(&fs::read(&path).expect("read ARI document"))
            .expect("valid JSON");
        let resource = path.file_stem().unwrap().to_str().unwrap();
        for api in document["apis"].as_array().expect("apis array") {
            for operation in api["operations"].as_array().expect("operations array") {
                let parameters = operation
                    .get("parameters")
                    .and_then(Value::as_array)
                    .map(|parameters| {
                        parameters
                            .iter()
                            .map(|parameter| {
                                json!({
                                    "name": parameter["name"],
                                    "description": parameter.get("description").cloned().unwrap_or(Value::String(String::new())),
                                    "placement": parameter["paramType"],
                                    "required": parameter["required"],
                                    "allow_multiple": parameter["allowMultiple"],
                                    "data_type": parameter["dataType"],
                                    "enum_values": parameter.pointer("/allowableValues/values").cloned().unwrap_or_else(|| json!([])),
                                    "default_value": parameter.get("defaultValue").cloned().unwrap_or(Value::Null),
                                    "allowable_values": parameter.get("allowableValues").cloned().unwrap_or(Value::Null),
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                operations.push(json!({
                    "resource": resource,
                    "nickname": operation["nickname"],
                    "method": operation["httpMethod"],
                    "path": api["path"],
                    "websocket": operation.get("upgrade").and_then(Value::as_str) == Some("websocket"),
                    "response_class": operation["responseClass"],
                    "parameters": parameters,
                }));
            }
        }
        for model in document["models"]
            .as_object()
            .expect("models object")
            .keys()
        {
            assert!(models.insert(model.clone()), "duplicate model `{model}`");
        }
    }
    operations.sort_by_key(|operation| {
        (
            operation["resource"].as_str().unwrap().to_owned(),
            operation["nickname"].as_str().unwrap().to_owned(),
        )
    });
    (operations, models)
}

#[test]
fn generated_operations_match_every_vendor_contract_in_both_directions() {
    let (vendor, _) = vendor_contracts();
    let mut generated = ari::source_operations().expect("generated operation facts");
    generated.sort_by_key(|operation| {
        (
            operation["resource"].as_str().unwrap().to_owned(),
            operation["nickname"].as_str().unwrap().to_owned(),
        )
    });
    assert_eq!(generated, vendor);
    assert_eq!(generated.len(), 109);
    assert_eq!(
        generated
            .iter()
            .filter(|operation| !operation["websocket"].as_bool().unwrap())
            .count(),
        108
    );
}

#[test]
fn generated_model_schemas_resolve_every_vendor_model() {
    let (_, vendor_models) = vendor_contracts();
    let schemas = ari::model_schemas().expect("generated model schemas");
    let generated_models: BTreeSet<_> = schemas.keys().cloned().collect();
    assert_eq!(generated_models, vendor_models);
    assert_eq!(generated_models.len(), 85);

    for (name, schema) in &schemas {
        let encoded = serde_json::to_string(schema).expect("schema JSON");
        for reference in encoded.match_indices("#/$defs/") {
            let rest = &encoded[reference.0 + "#/$defs/".len()..];
            let target = rest.split('"').next().unwrap();
            assert!(
                schemas.contains_key(target),
                "model `{name}` has unresolved ref `{target}`"
            );
        }
    }
}

#[test]
fn every_generated_input_is_closed_and_safety_is_explicit() {
    let contracts = ari::contracts().expect("generated contracts");
    let sources = ari::source_operations().expect("source facts");
    assert_eq!(contracts.len(), 109);
    for (contract, source) in contracts.into_iter().zip(sources) {
        assert_eq!(contract.input_schema["type"], "object", "{}", contract.name);
        assert_eq!(
            contract.input_schema["additionalProperties"], false,
            "{}",
            contract.name
        );
        assert!(
            !contract.effects.is_empty(),
            "{} has no effects",
            contract.name
        );
        assert!(!contract.risk.is_empty(), "{} has no risk", contract.name);
        assert!(
            !contract.idempotency.is_empty(),
            "{} has no idempotency",
            contract.name
        );
        if contract.method == "DELETE" {
            assert_eq!(contract.risk, "destructive", "{}", contract.name);
            assert!(
                contract
                    .semantic_effects
                    .iter()
                    .any(|effect| effect == "delete"),
                "{}",
                contract.name
            );
        }
        let required = contract.input_schema["required"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for parameter in source["parameters"].as_array().unwrap() {
            let name = parameter["name"].as_str().unwrap();
            let property = &contract.input_schema["properties"][name];
            assert_eq!(
                property["x-ari-placement"], parameter["placement"],
                "{}.{name}",
                contract.name
            );
            assert_eq!(
                required.iter().any(|value| value.as_str() == Some(name)),
                parameter["required"].as_bool().unwrap_or(false),
                "{}.{name}",
                contract.name
            );
            let multiple = parameter["allow_multiple"].as_bool().unwrap_or(false);
            let typed = if multiple {
                assert_eq!(property["type"], "array", "{}.{name}", contract.name);
                &property["items"]
            } else {
                property
            };
            let expected_type = match parameter["data_type"].as_str().unwrap() {
                "string" => "string",
                "int" | "long" => "integer",
                "boolean" => "boolean",
                "object" | "containers" => "object",
                other => panic!("uncovered parameter type `{other}`"),
            };
            assert_eq!(typed["type"], expected_type, "{}.{name}", contract.name);
            if !parameter["enum_values"].as_array().unwrap().is_empty() {
                assert_eq!(
                    typed["enum"], parameter["enum_values"],
                    "{}.{name}",
                    contract.name
                );
            }
        }
    }
}

#[test]
fn reviewed_risk_classes_distinguish_live_media_from_ordinary_state_writes() {
    let contracts = ari::contracts().expect("generated contracts");
    let by_name = |name: &str| {
        contracts
            .iter()
            .find(|contract| contract.name == name)
            .unwrap_or_else(|| panic!("missing `{name}`"))
    };
    for name in [
        "asterisk.ari.channels.originate",
        "asterisk.ari.bridges.play",
        "asterisk.ari.recordings.stop",
        "asterisk.ari.events.userEvent",
    ] {
        assert_eq!(by_name(name).risk, "high", "{name}");
    }
    assert_eq!(by_name("asterisk.ari.device_states.update").risk, "medium");
    assert_eq!(by_name("asterisk.ari.mailboxes.update").risk, "medium");
}

#[test]
fn model_inheritance_and_registered_output_schemas_are_complete() {
    let schemas = ari::model_schemas().expect("generated model schemas");
    let expected = [("Message", 2usize), ("Event", 45usize)];
    for (name, subtype_count) in expected {
        let alternatives = schemas[name]["anyOf"]
            .as_array()
            .expect("inheritance anyOf");
        assert_eq!(alternatives.len(), subtype_count, "{name} subtype count");
        for alternative in alternatives {
            let target = alternative["$ref"]
                .as_str()
                .unwrap()
                .trim_start_matches("#/$defs/");
            assert!(schemas.contains_key(target), "{name} subtype `{target}`");
        }
    }

    let builder = ari::register(host_kit::PluginBuilder::new("asterisk-test", "0.0.0"))
        .expect("generated registration");
    let manifest = builder.manifest();
    assert_eq!(manifest.operations.len(), 108);
    for operation in manifest.operations {
        let output = operation.output_schema.expect("output schema");
        if output.get("$ref").is_some() || output["type"] == "array" {
            assert_eq!(
                output["$defs"].as_object().unwrap().len(),
                85,
                "{}",
                operation.name
            );
        }
    }
}

#[test]
fn generator_refuses_bad_input_without_touching_output_and_committed_output_is_current() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let generator = manifest_dir.join("scripts/generate-ari-contracts.py");
    let check = Command::new(&generator)
        .arg("--check")
        .status()
        .expect("run generator check");
    assert!(check.success(), "committed generated contracts are stale");

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "asterisk-ari-generator-{}-{nonce}",
        std::process::id()
    ));
    let source = temp.join("api-docs");
    fs::create_dir_all(&source).expect("create isolated source");
    for entry in fs::read_dir(spec_dir()).expect("read source") {
        let path = entry.expect("entry").path();
        fs::copy(&path, source.join(path.file_name().unwrap())).expect("copy source");
    }
    fs::write(source.join("events.json"), b"{ malformed").expect("corrupt isolated source");
    let output = temp.join("ari_generated.rs");
    fs::write(&output, b"sentinel\n").expect("write sentinel");
    let status = Command::new(&generator)
        .args([
            "--source-dir",
            source.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .status()
        .expect("run malformed generator");
    assert!(!status.success(), "malformed source must fail");
    assert_eq!(
        fs::read(&output).unwrap(),
        b"sentinel\n",
        "failed generation changed output"
    );
    fs::remove_dir_all(&temp).expect("remove isolated generator fixture");
}

#[test]
fn production_sources_do_not_read_or_embed_swagger() {
    for source in ["src/main.rs", "src/ari.rs", "src/ari_generated.rs"] {
        let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(source)).unwrap();
        assert!(
            !text.contains("include_str!"),
            "{source} embeds source text"
        );
        assert!(
            !text.contains("specs/ari-"),
            "{source} names vendored Swagger at runtime"
        );
        assert!(
            !text.contains("read_to_string("),
            "{source} reads source text at runtime"
        );
    }
}
