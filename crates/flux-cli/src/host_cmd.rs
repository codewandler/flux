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

/// The reverse of [`backend_from_config`], for `flux host add` writing a validated part back into
/// the config vocabulary.
pub(super) fn config_kind_from_backend(backend: HostBackend) -> flux_config::HostBackendKind {
    match backend {
        HostBackend::Local => flux_config::HostBackendKind::Local,
        HostBackend::Sandboxed => flux_config::HostBackendKind::Sandboxed,
        HostBackend::Container => flux_config::HostBackendKind::Container,
        HostBackend::Kubernetes => flux_config::HostBackendKind::Kubernetes,
        HostBackend::Remote => flux_config::HostBackendKind::Remote,
    }
}

/// The credential-ref **location** column for a binding — the `Ref` location string or `none`.
/// NEVER a value: `Ref`'s `Display` is a location by construction.
fn host_credential_location(record: &HostRecord) -> String {
    record
        .host
        .credential_ref
        .as_ref()
        .map(|r| r.to_string())
        .unwrap_or_else(|| "none".to_string())
}

/// One binding as a list row — id, backend kind, bare address, static availability, owner and the
/// credential *location*. Shared by `ls` and `show` and tested directly so the redaction
/// guarantee is pinned.
pub(super) fn render_host_row(record: &HostRecord) -> String {
    let host = &record.host;
    let mut row = format!(
        "{id}  [{backend}]  {address}  {availability}  owner={owner}  credential: {cred}",
        id = host.id,
        backend = host.backend,
        address = host.display_address(),
        availability = flux_capabilities::static_availability(host.backend),
        owner = record.owner,
        cred = host_credential_location(record),
    );
    if !host.labels.is_empty() {
        let labels: Vec<String> = host
            .labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        row.push_str(&format!("  {{{}}}", labels.join(", ")));
    }
    row
}

/// The JSON view of one binding — the automation API's stable shape (weak ref only).
fn host_view(record: &HostRecord) -> serde_json::Value {
    let host = &record.host;
    serde_json::json!({
        "id": host.id,
        "backend": host.backend.as_str(),
        "url": host.url,
        "availability": flux_capabilities::static_availability(host.backend),
        "owner": record.owner,
        "credential_ref": host.credential_ref.as_ref().map(ToString::to_string),
        "labels": host.labels,
    })
}

fn unknown_binding_error(id: &str, registry: &flux_capabilities::HostRegistry) -> anyhow::Error {
    let known = registry.known_names();
    if known.is_empty() {
        anyhow::anyhow!("no host binding `{id}` (none declared)")
    } else {
        anyhow::anyhow!(
            "no host binding `{id}`; known bindings: {}",
            known.join(", ")
        )
    }
}

/// The CLI's [`HostProber`]: local identity through the running guarded `System`, remote identity
/// through the protocol handshake (a GET of the identity route — nothing executes). The peer
/// backends report [`HostProbeFailure::BackendUnwired`] until their selection stories wire them.
pub(super) struct CliHostProber {
    pub(super) system: Arc<System>,
}

#[async_trait]
impl flux_capabilities::HostProber for CliHostProber {
    async fn probe(
        &self,
        host: &HostRef,
    ) -> std::result::Result<flux_capabilities::HostProbeReport, flux_capabilities::HostProbeFailure>
    {
        use flux_capabilities::{HostProbeFailure, HostProbeReport};
        match host.backend {
            HostBackend::Local => {
                let identity =
                    flux_system::port::ExecutionIdentity::substrate_identity(self.system.as_ref());
                Ok(HostProbeReport {
                    kind: identity.kind,
                    workspace: identity.workspace,
                    confinement: identity.confinement,
                    remotely_reported: identity.remotely_reported,
                    protocol_version: None,
                })
            }
            HostBackend::Sandboxed | HostBackend::Container | HostBackend::Kubernetes => {
                Err(HostProbeFailure::BackendUnwired {
                    backend: host.backend.as_str().to_string(),
                })
            }
            HostBackend::Remote => {
                let Some(url) = host.url.as_deref() else {
                    // Unreachable through validated construction paths, but fail typed anyway.
                    return Err(HostProbeFailure::Connect {
                        detail: "binding has no url".to_string(),
                    });
                };
                let token = match &host.credential_ref {
                    Some(reference) if reference.scheme == flux_secret::Scheme::Env => self
                        .system
                        .env(&reference.slot)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| HostProbeFailure::CredentialUnavailable {
                            reference: reference.to_string(),
                            detail: format!(
                                "environment variable `{}` is unset or empty",
                                reference.slot
                            ),
                        })?,
                    Some(reference) => {
                        return Err(HostProbeFailure::CredentialUnavailable {
                            reference: reference.to_string(),
                            detail: "only an env-scheme credential resolves for a probe \
                                     (store-scheme resolution needs the discovery broker)"
                                .to_string(),
                        })
                    }
                    None => {
                        return Err(HostProbeFailure::CredentialUnavailable {
                            reference: "none".to_string(),
                            detail: "a remote binding needs a credential reference \
                                     (the serving endpoint refuses an empty token)"
                                .to_string(),
                        })
                    }
                };
                match flux_server::system::probe_remote_system(url, token, &fleet_private_net())
                    .await
                {
                    Ok(handshake) => Ok(HostProbeReport {
                        kind: handshake.substrate_kind,
                        workspace: handshake.workspace,
                        confinement: handshake.confinement,
                        remotely_reported: true,
                        protocol_version: Some(handshake.protocol_version),
                    }),
                    Err(error) => Err(HostProbeFailure::Connect {
                        detail: error.to_string(),
                    }),
                }
            }
        }
    }
}

/// `flux host …` — see [`HostAction`]. Reads resolve through the same session registry the agent
/// surfaces use; `add`/`rm` edit the `[[host]]` table in `~/.flux/config.toml` atomically.
pub(super) async fn run_host(action: HostAction) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = flux_runtime::metadata::load_config(&cwd)?;
    match action {
        HostAction::Ls { output } => {
            let registry = session_host_registry(&cfg);
            let records = registry.list();
            match output {
                AgentOutput::Human => {
                    if records.is_empty() {
                        println!(
                            "no host bindings declared — add a [[host]] entry or run `flux host add`"
                        );
                    }
                    for record in &records {
                        println!("{}", render_host_row(record));
                    }
                }
                AgentOutput::Json => {
                    let views: Vec<_> = records.iter().map(host_view).collect();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema": "flux.hosts/v1",
                            "hosts": views,
                        }))?
                    );
                }
                AgentOutput::Ndjson => {
                    for record in &records {
                        println!("{}", serde_json::to_string(&host_view(record))?);
                    }
                }
            }
        }
        HostAction::Show { id, output } => {
            let registry = session_host_registry(&cfg);
            let record = registry
                .get(&id)
                .ok_or_else(|| unknown_binding_error(&id, &registry))?;
            match output {
                AgentOutput::Human => println!("{}", render_host_row(&record)),
                AgentOutput::Json | AgentOutput::Ndjson => {
                    println!("{}", serde_json::to_string_pretty(&host_view(&record))?)
                }
            }
        }
        HostAction::Add {
            id,
            backend,
            url,
            credential_ref,
            labels,
        } => {
            let backend: HostBackend = backend
                .parse()
                .map_err(|e: String| anyhow::anyhow!("{e}"))?;
            let labels = parse_labels(&labels)?;
            // Validate through the shared invariants first; only a validated part is written.
            let reference = host_ref_from_parts(
                &id,
                backend,
                url.as_deref(),
                credential_ref.as_deref(),
                labels.clone(),
            )?;
            let entry = flux_config::HostEntry {
                id: reference.id.clone(),
                backend: config_kind_from_backend(backend),
                url: reference.url.clone(),
                credential_ref: reference.credential_ref.as_ref().map(ToString::to_string),
                labels,
            };
            flux_runtime::metadata::persist_user_host_in(
                entry,
                &flux_runtime::metadata::DiscoveryEnv::from_process(),
            )?;
            println!(
                "host `{}` declared ({}) in ~/.flux/config.toml",
                reference.id, backend
            );
        }
        HostAction::Rm { id } => {
            let removed = flux_runtime::metadata::remove_user_host_in(
                &id,
                &flux_runtime::metadata::DiscoveryEnv::from_process(),
            )?;
            if removed {
                println!("host `{id}` removed from ~/.flux/config.toml");
            } else if cfg.hosts.iter().any(|h| h.id == id) {
                bail!(
                    "host `{id}` is declared in the project config (.flux/config.toml); \
                     remove it there — `flux host rm` edits only the user layer"
                );
            } else {
                bail!("no host `{id}` declared in ~/.flux/config.toml");
            }
        }
        HostAction::Probe { id, output } => {
            let registry = session_host_registry(&cfg);
            let record = registry
                .get(&id)
                .ok_or_else(|| unknown_binding_error(&id, &registry))?;
            let system =
                Arc::new(System::new(Workspace::new(&cwd)?).with_sandbox(resolved_sandbox()));
            let prober = CliHostProber { system };
            let outcome = flux_capabilities::HostProber::probe(&prober, &record.host).await;
            match output {
                AgentOutput::Human => match outcome {
                    Ok(report) => {
                        println!(
                            "{id}: kind={} workspace={} confinement={} remotely_reported={}",
                            report.kind,
                            report.workspace,
                            report.confinement,
                            report.remotely_reported
                        );
                        if let Some(version) = report.protocol_version {
                            println!("{id}: protocol v{version} (negotiated)");
                        }
                    }
                    Err(failure) => bail!("probe `{id}` failed — {failure}"),
                },
                AgentOutput::Json | AgentOutput::Ndjson => {
                    let doc = match &outcome {
                        Ok(report) => serde_json::json!({
                            "schema": "flux.host-probe/v1",
                            "id": id,
                            "ok": true,
                            "report": report,
                        }),
                        Err(failure) => serde_json::json!({
                            "schema": "flux.host-probe/v1",
                            "id": id,
                            "ok": false,
                            "failure": failure,
                        }),
                    };
                    println!("{}", serde_json::to_string_pretty(&doc)?);
                    if let Err(failure) = outcome {
                        bail!("probe `{id}` failed — {failure}");
                    }
                }
            }
        }
    }
    Ok(())
}
