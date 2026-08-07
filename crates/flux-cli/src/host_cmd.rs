use super::*;

use flux_secret::host::{HostBackend, HostRecord, HostRef};

/// The ambient signal a session emits when host bindings are declared (mirrors the `endpoint`
/// store signal); the `host` tool group surfaces on it.
pub(super) const HOST_SIGNAL: &str = "host";

/// The `flux-config` backend kind and the `flux-secret` one are the same closed vocabulary held in
/// two crates that may not depend on each other (flux-config stays a `flux-secret`-free leaf), so
/// the surface crate owns the conversion.
pub(super) fn backend_from_config(kind: flux_config::HostBackendKind) -> HostBackend {
    match kind {
        flux_config::HostBackendKind::Local => HostBackend::Local,
        flux_config::HostBackendKind::Sandboxed => HostBackend::Sandboxed,
        flux_config::HostBackendKind::Container => HostBackend::Container,
        flux_config::HostBackendKind::Kubernetes => HostBackend::Kubernetes,
        flux_config::HostBackendKind::Remote => HostBackend::Remote,
    }
}

/// Build a weak, named [`HostRef`] from operator-supplied parts, enforcing the C-648 invariants
/// shared by `flux host add` and `[[host]]`: an address exactly where the backend takes one, a
/// credential-free URL, and a parseable credential *location* (never a value).
pub(super) fn host_ref_from_parts(
    id: &str,
    backend: HostBackend,
    url: Option<&str>,
    credential_ref: Option<&str>,
    labels: std::collections::BTreeMap<String, String>,
) -> Result<HostRef> {
    if id.trim().is_empty() {
        bail!("host id must not be empty");
    }
    match (backend, url) {
        (HostBackend::Remote, None) => {
            bail!("remote host `{id}` needs a `url` (the `flux system serve` endpoint it binds)")
        }
        (HostBackend::Local | HostBackend::Sandboxed, Some(_)) => {
            bail!("host `{id}` binds the {backend} substrate, which has no address; drop `url`")
        }
        _ => {}
    }
    if let Some(url) = url {
        if url.trim().is_empty() {
            bail!("host url must not be empty when given");
        }
        if url_has_userinfo(url) {
            bail!(
                "url must not embed credentials (`user:pass@…`); pass the bare host and put the \
                 credential location in `credential_ref` (e.g. `env/FLUX_REMOTE_SYSTEM_TOKEN`)"
            );
        }
    }
    let credential_ref = match credential_ref {
        Some(s) => Some(
            flux_secret::Ref::parse(s)
                .map_err(|e| anyhow::anyhow!("invalid credential ref `{s}`: {e}"))?,
        ),
        None => None,
    };
    Ok(HostRef {
        url: url.map(str::to_string),
        credential_ref,
        labels,
        ..HostRef::declared(id, backend)
    })
}

/// Merge operator-declared `[[host]]` bindings into `registry` as config-declared records so they
/// list, resolve and (C-650) select like any registered binding. An invalid entry is
/// warned-and-skipped so one typo can't sink the rest — the *backend kind* is already typed at the
/// config layer, so an unknown kind never gets this far (it is a hard config error).
pub(super) fn merge_config_hosts(
    registry: &flux_capabilities::HostRegistry,
    cfg: &flux_config::Config,
) {
    for host in &cfg.hosts {
        match host_ref_from_parts(
            &host.id,
            backend_from_config(host.backend),
            host.url.as_deref(),
            host.credential_ref.as_deref(),
            host.labels.clone(),
        ) {
            Ok(reference) => registry.put(HostRecord::config(reference)),
            Err(e) => eprintln!(
                "{}",
                style::dim(&format!("(ignoring invalid [[host]] `{}`: {e})", host.id))
            ),
        }
    }
}

/// The session's host registry: the persisted store (if any) loaded first, then `[[host]]` config
/// declarations merged over it by id — config wins, exactly like the endpoint registry. Built once
/// at session start; every surface (assembly, selection, the `flux host` family) goes through this
/// so they cannot drift.
pub(super) fn session_host_registry(
    cfg: &flux_config::Config,
) -> Arc<flux_capabilities::HostRegistry> {
    let registry = match flux_capabilities::HostRegistry::default_path() {
        Some(path) => flux_capabilities::HostRegistry::with_path(path),
        None => flux_capabilities::HostRegistry::new(),
    };
    if let Err(error) = registry.load() {
        eprintln!(
            "{}",
            style::dim(&format!("(hosts store not loaded: {error})"))
        );
    }
    merge_config_hosts(&registry, cfg);
    Arc::new(registry)
}
