//! `flux-lsp` — stdio bootstrap. Everything else lives in the library (see `lib.rs`).

#[tokio::main]
async fn main() {
    flux_lsp::serve_io(
        tokio::io::stdin(),
        tokio::io::stdout(),
        flux_lsp::WorkspacePolicy::ClientProvided,
    )
    .await;
}
