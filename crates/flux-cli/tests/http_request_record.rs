//! C-304 — `http.request` returns a **record**, and an authored flow can select a field from it.
//!
//! This is the story's headline proof and it lives here rather than in `flux-web` for a layering
//! reason: `flux-web` is L5 and the flow interpreter (`flux-flow`) is L3, so only a surface crate
//! sees both. `flux-cli` is the surface that already depends on each.
//!
//! No network: a one-shot loopback server serves the response, and the `web` egress scope is
//! granted for the test (loopback is private, so an ungranted request would be refused by the SSRF
//! guard — which is the behaviour `flux-web`'s own tests cover).

use std::sync::Arc;

use flux_flow::AgentSink;
use flux_runtime::{AllowApprover, Executor, PermissionManager, ToolContext, ToolRegistry};
use flux_system::net::PrivateNetAllow;
use flux_system::{System, Workspace};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A no-op sink (every `AgentSink` method has a default).
#[derive(Default)]
struct NullSink;
impl AgentSink for NullSink {}

/// A one-shot loopback HTTP server: accepts one connection, reads the request, writes a canned
/// response. Returns its `http://127.0.0.1:<port>` base URL.
async fn one_shot(
    status_line: &'static str,
    content_type: &'static str,
    body: &'static str,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    format!("http://{addr}")
}

/// Lower and run `source` against a registry holding the real `http.request` tool, exactly the way
/// `flux flow run` does: the analyzer gate first, then `execute_flow` over a live `Executor`.
async fn run_flow(source: &str) -> Result<String, String> {
    let ast = flux_lang::parse::parse(source).expect("the flow parses");

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(flux_web::http::HttpRequestTool::new(
        &flux_web::WebOptions {
            private_net: PrivateNetAllow::Any,
            ..Default::default()
        },
    )));

    // The same gate `flux flow run` applies — unknown ops, missing required params, type conflicts.
    let ops = flux_flow::registry::OpRegistry::new(&registry);
    flux_flow::analyze::lower(&ast, &ops, &Default::default())
        .unwrap_or_else(|diags| panic!("the flow fails the flow-run gate: {diags:?}"));

    let dir = std::env::temp_dir().join(format!("flux-c304-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let executor = Executor::new(
        registry,
        PermissionManager::from_rules(&["*".into()], &[]),
        Arc::new(AllowApprover),
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
    );
    let store = flux_flow::state::FlowStore::in_memory().unwrap();
    let mut sink = NullSink;
    flux_flow::runtime::execute_flow(&store, &executor, "c304", &ast, &mut sink)
        .await
        .map(|outcome| outcome.result)
        .map_err(|e| e.to_string())
}

/// **The story's failing-first test.** A flow selects `.body.data.id` out of a JSON response.
///
/// Before C-304 this failed — and failed the way the story calls the worst version of the failure:
/// the whole response arrived as one flat `HTTP 200 …` string, so the field access had nothing to
/// traverse. Afterwards the bound value is the record `{status, headers, body}` and the selection
/// resolves.
#[tokio::test]
async fn a_flow_selects_a_field_from_a_json_response_body() {
    let base = one_shot(
        "200 OK",
        "application/json",
        r#"{"data":{"id":"cus_42","plan":"pro"}}"#,
    )
    .await;
    let source = format!(
        "flow probe\n  $resp = http.request({{ url: \"{base}/v1/customers\" }})\n  $id = $resp.body.data.id\n  return $id\n"
    );
    let result = run_flow(&source)
        .await
        .unwrap_or_else(|e| panic!("the flow must complete: {e}"));
    assert_eq!(
        result.trim(),
        "cus_42",
        "the flow selected the id out of the parsed response body"
    );
}

/// The other half of the record a caller needs: the status is a number a flow can compare, not a
/// substring it has to scrape out of a rendered first line.
#[tokio::test]
async fn a_flow_reads_the_status_and_a_response_header_off_the_record() {
    let base = one_shot("201 Created", "application/json", r#"{"ok":true}"#).await;
    let source = format!(
        "flow probe\n  $resp = http.request({{ url: \"{base}/v1/things\", method: \"POST\" }})\n  $status = $resp.status\n  return $status\n"
    );
    let result = run_flow(&source)
        .await
        .unwrap_or_else(|e| panic!("the flow must complete: {e}"));
    assert_eq!(
        result.trim(),
        "201",
        "the status is a value the flow can bind and compare"
    );
}
