//! D-154: an embedded agent can load a subprocess plugin's operations as policy-gated tools.
//!
//! Only compiled/run under `--features plugins` (the fixture plugin binary
//! `flux_sdk_plugin_fixture` builds under the same feature, so its exe path is available here via
//! `CARGO_BIN_EXE_*`).
#![cfg(feature = "plugins")]

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::Result;
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::plugins::PluginDescriptor;
use flux_sdk::FlowClient;
use serde_json::json;

/// The plugin op dispatches directly (no planning), so the provider is never called — panic if it
/// is, to prove the plugin path doesn't touch the model.
struct NeverProvider;
#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        panic!("a plugin-op flow must not invoke the provider");
    }
}

fn fixture_descriptor() -> PluginDescriptor {
    PluginDescriptor {
        program: env!("CARGO_BIN_EXE_flux_sdk_plugin_fixture").to_string(),
        args: vec![],
        pinned: None,
        ..Default::default()
    }
}

fn upper_flow() -> flux_sdk::flow::DraftAst {
    serde_json::from_value(json!({
        "body": [
            { "kind": "call", "op": "fixture.upper",
              "args": [{ "kind": "lit", "value": { "text": "hello" } }] }
        ]
    }))
    .unwrap()
}

/// The fixture plugin's `upper` op is registered as `fixture.upper`, appears in `op_names()`, and
/// dispatches through the safety envelope: allowed with `auto_approve`, it uppercases the input.
#[tokio::test]
async fn plugin_op_registers_dispatches_and_uppercases_when_allowed() {
    let root = std::env::temp_dir().join(format!("flux-sdk-plugins-ok-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let mut client = FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(Arc::new(NeverProvider), &root)
        .unwrap();
    client
        .register_plugin("fixture", &fixture_descriptor())
        .await
        .expect("the fixture plugin loads");

    // The op is projected into the catalog under its `<plugin>.<op>` name.
    assert!(
        client.op_names().iter().any(|n| n == "fixture.upper"),
        "the plugin op appears in op_names(): {:?}",
        client.op_names()
    );

    // Dispatches through the envelope (approved) → the plugin uppercases the text.
    let out = client.execute(&upper_flow()).await.unwrap();
    assert_eq!(out.tool_calls, vec!["fixture.upper"]);
    assert!(
        out.result.contains("HELLO"),
        "the plugin op ran and uppercased the input: {}",
        out.result
    );

    std::fs::remove_dir_all(&root).ok();
}

/// The same op under the default (deny) approver is gated: a `Risk::Medium` plugin op requires
/// approval, so without a grant it does not execute — the input is never uppercased.
#[tokio::test]
async fn plugin_op_is_denied_by_the_default_approver() {
    let root = std::env::temp_dir().join(format!("flux-sdk-plugins-deny-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    // No `auto_approve` and no allow rule → the default DenyApprover gates the op.
    let mut client = FlowClient::builder()
        .model("mock")
        .build(Arc::new(NeverProvider), &root)
        .unwrap();
    client
        .register_plugin("fixture", &fixture_descriptor())
        .await
        .expect("the fixture plugin loads");

    let result = client.execute(&upper_flow()).await;
    // Whether the denial surfaces as an Err or as a non-uppercased result, the op must NOT have run.
    let uppercased = result
        .as_ref()
        .map(|o| o.result.contains("HELLO"))
        .unwrap_or(false);
    assert!(
        !uppercased,
        "a gated plugin op must not execute without approval: {result:?}"
    );

    std::fs::remove_dir_all(&root).ok();
}
