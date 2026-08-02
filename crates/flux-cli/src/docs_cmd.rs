use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;

use crate::{resolve_cli_provider, resolve_model_spec};

/// Serve the release-matched public site embedded in this binary.
pub(super) async fn run_docs(bind: SocketAddr, requested_model: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config = flux_runtime::metadata::load_config(&cwd)?;
    let model_spec = resolve_model_spec(&requested_model, &config);
    let resolved = resolve_cli_provider(&model_spec, false)?;
    flux_server::public_docs::serve(
        bind,
        env!("CARGO_PKG_VERSION"),
        Arc::from(resolved.provider),
        resolved.model,
    )
    .await
}
