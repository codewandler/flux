//! `flux-lsp` — stdio bootstrap. Everything else lives in the library (see `lib.rs`).

use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(flux_lsp::Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
