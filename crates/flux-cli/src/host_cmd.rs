use super::*;

use flux_secret::host::{HostBackend, HostGrant, HostRecord, HostRef};

/// The reserved prefix for session-constructed binding names (the `--remote` sugar records as
/// `@session/remote`); a declared binding may not claim it.
pub(super) const SESSION_HOST_PREFIX: &str = "@";

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
        flux_config::HostBackendKind::Microvm => HostBackend::Microvm,
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
    grant: &[String],
    labels: std::collections::BTreeMap<String, String>,
) -> Result<HostRef> {
    if id.trim().is_empty() {
        bail!("host id must not be empty");
    }
    if id.starts_with(SESSION_HOST_PREFIX) {
        bail!(
            "`{id}` uses the reserved `{SESSION_HOST_PREFIX}` prefix (that is for \
             session-constructed bindings); pick a bare name like `build-farm`"
        );
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
    let grant = grant
        .iter()
        .map(|class| {
            class
                .parse::<HostGrant>()
                .map_err(|e| anyhow::anyhow!("{e}"))
        })
        .collect::<Result<Vec<HostGrant>>>()?;
    Ok(HostRef {
        url: url.map(str::to_string),
        credential_ref,
        grant,
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
            &host.grant,
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
        HostBackend::Microvm => flux_config::HostBackendKind::Microvm,
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
        availability = flux_capabilities::binding_availability(host),
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
        "availability": flux_capabilities::binding_availability(host),
        "owner": record.owner,
        "credential_ref": host.credential_ref.as_ref().map(ToString::to_string),
        "grant": host.grant.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "labels": host.labels,
    })
}

/// The outcome of an explicit substrate selection (C-650): the binding name every dispatch
/// record's provenance will carry, and the execution system to install where the backend is not
/// the native default (a named `local` binding records its name but keeps the native
/// workspace-following path).
pub(super) struct SelectedSubstrate {
    pub(super) binding: String,
    pub(super) system: Option<Arc<dyn flux_system::port::ExecutionSystem>>,
}

impl std::fmt::Debug for SelectedSubstrate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectedSubstrate")
            .field("binding", &self.binding)
            .field("system_override", &self.system.is_some())
            .finish()
    }
}

/// Tightest-wins **backend** selection (C-651): a posture floor that will not run unconfined
/// raises a `local` binding to the `sandboxed` peer, exactly as `SandboxFloor::raise_mode` raises
/// the spawn-time sandbox mode — a floor may tighten a selection, never loosen one.
///
/// Only `local` is raised, and only from a `Require` floor. A `remote`, `container`, `kubernetes`
/// or `microvm` binding carries its own physical boundary, so silently re-pointing it at this
/// machine's sandbox would answer a different question than the operator asked; and an `On` floor
/// tolerates an unconfined run by construction, so forcing a fail-closed backend under it would
/// turn a tolerated degradation into a startup refusal.
///
/// This applies only where a binding is being selected. It does not reach into the *unselected*
/// case: `flux --yes …` with no `--host` still installs no system override, because that override
/// is a snapshot and the native path has to keep following worktree transitions. The consequence
/// of a raise is therefore worth stating plainly — `--host <local binding>` under a `Require`
/// posture pins the workspace the same way selecting a `remote` binding does, which is what
/// selecting a substrate has always meant.
pub(super) fn backend_under_floor(
    backend: HostBackend,
    floor: flux_runtime::SandboxFloor,
) -> HostBackend {
    match backend {
        HostBackend::Local if floor.mode == flux_system::sandbox::SandboxMode::Require => {
            HostBackend::Sandboxed
        }
        other => other,
    }
}

/// What a remote-shaped binding (`remote`, `microvm`) that names no address must tell an operator.
///
/// The two absences are not the same thing, so they do not get the same words. A `remote` binding
/// cannot be constructed without a url, so reaching this at all means the operator-editable store
/// was hand-edited. A `microvm` binding legitimately exists before its guest does — flux never
/// provisions a VM (C-677 / C-480's boundary), so the endpoint comes to exist when a deployment
/// serves one, and the refusal has to name that gap instead of implying a retry would close it.
fn missing_endpoint_error(name: &str, backend: HostBackend) -> String {
    match backend {
        HostBackend::Microvm => format!(
            "host `{name}` binds the `microvm` backend but names no served guest endpoint; flux \
             never provisions a VM — deploy the guest's `flux system serve` (see the VM/microVM \
             deployment profile) and declare its `url`. Selection fails closed until then"
        ),
        other => format!("host `{name}` has no url; a {other} binding needs one"),
    }
}

/// The same absence as a typed probe failure.
///
/// A urlless `microvm` binding is [`BackendUnavailable`](flux_capabilities::HostProbeFailure), not
/// [`BackendUnwired`](flux_capabilities::HostProbeFailure): the backend *is* wired — flux knows
/// exactly how to reach a guest — and what is missing is the guest, which is the distinction an
/// operator acts on.
fn missing_endpoint_failure(backend: HostBackend) -> flux_capabilities::HostProbeFailure {
    match backend {
        HostBackend::Microvm => flux_capabilities::HostProbeFailure::BackendUnavailable {
            backend: HostBackend::Microvm.as_str().to_string(),
            detail: "the binding names no served guest endpoint; flux never provisions a VM — \
                     deploy the guest's `flux system serve` and declare its `url`"
                .to_string(),
        },
        // Unreachable through validated construction, but the store is operator-editable.
        _ => flux_capabilities::HostProbeFailure::Connect {
            detail: "binding has no url".to_string(),
        },
    }
}

/// Resolve `--host <name>`: a registered binding, granted to `surface`, selects the execution
/// substrate. Every refusal is a startup refusal — an unknown name lists the known bindings, an
/// ungranted binding names the missing class (an unattended surface never inherits `operator`,
/// so a serving surface cannot widen a grant silently), and an unwired backend fails closed.
///
/// `floor` is the confinement floor the resolved autonomy posture carries (C-651); see
/// [`backend_under_floor`] for what it may and may not do to the declared backend kind.
pub(super) async fn resolve_named_host(
    name: &str,
    hosts: &flux_capabilities::HostRegistry,
    surface: HostGrant,
    local: &System,
    floor: flux_runtime::SandboxFloor,
) -> Result<SelectedSubstrate> {
    let Some(record) = hosts.get(name) else {
        return Err(unknown_binding_error(name, hosts));
    };
    let host = record.host;
    if !host.grant.contains(&surface) {
        let granted = if host.grant.is_empty() {
            "none".to_string()
        } else {
            host.grant
                .iter()
                .map(|g| g.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        bail!(
            "host `{name}` is not granted to this surface (surface class `{surface}`, granted: \
             {granted}); the default is deny — add `grant = [\"{surface}\"]` to its [[host]] \
             entry deliberately, since widening a grant is an escalation"
        );
    }
    match backend_under_floor(host.backend, floor) {
        HostBackend::Local => Ok(SelectedSubstrate {
            binding: host.id,
            system: None,
        }),
        // C-651: confinement is a peer substrate. It composes the running native system rather
        // than replacing it, so the workspace and its guards are unchanged; what the selection
        // adds is a posture that must hold before the binding resolves at all.
        HostBackend::Sandboxed => {
            let peer = flux_system::sandboxed::SandboxedSystem::from_env(local.clone())
                .map_err(|error| anyhow::anyhow!("host `{name}`: {error}"))?;
            Ok(SelectedSubstrate {
                binding: host.id,
                system: Some(Arc::new(peer)),
            })
        }
        backend @ (HostBackend::Container | HostBackend::Kubernetes) => bail!(
            "host `{name}` binds the `{backend}` backend, which has no selectable implementation \
             wired yet; selection fails closed"
        ),
        // C-677: `microvm` shares this arm because Decision 0018 rule 3 composes rather than
        // invents — a microVM host is the delivered remote protocol served from inside a guest, so
        // the same authenticated client, the same handshake admission and the same credential
        // *reference* scheme apply. Sharing the arm is the guarantee: there is no second
        // implementation that could drift, and no wire code belongs to the VM at all.
        backend @ (HostBackend::Remote | HostBackend::Microvm) => {
            let Some(url) = host.url.as_deref() else {
                bail!("{}", missing_endpoint_error(name, backend));
            };
            let token = match &host.credential_ref {
                Some(reference) if reference.scheme == flux_secret::Scheme::Env => local
                    .env(&reference.slot)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "host `{name}`: credential `{reference}` unavailable — environment \
                             variable `{}` is unset or empty",
                            reference.slot
                        )
                    })?,
                Some(reference) => bail!(
                    "host `{name}`: only an env-scheme credential resolves for substrate \
                     selection today (got `{reference}`)"
                ),
                None => bail!(
                    "host `{name}` names no credential reference; the serving endpoint refuses \
                     an empty token"
                ),
            };
            let system =
                flux_server::system::connect_remote_system(url, token, &fleet_private_net())
                    .await
                    .with_context(|| format!("connect host `{name}`"))?;
            Ok(SelectedSubstrate {
                binding: host.id,
                system: Some(Arc::new(system)),
            })
        }
    }
}

/// The named Exchange catalogue binding (C-650): `[exchange] host = "<name>"` resolves through
/// the session registry to `(binding name, origin url, token reference)`. Only an env-scheme
/// reference resolves on this transitional path — it replaces exactly the environment pair — and
/// every misdeclaration is a dim diagnostic plus a disabled catalogue, never a hard startup
/// failure (matching the pair's own partial-configuration behavior).
pub(super) fn exchange_binding_from_config(
    cfg: &flux_config::Config,
    hosts: &flux_capabilities::HostRegistry,
) -> Option<(String, String, flux_secret::Ref)> {
    let name = cfg.exchange.host.as_deref()?;
    let dim_skip = |detail: &str| {
        eprintln!(
            "{}",
            style::dim(&format!(
                "(exchange host `{name}`: {detail}; catalogue disabled)"
            ))
        );
    };
    let Some(record) = hosts.get(name) else {
        dim_skip("no such [[host]] binding");
        return None;
    };
    let host = record.host;
    let Some(url) = host.url else {
        dim_skip("the binding has no url");
        return None;
    };
    let Some(reference) = host.credential_ref else {
        dim_skip("the binding names no credential reference");
        return None;
    };
    if reference.scheme != flux_secret::Scheme::Env {
        dim_skip(&format!(
            "only an env-scheme credential resolves here (got `{reference}`)"
        ));
        return None;
    }
    Some((host.id, url, reference))
}

/// Record the anonymous `--remote <url>` selection as the session's ephemeral binding
/// (`@session/remote`) so it lists and probes like any named one. Session-owned: no production
/// path ever persists it, and it carries no grant — it *is* the selection, not a reusable one.
pub(super) fn record_ephemeral_remote(
    hosts: &flux_capabilities::HostRegistry,
    url: &str,
    token_env: &str,
) {
    hosts.put(HostRecord::session(HostRef {
        url: Some(url.to_string()),
        credential_ref: Some(flux_secret::Ref::env(token_env)),
        ..HostRef::ephemeral("@session/remote", HostBackend::Remote)
    }));
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
/// through the protocol handshake (a GET of the identity route — nothing executes), and (C-651)
/// `sandboxed` identity by resolving the confinement peer, which reports whether this platform can
/// confine at all. A `microvm` binding (C-677) takes the remote path: its guest serves the same
/// protocol, so the same handshake is what admits it. The remaining peer backends report
/// [`HostProbeFailure::BackendUnwired`] until their selection stories wire them.
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
            // Resolving the peer is the identity check: it discovers the platform's confinement
            // backend (a cached preflight, no workspace effect) and either reports the posture it
            // would run under or names why this machine cannot serve the binding.
            HostBackend::Sandboxed => {
                match flux_system::sandboxed::SandboxedSystem::from_env((*self.system).clone()) {
                    Ok(peer) => {
                        let identity =
                            flux_system::port::ExecutionIdentity::substrate_identity(&peer);
                        Ok(HostProbeReport {
                            kind: identity.kind,
                            workspace: identity.workspace,
                            confinement: identity.confinement,
                            remotely_reported: identity.remotely_reported,
                            protocol_version: None,
                        })
                    }
                    Err(error) => Err(HostProbeFailure::BackendUnavailable {
                        backend: host.backend.as_str().to_string(),
                        detail: error.to_string(),
                    }),
                }
            }
            HostBackend::Container | HostBackend::Kubernetes => {
                Err(HostProbeFailure::BackendUnwired {
                    backend: host.backend.as_str().to_string(),
                })
            }
            // C-677: one arm, because a microvm host *is* the remote protocol served from inside a
            // guest. The handshake is what admits either of them, and the report is the far side's
            // own `SubstrateIdentity` passed through — the guest's truth, stamped
            // `remotely_reported` here because nothing on this machine observed it.
            HostBackend::Remote | HostBackend::Microvm => {
                let Some(url) = host.url.as_deref() else {
                    return Err(missing_endpoint_failure(host.backend));
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

    /// C-654: resolve the binding to a live substrate and ask **it** to measure itself.
    ///
    /// Read-only, and gated exactly as `probe` is rather than as a *selection* is: measuring a
    /// binding does not install it, run anything on it, or change where this turn's effects land,
    /// so it does not ask for the `HostGrant` that selecting one requires. What it does share with
    /// `probe` is the credential path — a remote binding is reached with its declared credential —
    /// which is why both live behind the same operator surface.
    async fn read_metrics(
        &self,
        host: &HostRef,
    ) -> std::result::Result<flux_capabilities::HostMetrics, flux_capabilities::HostProbeFailure>
    {
        use flux_capabilities::{HostMetrics, HostProbeFailure};
        use flux_system::port::GuardedMetrics;

        let substrate: Arc<dyn flux_system::port::ExecutionSystem> = match host.backend {
            HostBackend::Local => self.system.clone(),
            HostBackend::Sandboxed => {
                match flux_system::sandboxed::SandboxedSystem::from_env((*self.system).clone()) {
                    Ok(peer) => Arc::new(peer),
                    Err(error) => {
                        return Err(HostProbeFailure::BackendUnavailable {
                            backend: host.backend.as_str().to_string(),
                            detail: error.to_string(),
                        })
                    }
                }
            }
            HostBackend::Container | HostBackend::Kubernetes => {
                return Err(HostProbeFailure::BackendUnwired {
                    backend: host.backend.as_str().to_string(),
                })
            }
            HostBackend::Remote | HostBackend::Microvm => {
                Arc::new(self.connect_remote(host).await?)
            }
        };

        match GuardedMetrics::read_metrics(&*substrate).await {
            Ok(answers) => Ok(HostMetrics::Served {
                remotely_reported: flux_system::port::ExecutionIdentity::substrate_identity(
                    &*substrate,
                )
                .remotely_reported,
                answers,
            }),
            // The port's two negatives stay apart here, where they are last distinguishable: an
            // `Unserved` means this substrate measures nothing at all, which is not a transport
            // failure and not an empty snapshot. Anything else really did fail to reach it.
            Err(error) => match flux_system::remote::failure_mode(&error) {
                Some(flux_system::remote::FailureMode::Unserved) => Ok(HostMetrics::Unserved {
                    detail: error.to_string(),
                }),
                _ => Err(HostProbeFailure::Connect {
                    detail: error.to_string(),
                }),
            },
        }
    }
}

impl CliHostProber {
    /// A remote-shaped binding's connected substrate (`remote` or `microvm`), with the credential
    /// resolved from its *location*. Shares `probe`'s credential rules, including that only an
    /// env-scheme reference resolves on this path today.
    async fn connect_remote(
        &self,
        host: &HostRef,
    ) -> std::result::Result<flux_system::remote::RemoteSystem, flux_capabilities::HostProbeFailure>
    {
        use flux_capabilities::HostProbeFailure;

        let Some(url) = host.url.as_deref() else {
            return Err(missing_endpoint_failure(host.backend));
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
                    detail: "only an env-scheme credential resolves here (store-scheme resolution \
                             needs the discovery broker)"
                        .to_string(),
                })
            }
            None => {
                return Err(HostProbeFailure::CredentialUnavailable {
                    reference: "none".to_string(),
                    detail: "a remote binding needs a credential reference (the serving endpoint \
                             refuses an empty token)"
                        .to_string(),
                })
            }
        };
        flux_server::system::connect_remote_system(url, token, &fleet_private_net())
            .await
            .map_err(|error| HostProbeFailure::Connect {
                detail: error.to_string(),
            })
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
            grant,
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
                &grant,
                labels.clone(),
            )?;
            let entry = flux_config::HostEntry {
                id: reference.id.clone(),
                backend: config_kind_from_backend(backend),
                url: reference.url.clone(),
                credential_ref: reference.credential_ref.as_ref().map(ToString::to_string),
                grant: reference.grant.iter().map(ToString::to_string).collect(),
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
        HostAction::Metrics { id, output } => {
            let registry = session_host_registry(&cfg);
            let record = registry
                .get(&id)
                .ok_or_else(|| unknown_binding_error(&id, &registry))?;
            let system =
                Arc::new(System::new(Workspace::new(&cwd)?).with_sandbox(resolved_sandbox()));
            let prober = CliHostProber { system };
            let outcome = flux_capabilities::HostProber::read_metrics(&prober, &record.host).await;
            render_host_metrics(&id, outcome, output)?;
        }
    }
    Ok(())
}

/// `flux host metrics` output, both faces (C-654).
///
/// The two negatives the port keeps apart stay apart here, because this is the surface an operator
/// reads: a substrate that serves no metrics says so once, and an instrument this machine does not
/// have is a per-kind `unavailable` entry with a reason. Neither is ever rendered as a number.
fn render_host_metrics(
    id: &str,
    outcome: std::result::Result<
        flux_capabilities::HostMetrics,
        flux_capabilities::HostProbeFailure,
    >,
    output: AgentOutput,
) -> Result<()> {
    use flux_capabilities::HostMetrics;

    match output {
        AgentOutput::Human => match outcome {
            Ok(HostMetrics::Served {
                remotely_reported,
                answers,
            }) => {
                println!(
                    "{id}: {}",
                    if remotely_reported {
                        "remotely reported by the serving substrate"
                    } else {
                        "observed locally"
                    }
                );
                for answer in &answers {
                    println!("  {}", flux_capabilities::render_metric_answer(answer));
                }
            }
            Ok(HostMetrics::Unserved { detail }) => {
                println!("{id}: this substrate does not serve host metrics — {detail}");
            }
            Err(failure) => bail!("metrics `{id}` failed — {failure}"),
        },
        // JSON, not the human prose above, is the automation API: every value stays a raw number
        // in its declared unit, and `status` is what a consumer branches on.
        AgentOutput::Json | AgentOutput::Ndjson => {
            let doc = match &outcome {
                Ok(HostMetrics::Served {
                    remotely_reported,
                    answers,
                }) => serde_json::json!({
                    "schema": "flux.host-metrics/v1",
                    "id": id,
                    "ok": true,
                    "served": true,
                    "remotely_reported": remotely_reported,
                    "metrics": answers
                        .iter()
                        .map(flux_capabilities::metric_answer_json)
                        .collect::<Vec<_>>(),
                }),
                Ok(HostMetrics::Unserved { detail }) => serde_json::json!({
                    "schema": "flux.host-metrics/v1",
                    "id": id,
                    "ok": true,
                    // Served-nothing is a *reported* state, not a failure: the binding answered.
                    "served": false,
                    "detail": detail,
                }),
                Err(failure) => serde_json::json!({
                    "schema": "flux.host-metrics/v1",
                    "id": id,
                    "ok": false,
                    "failure": failure,
                }),
            };
            println!("{}", serde_json::to_string_pretty(&doc)?);
            if let Err(failure) = outcome {
                bail!("metrics `{id}` failed — {failure}");
            }
        }
    }
    Ok(())
}
