//! Resolving an `ssh` [[host]] binding to the bootstrap its state machine runs on (C-683).
//!
//! This is the seam between a *declaration* — the weak, credential-free `HostRef` an operator wrote
//! — and the bootstrap `flux-server` drives. Everything credential-shaped is resolved here, from a
//! reference, once, and only ever into a path or a token held in memory:
//!
//! - the **private key** is the binding's own `credential_ref`, and what it resolves to is the key's
//!   *path*. Flux never reads the material; openssh opens the file. The check that the file is there
//!   at all is a single byte through the guarded host-file port, which is enough to name the missing
//!   piece and nowhere near enough to be a key.
//! - the **bearer token** is `ssh.token_ref` (or the delivered default). It authenticates the
//!   protocol handshake through the tunnel — the tunnel never substitutes for it — and, when a serve
//!   has to be started, reaches the far side through the ssh channel's environment rather than an
//!   argv.
//!
//! The order of resolution is the order an operator can act on: the declaration must be well-formed
//! before any credential is looked up, so a typo in a far-side path is never reported as a missing
//! secret.

use super::*;

use flux_secret::host::HostRef;
use flux_server::ssh::SshBootstrap;
use flux_system::ssh::{SshPlan, SshTarget, DEFAULT_SERVE_PORT};

/// The token environment variable both seats default to.
const DEFAULT_TOKEN_ENV: &str = "FLUX_REMOTE_SYSTEM_TOKEN";

/// The host the tunnelled endpoint is addressed as when the binding declares no name. It is the
/// address the forward actually lands on, so a far-side certificate with `127.0.0.1` in its SAN
/// needs no further declaration.
const DEFAULT_SERVER_NAME: &str = "127.0.0.1";

/// How long a started far-side serve is given to become admissible.
const DEFAULT_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Enough of the key file to prove it exists and this process may read it. A PEM's first byte is a
/// dash; no amount of key material fits in one byte.
const KEY_PRESENCE_BYTES: usize = 1;

/// A resolved ssh binding: the bootstrap, the token that authenticates the protocol through it, and
/// the private-network grant the tunnelled loopback endpoint needs.
pub(super) struct ResolvedSshBinding {
    pub(super) bootstrap: SshBootstrap,
    pub(super) token: String,
    pub(super) private_net: flux_system::net::PrivateNetAllow,
}

/// Resolve `host` into a bootstrap, or name the piece that is missing.
///
/// The failure type is [`HostProbeFailure`](flux_capabilities::HostProbeFailure) because both
/// callers — the probe and the selection — need the same distinctions, and a selection that
/// re-worded them would be a second vocabulary for the same faces.
pub(super) async fn resolve_ssh_binding(
    host: &HostRef,
    local: &System,
) -> std::result::Result<ResolvedSshBinding, flux_capabilities::HostProbeFailure> {
    use flux_capabilities::HostProbeFailure;

    let declaration = |detail: String| HostProbeFailure::Connect { detail };

    // 1. The declaration. A binding that does not describe a reachable far side is refused before
    //    any credential is resolved, so a typo never reads as a missing secret.
    let Some(url) = host.url.as_deref() else {
        return Err(declaration(
            "an ssh binding needs a `url` of the form `ssh://user@host[:port]`".to_string(),
        ));
    };
    let target = SshTarget::parse(url).map_err(|refusal| declaration(refusal.to_string()))?;
    let ssh = host.ssh.clone().unwrap_or_default();

    // 2. The key, by reference. What resolves is a path; the material stays in the file.
    let key_path = match &host.credential_ref {
        Some(reference) if reference.scheme == flux_secret::Scheme::Env => local
            .env(&reference.slot)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| HostProbeFailure::CredentialUnavailable {
                reference: reference.to_string(),
                detail: format!(
                    "environment variable `{}` is unset or empty; it must hold the *path* of the \
                     private key openssh should offer",
                    reference.slot
                ),
            })?,
        Some(reference) => {
            return Err(HostProbeFailure::CredentialUnavailable {
                reference: reference.to_string(),
                detail: "only an env-scheme credential resolves for an ssh binding today, and it \
                         resolves to the private key's path (store-scheme resolution needs the \
                         discovery broker)"
                    .to_string(),
            })
        }
        None => {
            return Err(HostProbeFailure::CredentialUnavailable {
                reference: "none".to_string(),
                detail: "an ssh binding needs a credential reference locating its private key; \
                         every interactive authentication face is off by construction, so there is \
                         nothing to fall back to"
                    .to_string(),
            })
        }
    };

    let plan = SshPlan {
        target,
        key_path: key_path.clone(),
        binary: ssh.binary.clone().unwrap_or_else(|| "flux".to_string()),
        serve_port: ssh.serve_port.unwrap_or(DEFAULT_SERVE_PORT),
        workspace: ssh.workspace.clone(),
        cert: ssh.cert.clone(),
        key: ssh.key.clone(),
        known_hosts: ssh.known_hosts.clone(),
        token_env: None,
        start_timeout: DEFAULT_START_TIMEOUT,
    };
    // Refuse a far-side word the login shell would re-interpret before anything is spawned, read or
    // connected — this is the only place the argv-only guarantee can be kept for the far side.
    plan.validate()
        .map_err(|refusal| declaration(refusal.to_string()))?;

    // 3. The key file itself. One byte through the guarded host-file port: enough to say "that key
    //    is not there" in flux's own words instead of leaving it to ssh's, and never enough to be
    //    key material.
    if let Err(error) = flux_system::port::GuardedHostFiles::read_file_scoped(
        local,
        &key_path,
        &key_path,
        KEY_PRESENCE_BYTES,
    )
    .await
    {
        return Err(HostProbeFailure::CredentialUnavailable {
            reference: host
                .credential_ref
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "none".to_string()),
            detail: format!("private key `{key_path}` is not readable here: {error}"),
        });
    }

    // 4. The serving endpoint's bearer token — the protocol's own auth, which the tunnel never
    //    replaces.
    let token_ref = ssh
        .token_ref
        .clone()
        .unwrap_or_else(|| flux_secret::Ref::env(DEFAULT_TOKEN_ENV));
    if token_ref.scheme != flux_secret::Scheme::Env {
        return Err(HostProbeFailure::CredentialUnavailable {
            reference: token_ref.to_string(),
            detail:
                "only an env-scheme credential resolves for an ssh binding's bearer token today"
                    .to_string(),
        });
    }
    let token = local
        .env(&token_ref.slot)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| HostProbeFailure::CredentialUnavailable {
            reference: token_ref.to_string(),
            detail: format!(
                "environment variable `{}` is unset or empty; the served endpoint refuses an empty \
                 token, and the tunnel does not authenticate on its behalf",
                token_ref.slot
            ),
        })?;

    // C-684: one binding, one trust anchor. `[host.ssh] ca` is the ssh-local spelling and stays
    // authoritative for an ssh binding — it is resolved during the bootstrap that *makes* the
    // endpoint — but a binding that declares the ordinary `ca_cert` is honoured rather than
    // silently ignored. Both spellings resolve through `host_cmd::read_ca_pem`, so they agree on
    // precedence *and* on which paths are nameable; the field means the same thing on every kind
    // that dials TLS. Declaring both to different paths is refused here, since one of them would
    // otherwise have to lose.
    if let (Some(local_ca), Some(binding_ca)) = (ssh.ca.as_deref(), host.ca_cert.as_deref()) {
        if local_ca != binding_ca {
            return Err(declaration(format!(
                "this binding declares two different trust anchors — `ca_cert = \"{binding_ca}\"` \
                 and [host.ssh] `ca = \"{local_ca}\"`. One of them would have to be ignored, so \
                 neither is used; keep a single declaration"
            )));
        }
    }
    let declared_ca = ssh.ca.as_deref().or(host.ca_cert.as_deref());
    let ca_pem = match declared_ca {
        Some(path) => Some(host_cmd::read_ca_pem(local, path).await.map_err(|error| {
            declaration(format!("CA certificate `{path}` is unreadable: {error}"))
        })?),
        None => None,
    };

    let server_name = ssh
        .server_name
        .clone()
        .unwrap_or_else(|| DEFAULT_SERVER_NAME.to_string());
    // The forward lands on loopback, which the egress guard refuses by default. The grant is the
    // one name this binding addresses, not a blanket private-network opening.
    let private_net = flux_system::net::PrivateNetAllow::from_hosts(vec![server_name.clone()]);

    Ok(ResolvedSshBinding {
        bootstrap: SshBootstrap {
            plan: SshPlan {
                token_env: Some(token_ref.slot.clone()),
                ..plan
            },
            server_name,
            ca_pem,
        },
        token,
        private_net,
    })
}

// The CA read's cap and its port live with the shared `ca_cert` resolution in `host_cmd`, so an
// ssh binding and a remote one cannot end up with different reachability for the same field.
