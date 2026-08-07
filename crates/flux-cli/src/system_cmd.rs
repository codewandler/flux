use std::sync::Arc;

use anyhow::{Context, Result};

use crate::SystemAction;

/// The `grant_source` this daemon records on a private-destination admit.
///
/// It names how the grant *arrived* rather than a local config key, because there is no local one:
/// the private-network scope a served request is admitted under travels in the request frame, from
/// the turn that asked. Claiming `config:web` here would attribute the operator's decision to the
/// wrong machine's configuration.
const WIRE_GRANT_SOURCE: &str = "wire:remote-system-request";

/// The serving daemon's audit sink for `PrivateNetAdmit` (C-674).
///
/// A remote host that admits a request to an internal address has performed the same auditable
/// security event a local run does, and it has to say so *here* as well as report it back — the
/// operator watching this process's output is not the one reading the turn's audit trail, and
/// neither of them should have to ask the other what happened. There is no event store in a
/// `flux system serve` process, so the disclosure goes where its startup banner goes.
struct ServeEgressAudit;

impl flux_plugin::EgressAudit for ServeEgressAudit {
    fn record_private_admit(&self, caller: &str, host: &str, grant_source: &str) {
        eprintln!("private-net admit · {caller} · {host} · {grant_source}");
    }
}

pub(super) async fn run_system(action: SystemAction) -> Result<()> {
    match action {
        SystemAction::Serve {
            bind,
            workspace,
            cert,
            key,
            token_env,
        } => {
            // The composition site joins the two halves (C-675): this surface builds the
            // workspace's one reviewed egress backend and the substrate that will serve it, so a
            // request delegated to this daemon lands on the same guarded client, guard and byte cap
            // an unselected local run uses. Without it the daemon declares no HTTP frame and
            // answers the port's `Unserved` — which is the honest posture, not a degraded one.
            let http: Arc<dyn flux_system::port::GuardedHttp> =
                Arc::new(flux_web::NativeHttp::new(&flux_web::WebOptions {
                    audit: Some(Arc::new(ServeEgressAudit)),
                    grant_source: Some(WIRE_GRANT_SOURCE.to_string()),
                    ..Default::default()
                }));
            let system = Arc::new(
                flux_system::System::from_env(&workspace)
                    .with_context(|| format!("open remote workspace `{}`", workspace.display()))?
                    .with_http(http),
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
