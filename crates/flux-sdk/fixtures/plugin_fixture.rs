//! A minimal fixture flux plugin for the SDK `plugins` integration test — advertises one pure
//! `upper` operation that uppercases `text`. Mirrors `flux-plugin`'s `echo_plugin`, replicated here
//! so `flux-sdk`'s own integration test can reach it via `CARGO_BIN_EXE_flux_sdk_plugin_fixture`
//! (a binary's exe path is only exported to tests of the crate that declares it). Built only under
//! `--features plugins`.

use serde_json::{json, Value};

use flux_plugin::{
    serve, GuestHost, OperationSpec, PluginCapabilities, PluginHandler, PluginManifest,
};

struct Fixture;

impl PluginHandler for Fixture {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "fixture".into(),
            version: "0.1.0".into(),
            operations: vec![OperationSpec {
                name: "upper".into(),
                description: "Uppercase the `text` field".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"]
                }),
                effects: Vec::new(), // pure transform — no IO
                risk: None,
                ..OperationSpec::default()
            }],
            capabilities: PluginCapabilities::default(), // requests no host capabilities
            ..PluginManifest::default()
        }
    }

    fn call(
        &self,
        operation: &str,
        input: Value,
        _host: &mut dyn GuestHost,
    ) -> Result<Value, String> {
        match operation {
            "upper" => {
                let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
                Ok(json!({ "text": text.to_uppercase() }))
            }
            other => Err(format!("unknown operation: {other}")),
        }
    }
}

fn main() {
    serve(Fixture);
}
