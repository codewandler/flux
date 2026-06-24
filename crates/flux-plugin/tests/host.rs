//! End-to-end: the host spawns plugin subprocesses and drives them over the framed stdio protocol
//! (manifest discovery, operation calls, host-capability callbacks, and tool projection).

use std::sync::Arc;

use async_trait::async_trait;
use flux_plugin::{HostCapabilities, PluginHost};
use serde_json::{json, Value};

#[tokio::test]
async fn host_discovers_manifest_and_calls_operation() {
    let exe = env!("CARGO_BIN_EXE_echo_plugin");
    let mut host = PluginHost::spawn(exe, &[]).await.unwrap();

    let manifest = host.manifest().await.unwrap();
    assert_eq!(manifest.name, "echo");
    assert!(manifest.operations.iter().any(|o| o.name == "upper"));

    let out = host
        .call("upper", json!({"text": "hello plugin"}))
        .await
        .unwrap();
    assert_eq!(out["text"], "HELLO PLUGIN");

    // unknown operation surfaces as an error
    assert!(host.call("nope", json!({})).await.is_err());

    host.shutdown().await.unwrap();
}

/// A test host capability: answers `ping` by echoing the payload back.
struct PingCaps;

#[async_trait]
impl HostCapabilities for PingCaps {
    async fn handle(&self, command: &str, payload: &Value) -> Result<Value, String> {
        if command == "ping" {
            Ok(json!({ "pong": payload.get("echo").cloned().unwrap_or(Value::Null) }))
        } else {
            Err(format!("unknown capability {command}"))
        }
    }
}

#[tokio::test]
async fn host_services_plugin_capability_callback() {
    let exe = env!("CARGO_BIN_EXE_caps_plugin");
    let mut host = PluginHost::spawn(exe, &[]).await.unwrap();

    // The plugin's `viahost` op calls back into the host (`ping`); the round-trip returns the echo.
    let out = host
        .call_with_host("viahost", json!({"msg": "round-trip"}), &PingCaps)
        .await
        .unwrap();
    assert_eq!(out["host_said"]["pong"], "round-trip");

    // Without host capabilities, the same callback is denied.
    let denied = host.call("viahost", json!({"msg": "x"})).await;
    assert!(denied.is_err());

    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn plugin_operations_project_as_tools() {
    use flux_plugin::{load_plugin_tools, DenyHostCaps};

    let exe = env!("CARGO_BIN_EXE_echo_plugin");
    let (tools, host) = load_plugin_tools(exe, &[], |_| Arc::new(DenyHostCaps))
        .await
        .unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].spec().name, "echo.upper");
    assert_eq!(
        tools[0].permission_subjects(&json!({})),
        vec!["echo.upper".to_string()]
    );
    // The op declares no effects, so it projects a conservative effect set and is NOT a no-op for
    // the authorization floor (which would otherwise skip plugin ops entirely).
    assert!(
        !tools[0].spec().effects.is_empty(),
        "plugin op must declare effects so the policy floor gates it"
    );

    // Drive the projected tool through the Tool interface.
    let dir = std::env::temp_dir().join(format!("flux-plugintool-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = flux_runtime::ToolContext::new(Arc::new(flux_system::System::new(
        flux_system::Workspace::new(&dir).unwrap(),
    )));
    let r = tools[0].execute(&ctx, json!({"text": "hi"})).await.unwrap();
    assert!(!r.is_error);
    assert!(r.content.contains("HI"));

    // Release the tools' shared host references, then shut the subprocess down.
    drop(tools);
    Arc::try_unwrap(host)
        .ok()
        .expect("host is sole owner")
        .into_inner()
        .shutdown()
        .await
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
}
