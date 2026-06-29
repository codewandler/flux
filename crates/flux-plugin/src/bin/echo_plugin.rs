//! A minimal example flux plugin: advertises an `upper` operation that uppercases `text`.
//! Build target name `echo_plugin`; used by the host integration test.

use serde_json::{json, Value};

use flux_plugin::{
    serve, GuestHost, OperationSpec, PluginCapabilities, PluginHandler, PluginManifest,
};

struct Echo;

impl PluginHandler for Echo {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "echo".into(),
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
    serve(Echo);
}
