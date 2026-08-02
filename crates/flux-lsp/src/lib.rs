//! `flux-lsp` — a Language Server for Flux-Lang.
//!
//! Editor-grade support for `.flux` files, driven by the lossless CST front-end in `flux-lang`:
//! error-recovering **diagnostics** with real spans and stable codes, cursor-aware **completion**,
//! CST-precise **hover**, **go-to-definition / references / rename** over one scope model, canonical
//! comment- and order-preserving **formatting**, **document symbols**, and **semantic tokens** with
//! range and delta. Wired into Helix config-only — see `.helix/languages.toml`.
//!
//! # Module shape
//! `main.rs` is the stdio bootstrap and nothing else. The server is split along the lines the
//! original design named:
//!
//! | module | responsibility |
//! |---|---|
//! | [`server`] | capabilities + the `LanguageServer` handlers |
//! | [`document`] | the open-document store: text, **cached** CST, incremental reparse |
//! | [`convert`] | byte offsets ↔ LSP positions, edit application |
//! | [`catalog`] | host ops + workspace composites + the buffer's own |
//! | [`diagnostics`] | parse errors + analyzer findings, coded and severity-classified |
//! | [`completion`] | cursor context + scope-correct candidates |
//! | [`hover`] | the card for the token under the cursor |
//! | [`format`] | whole-document and range formatting (policy lives in `flux_lang::format_cst`) |
//! | [`scope`] | the CST scope model: definitions, references, rename |
//! | [`symbols`] | the document outline |
//! | [`semantic`] | semantic tokens, with range and delta |
//!
//! `tests/protocol.rs` drives the whole surface over an in-memory duplex, so an advertised
//! capability with no handler fails the suite.

pub mod catalog;
pub mod completion;
pub mod convert;
pub mod diagnostics;
pub mod document;
pub mod format;
pub mod hover;
pub mod scope;
pub mod semantic;
pub mod server;
pub mod symbols;

pub use server::{capabilities, Backend, WorkspacePolicy};

/// Serve the standard Content-Length-framed LSP protocol over arbitrary asynchronous IO.
///
/// Stdio and the documentation WebSocket bridge deliberately share this bootstrap; only their
/// workspace policy differs.
pub async fn serve_io<I, O>(input: I, output: O, workspace_policy: WorkspacePolicy)
where
    I: tokio::io::AsyncRead + Unpin,
    O: tokio::io::AsyncWrite + Unpin,
{
    let (service, socket) = tower_lsp::LspService::new(move |client| {
        Backend::with_workspace_policy(client, workspace_policy.clone())
    });
    tower_lsp::Server::new(input, output, socket)
        .serve(service)
        .await;
}
