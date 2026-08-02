use std::sync::Arc;

use anyhow::{Context, Result};

use crate::SystemAction;

pub(super) async fn run_system(action: SystemAction) -> Result<()> {
    match action {
        SystemAction::Serve {
            bind,
            workspace,
            cert,
            key,
            token_env,
        } => {
            let system = Arc::new(
                flux_system::System::from_env(&workspace)
                    .with_context(|| format!("open remote workspace `{}`", workspace.display()))?,
            );
            let token = system
                .env(&token_env)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "remote-system token environment variable `{token_env}` is unset or empty"
                    )
                })?;
            let identity =
                flux_system::port::ExecutionIdentity::substrate_identity(system.as_ref());
            eprintln!(
                "remote system · https://{bind} · workspace {} · {}",
                identity.workspace, identity.confinement
            );
            flux_server::system::serve_tls(bind, system, token, cert, key)
                .await
                .context("serve remote execution system")
        }
    }
}
