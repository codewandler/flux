//! An example plugin that exercises a host-capability callback: its `viahost` operation calls back
//! into the host (command `ping`) and wraps the host's reply. Used by the host round-trip test.

use serde_json::{json, Value};

use flux_plugin::{
    serve, GuestHost, OperationSpec, PluginCapabilities, PluginHandler, PluginManifest,
};

struct Caps;

impl PluginHandler for Caps {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "caps".into(),
            version: "0.1.0".into(),
            operations: vec![
                OperationSpec {
                    name: "viahost".into(),
                    description: "Echo `msg` back through a host callback".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {"msg": {"type": "string"}},
                        "required": ["msg"]
                    }),
                    effects: Vec::new(),
                    risk: None,
                    ..OperationSpec::default()
                },
                OperationSpec {
                    name: "readenv".into(),
                    description:
                        "Isolation probe: report this plugin process's own view of an env var"
                            .into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {"key": {"type": "string"}},
                        "required": ["key"]
                    }),
                    effects: Vec::new(),
                    risk: None,
                    ..OperationSpec::default()
                },
            ],
            capabilities: PluginCapabilities::default(),
            ..PluginManifest::default()
        }
    }

    fn call(
        &self,
        operation: &str,
        input: Value,
        host: &mut dyn GuestHost,
    ) -> Result<Value, String> {
        match operation {
            "viahost" => {
                let msg = input.get("msg").cloned().unwrap_or(Value::Null);
                // Call back into the host; the host services `ping` and returns a result.
                let reply = host.host_call("ping", json!({ "echo": msg }))?;
                Ok(json!({ "host_said": reply }))
            }
            "readenv" => {
                // Read this plugin process's OWN environment directly (NOT via the host). Under the
                // guarded spawn path the env is cleared to the allow-list, so a non-allow-listed
                // (e.g. secret) var resolves to null — the plugin can't reach around the host.
                let key = input
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                Ok(json!({ "value": std::env::var(key).ok() }))
            }
            other => Err(format!("unknown operation: {other}")),
        }
    }
}

fn main() {
    serve(Caps);
}
