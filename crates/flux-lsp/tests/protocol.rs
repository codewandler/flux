//! Protocol-level coverage: the server driven over an **in-memory duplex**, as a client drives it
//! (L-91).
//!
//! Every other test in this crate calls an internal function. That leaves one class of bug
//! invisible: a capability advertised in `initialize` that no handler answers — which is exactly
//! what `range: Some(false)` alongside a full-only implementation was. These tests drive
//! `LspService` over a real JSON-RPC channel and assert the pairing:
//!
//! - [`a_scripted_session_answers_every_request`] walks a realistic session and requires each
//!   response to be well-formed;
//! - [`every_advertised_capability_has_a_handler`] reads the capabilities the server *itself*
//!   advertised and requires each to be both mapped to a method here and answered by the server, so
//!   adding a capability without a handler (or without coverage) fails the suite.

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tower_lsp::{LspService, Server};

const SOURCE: &str = "flow greet(name: String)\n  $msg = fmt(\"hi\")\n  return $msg\n";
const URI: &str = "file:///workspace/greet.flux";

/// A client speaking LSP to an in-process server over a duplex pair.
struct Harness {
    writer: DuplexStream,
    reader: BufReader<DuplexStream>,
    next_id: i64,
}

impl Harness {
    fn start() -> Self {
        let (client_writes, server_reads) = tokio::io::duplex(64 * 1024);
        let (server_writes, client_reads) = tokio::io::duplex(64 * 1024);
        let (service, socket) = LspService::new(flux_lsp::Backend::new);
        tokio::spawn(Server::new(server_reads, server_writes, socket).serve(service));
        Harness {
            writer: client_writes,
            reader: BufReader::new(client_reads),
            next_id: 0,
        }
    }

    fn start_fixed(root: std::path::PathBuf) -> Self {
        let (client_writes, server_reads) = tokio::io::duplex(64 * 1024);
        let (server_writes, client_reads) = tokio::io::duplex(64 * 1024);
        tokio::spawn(flux_lsp::serve_io(
            server_reads,
            server_writes,
            flux_lsp::WorkspacePolicy::Fixed(Some(root)),
        ));
        Harness {
            writer: client_writes,
            reader: BufReader::new(client_reads),
            next_id: 0,
        }
    }

    async fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).expect("serializable");
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.writer
            .write_all(frame.as_bytes())
            .await
            .expect("the server is listening");
        self.writer.flush().await.expect("flush");
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await;
    }

    /// Send a request and return its response object, skipping the notifications (diagnostics, log
    /// messages) the server interleaves.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        // `shutdown` takes no params at all; a `"params": null` is a protocol error.
        let message = if params.is_null() {
            json!({"jsonrpc": "2.0", "id": id, "method": method})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        };
        self.send(message).await;
        loop {
            let message = self.read_message().await;
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                return message;
            }
        }
    }

    async fn read_message(&mut self) -> Value {
        let mut length = None;
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).await.expect("header line");
            assert!(read > 0, "the server closed the connection");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = Some(value.parse::<usize>().expect("a numeric Content-Length"));
            }
        }
        let length = length.expect("every frame carries a Content-Length");
        let mut body = vec![0u8; length];
        self.reader.read_exact(&mut body).await.expect("frame body");
        serde_json::from_slice(&body).expect("a JSON-RPC message")
    }

    /// `initialize` → `initialized` → `didOpen`, returning the advertised capabilities.
    async fn open(&mut self) -> Value {
        let response = self
            .request(
                "initialize",
                json!({"capabilities": {}, "rootUri": "file:///workspace"}),
            )
            .await;
        let capabilities = response["result"]["capabilities"].clone();
        self.notify("initialized", json!({})).await;
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": URI, "languageId": "flux", "version": 1, "text": SOURCE
            }}),
        )
        .await;
        capabilities
    }
}

#[tokio::test]
async fn fixed_workspace_ignores_a_browser_supplied_root() {
    let fixed = tempfile::tempdir().unwrap();
    let hostile = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(hostile.path().join(".flux/flows")).unwrap();
    std::fs::write(
        hostile.path().join(".flux/flows/host-only.flux"),
        "op host_only() -> String\n  description \"host only\"\n  risk \"low\"\n  idempotency \"idempotent\"\n  return \"secret\"\n",
    )
    .unwrap();

    let mut lsp = Harness::start_fixed(fixed.path().to_path_buf());
    let hostile_uri = tower_lsp::lsp_types::Url::from_directory_path(hostile.path())
        .unwrap()
        .to_string();
    let response = lsp
        .request(
            "initialize",
            json!({"capabilities": {}, "rootUri": hostile_uri}),
        )
        .await;
    assert_answered("initialize", &response);
    lsp.notify("initialized", json!({})).await;
    lsp.notify(
        "textDocument/didOpen",
        json!({"textDocument": {
            "uri": URI, "languageId": "flux", "version": 1,
            "text": "flow demo -> String\n  return host_only()\n"
        }}),
    )
    .await;

    let completion = lsp
        .request(
            "textDocument/completion",
            json!({"textDocument": doc(), "position": position(1, 18)}),
        )
        .await;
    let body = serde_json::to_string(&completion["result"]).unwrap();
    assert!(
        !body.contains("host_only"),
        "host root leaked into catalog: {body}"
    );
}

fn position(line: u32, character: u32) -> Value {
    json!({"line": line, "character": character})
}

fn doc() -> Value {
    json!({"uri": URI})
}

/// The request each advertised capability is answered by. A capability with no entry here fails
/// `every_advertised_capability_has_a_handler`, which is the point: adding one to `capabilities()`
/// forces both a handler and its coverage.
fn request_for(capability: &str) -> Option<(&'static str, Value)> {
    // The `$msg` bind on line 1, column 2 — a symbol every navigation request can resolve.
    let msg = json!({"textDocument": doc(), "position": position(1, 3)});
    let whole = json!({"start": position(0, 0), "end": position(2, 0)});
    Some(match capability {
        // Sync is notification-driven; `didOpen`/`didChange` in the scripted session cover it.
        "textDocumentSync" => return None,
        "completionProvider" => ("textDocument/completion", msg),
        "hoverProvider" => ("textDocument/hover", msg),
        "definitionProvider" => ("textDocument/definition", msg),
        "documentSymbolProvider" => (
            "textDocument/documentSymbol",
            json!({"textDocument": doc()}),
        ),
        "referencesProvider" => (
            "textDocument/references",
            json!({"textDocument": doc(), "position": position(1, 3),
                   "context": {"includeDeclaration": true}}),
        ),
        "renameProvider" => (
            "textDocument/rename",
            json!({"textDocument": doc(), "position": position(1, 3), "newName": "body"}),
        ),
        "documentFormattingProvider" => (
            "textDocument/formatting",
            json!({"textDocument": doc(), "options": {"tabSize": 2, "insertSpaces": true}}),
        ),
        "documentRangeFormattingProvider" => (
            "textDocument/rangeFormatting",
            json!({"textDocument": doc(), "range": whole,
                   "options": {"tabSize": 2, "insertSpaces": true}}),
        ),
        "semanticTokensProvider" => (
            "textDocument/semanticTokens/full",
            json!({"textDocument": doc()}),
        ),
        _ => return Some(("", Value::Null)),
    })
}

fn assert_answered(method: &str, response: &Value) {
    if let Some(error) = response.get("error") {
        panic!("{method} returned an error: {error}");
    }
    assert!(
        response.get("result").is_some(),
        "{method} returned neither a result nor an error: {response}"
    );
}

#[tokio::test]
async fn a_scripted_session_answers_every_request() {
    let mut lsp = Harness::start();
    lsp.open().await;

    // An incremental edit: rename the bind's right-hand side.
    lsp.notify(
        "textDocument/didChange",
        json!({
            "textDocument": {"uri": URI, "version": 2},
            "contentChanges": [{
                "range": {"start": position(1, 9), "end": position(1, 19)},
                "text": "fmt(\"hello\")"
            }],
        }),
    )
    .await;

    let completion = lsp
        .request(
            "textDocument/completion",
            json!({"textDocument": doc(), "position": position(1, 2)}),
        )
        .await;
    assert_answered("completion", &completion);

    let hover = lsp
        .request(
            "textDocument/hover",
            json!({"textDocument": doc(), "position": position(1, 3)}),
        )
        .await;
    assert_answered("hover", &hover);
    assert!(
        hover["result"]["contents"]["value"]
            .as_str()
            .is_some_and(|card| card.contains("$msg")),
        "hover answered about the `$msg` bind: {hover}"
    );

    let references = lsp
        .request(
            "textDocument/references",
            json!({"textDocument": doc(), "position": position(1, 3),
                   "context": {"includeDeclaration": true}}),
        )
        .await;
    assert_answered("references", &references);
    assert_eq!(
        references["result"].as_array().map(Vec::len),
        Some(2),
        "the bind and its one use: {references}"
    );

    let prepare = lsp
        .request(
            "textDocument/prepareRename",
            json!({"textDocument": doc(), "position": position(1, 3)}),
        )
        .await;
    assert_answered("prepareRename", &prepare);
    assert!(!prepare["result"].is_null(), "a `$var` is renameable");

    let rename = lsp
        .request(
            "textDocument/rename",
            json!({"textDocument": doc(), "position": position(1, 3), "newName": "body"}),
        )
        .await;
    assert_answered("rename", &rename);
    let edits = rename["result"]["changes"][URI]
        .as_array()
        .expect("a WorkspaceEdit for this document");
    assert_eq!(edits.len(), 2, "the bind and its use are both edited");
    assert!(edits.iter().all(|e| e["newText"] == "$body"), "{edits:?}");

    for (method, params) in [
        (
            "textDocument/formatting",
            json!({"textDocument": doc(), "options": {"tabSize": 2, "insertSpaces": true}}),
        ),
        (
            "textDocument/documentSymbol",
            json!({"textDocument": doc()}),
        ),
        (
            "textDocument/definition",
            json!({"textDocument": doc(), "position": position(2, 10)}),
        ),
        (
            "textDocument/semanticTokens/full",
            json!({"textDocument": doc()}),
        ),
    ] {
        let response = lsp.request(method, params).await;
        assert_answered(method, &response);
    }

    let shutdown = lsp.request("shutdown", json!(null)).await;
    assert_answered("shutdown", &shutdown);
}

#[tokio::test]
async fn every_advertised_capability_has_a_handler() {
    let mut lsp = Harness::start();
    let capabilities = lsp.open().await;
    let capabilities = capabilities.as_object().expect("a capabilities object");
    assert!(
        !capabilities.is_empty(),
        "the server advertises something to check"
    );

    for name in capabilities.keys() {
        let Some((method, params)) = request_for(name) else {
            continue;
        };
        assert!(
            !method.is_empty(),
            "`{name}` is advertised but this harness has no request for it — add one (and a \
             handler) rather than advertising a capability nothing answers"
        );
        let response = lsp.request(method, params).await;
        assert_answered(method, &response);
    }
}

#[tokio::test]
async fn semantic_tokens_range_and_delta_are_answered() {
    // The specific pairing this epic set out to fix: `range` and `full/delta` were advertised as
    // absent while the handler was full-only. Both are now real, so both must answer.
    let mut lsp = Harness::start();
    lsp.open().await;

    let range = lsp
        .request(
            "textDocument/semanticTokens/range",
            json!({"textDocument": doc(),
                   "range": {"start": position(1, 0), "end": position(1, 20)}}),
        )
        .await;
    assert_answered("semanticTokens/range", &range);

    let full = lsp
        .request(
            "textDocument/semanticTokens/full",
            json!({"textDocument": doc()}),
        )
        .await;
    assert_answered("semanticTokens/full", &full);
    let result_id = full["result"]["resultId"]
        .as_str()
        .expect("a full request mints a resultId so a delta can follow")
        .to_string();

    let delta = lsp
        .request(
            "textDocument/semanticTokens/full/delta",
            json!({"textDocument": doc(), "previousResultId": result_id}),
        )
        .await;
    assert_answered("semanticTokens/full/delta", &delta);
    assert!(
        delta["result"]["edits"].is_array(),
        "an unchanged document answers with an (empty) edit list: {delta}"
    );
}

#[tokio::test]
async fn a_position_that_is_not_a_symbol_does_not_pretend_to_be_renameable() {
    let mut lsp = Harness::start();
    lsp.open().await;
    // Column 6 on line 1 is the `=` between `$msg` and its value.
    let prepare = lsp
        .request(
            "textDocument/prepareRename",
            json!({"textDocument": doc(), "position": position(1, 7)}),
        )
        .await;
    assert_answered("prepareRename", &prepare);
    assert!(
        prepare["result"].is_null(),
        "punctuation is not renameable: {prepare}"
    );

    let rename = lsp
        .request(
            "textDocument/rename",
            json!({"textDocument": doc(), "position": position(1, 3), "newName": "not a name"}),
        )
        .await;
    assert!(
        rename.get("error").is_some(),
        "an illegal new name is rejected rather than silently corrupting the buffer: {rename}"
    );
}
