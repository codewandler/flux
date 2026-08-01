use super::*;

/// The credential-ref **location** column for a record — the `Ref` location string (e.g.
/// `kubernetes/ns/secret/key`) or `none`. NEVER a value: `Ref`'s `Display` is a location by
/// construction (`flux-secret`), and the persisted record carries no material in the first place.
pub(super) fn credential_location(record: &flux_secret::endpoint::EndpointRecord) -> String {
    record
        .endpoint
        .credential_ref
        .as_ref()
        .map(|r| r.to_string())
        .unwrap_or_else(|| "none".to_string())
}

/// One persisted record as a list row — bare URL (no creds), owner, ttl/health, and the credential
/// *location*. Shared by the `list` renderer and tested directly so the redaction guarantee is pinned.
pub(super) fn render_endpoint_row(record: &flux_secret::endpoint::EndpointRecord) -> String {
    let ep = &record.endpoint;
    let product = if ep.product.is_empty() {
        "-"
    } else {
        ep.product.as_str()
    };
    let mut ttl_health = String::new();
    if let Some(ttl) = record.ttl_secs {
        ttl_health.push_str(&format!("ttl={ttl}s"));
    }
    if let Some(h) = &record.health {
        if !ttl_health.is_empty() {
            ttl_health.push(' ');
        }
        ttl_health.push_str(&format!("health={h}"));
    }
    if ttl_health.is_empty() {
        ttl_health.push('-');
    }
    format!(
        "{id}  [{product}]  {url}  owner={owner}  {ttl_health}  credential: {cred}",
        id = ep.id,
        url = ep.url,
        owner = record.owner,
        cred = credential_location(record),
    )
}

/// `flux endpoint …` — the operator mirror of the agent's `endpoint.*` ops over the persisted
/// `~/.flux/endpoints.toml` store. Every path is reference-only: it shows the credential *location*,
/// never a value. Synchronous (pure file IO over the store).
/// Parse repeatable `key=value` label args into a map (rejects a missing `=` or an empty key).
pub(super) fn parse_labels(pairs: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut out = std::collections::BTreeMap::new();
    for kv in pairs {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("label `{kv}` must be `key=value`"))?;
        if k.trim().is_empty() {
            bail!("label key in `{kv}` must not be empty");
        }
        out.insert(k.trim().to_string(), v.to_string());
    }
    Ok(out)
}

/// True if a URL embeds credentials in its authority (`scheme://user[:pass]@host…`). The credential
/// belongs in a `--credential-ref` *location*, never in the URL.
pub(super) fn url_has_userinfo(url: &str) -> bool {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    authority.contains('@')
}

/// Build a weak, config-bound [`EndpointRef`](flux_secret::endpoint::EndpointRef) from
/// operator-supplied parts, enforcing the D-116 invariants shared by `flux endpoint add` and
/// `[[endpoint.static]]`: a named (non-`@endpoint/`) id, a credential-free URL, and a parseable
/// credential *location* (never a value).
pub(super) fn endpoint_ref_from_parts(
    id: &str,
    url: &str,
    product: Option<&str>,
    protocol: Option<&str>,
    credential_ref: Option<&str>,
    labels: std::collections::BTreeMap<String, String>,
) -> Result<flux_secret::endpoint::EndpointRef> {
    use flux_secret::endpoint::{EndpointRef, ENDPOINT_REF_PREFIX};
    if id.trim().is_empty() {
        bail!("endpoint id must not be empty");
    }
    if id.starts_with(ENDPOINT_REF_PREFIX) {
        bail!(
            "`{id}` uses the reserved `{ENDPOINT_REF_PREFIX}` prefix (that is for discovered \
             endpoints); pick a bare name like `pg-prod`"
        );
    }
    if url.trim().is_empty() {
        bail!("endpoint url must not be empty");
    }
    if url_has_userinfo(url) {
        bail!(
            "url must not embed credentials (`user:pass@…`); pass the bare host and put the \
             credential location in `--credential-ref` (e.g. `env/PGPASSWORD`)"
        );
    }
    let credential_ref = match credential_ref {
        Some(s) => Some(
            flux_secret::Ref::parse(s)
                .map_err(|e| anyhow::anyhow!("invalid credential ref `{s}`: {e}"))?,
        ),
        None => None,
    };
    Ok(EndpointRef {
        product: product.unwrap_or_default().to_string(),
        protocol: protocol.map(str::to_string),
        credential_ref,
        labels,
        ..EndpointRef::named(id, url)
    })
}

/// Merge operator-declared `[[endpoint.static]]` bindings (D-116) into `registry` as config-bound
/// records so they surface, list, and resolve like a `flux endpoint add` record. An invalid entry is
/// warned-and-skipped so one typo can't sink the rest.
pub(super) fn merge_static_endpoints(
    registry: &flux_capabilities::EndpointRegistry,
    cfg: &flux_config::Config,
) {
    for ep in &cfg.endpoint.static_endpoints {
        let product = Some(ep.product.as_str()).filter(|s| !s.is_empty());
        match endpoint_ref_from_parts(
            &ep.id,
            &ep.url,
            product,
            ep.protocol.as_deref(),
            ep.credential_ref.as_deref(),
            ep.labels.clone(),
        ) {
            Ok(reference) => registry.put(flux_secret::endpoint::EndpointRecord::config(reference)),
            Err(e) => eprintln!(
                "{}",
                style::dim(&format!(
                    "(ignoring invalid [[endpoint.static]] `{}`: {e})",
                    ep.id
                ))
            ),
        }
    }
}

pub(super) fn run_endpoint(action: EndpointAction) -> Result<()> {
    // The persisted store. A standalone CLI invocation has no in-memory session registry, so every
    // subcommand operates on `~/.flux/endpoints.toml` (loaded fresh; a missing file is empty).
    let path = flux_capabilities::EndpointRegistry::default_path()
        .ok_or_else(|| anyhow::anyhow!("HOME is not set (no endpoints store path)"))?;
    run_endpoint_in(&path, action)
}

/// The path-parameterized body of [`run_endpoint`] (tests pass a temp store so they don't touch
/// `HOME`), mirroring [`run_plugin_in`].
pub(super) fn run_endpoint_in(path: &std::path::Path, action: EndpointAction) -> Result<()> {
    use flux_capabilities::EndpointRegistry;

    let registry = EndpointRegistry::with_path(path.to_path_buf());
    registry
        .load()
        .map_err(|e| anyhow::anyhow!("load endpoints store: {e}"))?;

    match action {
        EndpointAction::Add {
            id,
            url,
            product,
            protocol,
            credential_ref,
            labels,
        } => {
            // Wire a weak, credential-free config-bound ref (D-116). The shared validator rejects a
            // credential-bearing URL / an `@endpoint/` id / an unparseable credential ref — the same
            // rules a `[[endpoint.static]]` block is held to.
            let reference = endpoint_ref_from_parts(
                &id,
                &url,
                product.as_deref(),
                protocol.as_deref(),
                credential_ref.as_deref(),
                parse_labels(&labels)?,
            )?;
            registry.put(flux_secret::endpoint::EndpointRecord::config(
                reference.clone(),
            ));
            registry
                .save()
                .map_err(|e| anyhow::anyhow!("persist endpoint `{id}`: {e}"))?;
            println!(
                "added {} → {} (weak ref persisted to {}; credential: {})",
                reference.id,
                reference.url,
                path.display(),
                reference
                    .credential_ref
                    .as_ref()
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            );
            Ok(())
        }
        EndpointAction::List => {
            let records = registry.list();
            if records.is_empty() {
                eprintln!(
                    "no persisted endpoints — import one with `flux endpoint import <id>` (store: {})",
                    path.display()
                );
                return Ok(());
            }
            for r in &records {
                println!("{}", render_endpoint_row(r));
            }
            Ok(())
        }
        EndpointAction::Show { id } => {
            let r = registry
                .resolve(&id)
                .ok_or_else(|| anyhow::anyhow!("no persisted endpoint `{id}`"))?;
            let ep = &r.endpoint;
            println!("{}        {}", style::bold("id"), ep.id);
            println!(
                "{}   {}",
                style::bold("product"),
                if ep.product.is_empty() {
                    "-"
                } else {
                    &ep.product
                }
            );
            println!("{}       {}", style::bold("url"), ep.url); // bare URL — no embedded creds
            if let Some(proto) = &ep.protocol {
                println!("{}  {proto}", style::bold("protocol"));
            }
            println!("{}     {}", style::bold("owner"), r.owner);
            println!("{}    {:?}", style::bold("source"), ep.source);
            if let Some(ttl) = r.ttl_secs {
                println!("{}       {ttl}s", style::bold("ttl"));
            }
            if let Some(h) = &r.health {
                println!("{}    {h}", style::bold("health"));
            }
            if !ep.labels.is_empty() {
                let labels: Vec<String> =
                    ep.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("{}    {}", style::bold("labels"), labels.join(", "));
            }
            // The credential is shown only as a LOCATION (or `none`) — never a value.
            println!("{} {}", style::bold("credential"), credential_location(&r));
            Ok(())
        }
        EndpointAction::Resolve { id } => {
            let r = registry
                .resolve(&id)
                .ok_or_else(|| anyhow::anyhow!("no persisted endpoint `{id}`"))?;
            let ep = &r.endpoint;
            // Operator diagnostic: report what the reference WOULD bind to — source, bare host/url, and
            // the credential-ref LOCATION. The value is deliberately not shown: it is resolved host-side
            // at connect time (and may be a cross-plugin hop), never by this read-only operator command.
            println!(
                "{}       {} (owner={})",
                style::bold("source"),
                {
                    match ep.source {
                        flux_secret::endpoint::SourceKind::Config => "config",
                        flux_secret::endpoint::SourceKind::Discovered => "discovered",
                    }
                },
                r.owner
            );
            println!("{}          {}", style::bold("url"), ep.url);
            match &ep.credential_ref {
                Some(cred) => {
                    println!("{}   {cred}", style::bold("credential-ref"));
                    println!(
                        "{}       {}",
                        style::bold("credential"),
                        style::dim("<resolved at connect time, host-side>")
                    );
                }
                None => println!("{}   none (unauthenticated)", style::bold("credential-ref")),
            }
            Ok(())
        }
        EndpointAction::Import { id, from_json } => {
            // For a standalone CLI, the in-memory registry is just the loaded store. Import the record
            // if it is already present; otherwise accept an explicit `--from-json <EndpointRef>`; else
            // error clearly. (The agent-facing `endpoint.import` op is the primary in-session path.)
            if registry.resolve(&id).is_none() {
                let Some(json) = from_json else {
                    bail!(
                        "no endpoint `{id}` in the store — discover/select it in a session first \
                         (the `endpoint.import` op persists it), or pass `--from-json <EndpointRef>`"
                    );
                };
                let reference: flux_secret::endpoint::EndpointRef = serde_json::from_str(&json)
                    .context("parse --from-json as a weak EndpointRef")?;
                if reference.id != id {
                    bail!("`--from-json` id `{}` does not match `{id}`", reference.id);
                }
                // Stamp the record with the source's owner semantics: a discovered ref keeps no owner
                // info in the bare ref, so attribute an explicit import to `config` (operator-imported).
                registry.put(flux_secret::endpoint::EndpointRecord::config(reference));
            }
            let reference = registry
                .import(&id)
                .map_err(|e| anyhow::anyhow!("import endpoint `{id}`: {e}"))?;
            println!(
                "imported {} → {} (weak ref persisted to {}; credential: {})",
                reference.id,
                reference.url,
                path.display(),
                reference
                    .credential_ref
                    .as_ref()
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            );
            Ok(())
        }
    }
}

/// `flux plugin add <name> <program> [args…] | ls | pin <name> <version> | rollback <name>`.
pub(super) async fn run_plugin(action: Option<PluginAction>) -> Result<()> {
    let dir = plugins_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    run_plugin_in(&dir, action).await
}

/// The dir-parameterized body of [`run_plugin`] (tests pass a temp dir so they don't touch `HOME`).
pub(super) async fn run_plugin_in(
    dir: &std::path::Path,
    action: Option<PluginAction>,
) -> Result<()> {
    match action.unwrap_or(PluginAction::Ls) {
        PluginAction::Login { name, password } => login_plugin(&name, password).await,
        PluginAction::Ls => {
            let found = flux_plugin::discover(dir);
            if found.is_empty() {
                println!("no plugins (add one with `flux plugin add <name> <program> [args…]`)");
            }
            for p in found {
                let pin = p
                    .descriptor
                    .pinned
                    .as_deref()
                    .map(|v| format!("  (pinned {v})"))
                    .unwrap_or_default();
                let ver = p
                    .descriptor
                    .version
                    .as_deref()
                    .map(|v| format!("  v{v}"))
                    .unwrap_or_default();
                // Re-hash against the recorded sha256 (D-48) — sub-millisecond per plugin, so
                // even the terse listing shows drift instead of a stale descriptor-field label.
                let verification = match flux_plugin::verify_descriptor(&p.descriptor) {
                    flux_plugin::Verification::Verified => style::green("verified"),
                    flux_plugin::Verification::HashDrift { .. } => style::red("hash drift"),
                    flux_plugin::Verification::UnverifiedLocal => style::dim("unverified (local)"),
                    flux_plugin::Verification::UnverifiedFromSource => {
                        style::dim("from-source (unverified)")
                    }
                };
                println!(
                    "{:<16} {} {}{pin}{ver}  [{verification}]",
                    p.name,
                    p.descriptor.program,
                    p.descriptor.args.join(" "),
                );
            }
            Ok(())
        }
        PluginAction::Add {
            name,
            program,
            args,
        } => {
            flux_plugin::add_descriptor(
                dir,
                &name,
                &flux_plugin::PluginDescriptor {
                    program: program.clone(),
                    args,
                    pinned: None,
                    ..Default::default()
                },
            )
            .context("write plugin descriptor")?;
            println!("added plugin `{name}` → {program}");
            Ok(())
        }
        PluginAction::Pin { name, version } => {
            if flux_plugin::pack::CURRENT_TARGET.is_empty() {
                bail!(
                    "no prebuilt plugin pack for this platform — build from source and use \
                     `flux plugin install --dir` instead (pin manages the versioned store)"
                );
            }
            let store_root = dir.join("bin");
            let fetcher = flux_plugin::pack::GithubFetcher::default();
            let req = flux_plugin::pack::InstallRequest {
                fetcher: &fetcher,
                repo: flux_plugin::pack::DEFAULT_REPO,
                public_key: flux_plugin::pack::PUBLIC_KEY,
                descriptors_dir: dir,
                store_root: &store_root,
                target: flux_plugin::pack::CURRENT_TARGET,
            };
            let out = flux_plugin::pack::pin(&req, &name, &version)
                .await
                .map_err(|e| anyhow::anyhow!("pin plugin: {e}"))?;
            let how = if out.fetched {
                "fetched into the versioned store"
            } else {
                "already in the versioned store — offline repoint"
            };
            let prev = out
                .previous
                .map(|p| format!("; previous {p} kept for rollback"))
                .unwrap_or_default();
            println!(
                "pinned `{}` to {} ({how}; sha256 recorded, enforced at every spawn{prev})",
                out.name, out.version
            );
            Ok(())
        }
        PluginAction::Rollback { name } => {
            let store_root = dir.join("bin");
            let out = flux_plugin::pack::rollback(
                dir,
                &store_root,
                flux_plugin::pack::CURRENT_TARGET,
                &name,
            )
            .map_err(|e| anyhow::anyhow!("rollback plugin: {e}"))?;
            println!(
                "rolled back `{}`: {} → {} (offline flip; `rollback` again to return)",
                out.name,
                out.from.unwrap_or_else(|| "<unversioned>".into()),
                out.to
            );
            Ok(())
        }
        PluginAction::Call {
            name,
            op,
            input,
            arg,
            dry_run,
            no_validate,
        } => {
            let desc = flux_plugin::load_descriptor(dir, &name)
                .context("load plugin descriptor")?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no such plugin `{name}` — add it with `flux plugin add`/`install` first"
                    )
                })?;
            let base: Option<Value> = match input {
                Some(s) => Some(serde_json::from_str(&s).context("parse <json-input>")?),
                None => None,
            };
            // The same guarded boundary + datasource bridge the agent path uses, over a scratch index.
            // Propagate a malformed config like the agent paths do — swallowing it here would
            // silently drop the user's `[private_net]` plugin grants and refuse the call as
            // ungranted with no hint that the config failed to parse.
            let cwd = std::env::current_dir()?;
            let cfg =
                flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
            let system = Arc::new(System::from_env(&cwd).map_err(|e| anyhow::anyhow!("{e}"))?);
            let backend: Arc<dyn flux_capabilities::DatasourceBackend> =
                Arc::new(flux_capabilities::MemoryBackend::new());
            let mut host = flux_plugin::PluginHost::spawn_verified(&system, &name, &desc)
                .await
                .with_context(|| format!("spawn plugin `{name}` ({})", desc.program))?;
            let manifest = host.manifest().await.context("fetch plugin manifest")?;
            let resolved_op = resolve_plugin_operation_name(&name, &op, &manifest)?;
            // Build the op input from <json-input> + --arg, coercing args to the op's declared
            // input_schema types (Track A1 — fluxplane `operation invoke` ergonomics).
            let schema = manifest
                .operations
                .iter()
                .find(|o| o.name == resolved_op)
                .map(|o| o.input_schema.clone())
                .unwrap_or_else(|| serde_json::json!({}));
            let validate = !no_validate;
            let (input, mut problems) = build_invoke_input(&schema, base, &arg, validate);
            let caps = flux_capabilities::DatasourceHostCaps::new(
                flux_plugin::SystemHostCaps::new(system)
                    .with_manifest(&manifest)
                    .with_private_net_grants(effective_plugin_private_hosts(&cfg, &manifest.name))
                    .with_grant_source(private_net_grant_source_for(&manifest.name)),
                backend.clone(),
            );

            if dry_run {
                // Validate locally, then merge the plugin's own preflight verdict (D-88) when it
                // serves the reserved `plugin.validate` op. That verdict is the SAME check the
                // plugin's runtime dispatch enforces, so a green dry-run can no longer fail the
                // identical validation on the live call. Older plugins without the op keep the
                // schema-only verdict.
                let mut warnings: Vec<String> = Vec::new();
                if manifest
                    .operations
                    .iter()
                    .any(|o| o.name == flux_plugin::VALIDATE_OP)
                {
                    let ask = serde_json::json!({ "operation": resolved_op, "input": input });
                    match host
                        .call_with_host(flux_plugin::VALIDATE_OP, ask, &caps)
                        .await
                    {
                        Ok(verdict) => {
                            let take = |key: &str| -> Vec<String> {
                                verdict
                                    .get(key)
                                    .and_then(|v| v.as_array())
                                    .map(|a| {
                                        a.iter()
                                            .filter_map(|p| p.as_str())
                                            .map(String::from)
                                            .collect()
                                    })
                                    .unwrap_or_default()
                            };
                            problems.extend(take("problems"));
                            warnings.extend(take("warnings"));
                        }
                        Err(e) => eprintln!(
                            "{}",
                            style::dim(&format!(
                                "(plugin preflight unavailable — schema-only verdict: {e})"
                            ))
                        ),
                    }
                }
                let _ = host.shutdown().await;
                // Mask any secret-like input fields the op declared (GL-031) before echoing the
                // preview — the live `input` sent to the plugin above stays raw; only this copy is
                // printed. A dry-run of a CI-variable write must not leak the `value` to scrollback.
                let mut echoed_input = input.clone();
                redact_plugin_echo(&mut echoed_input, &manifest, &resolved_op);
                let dry = serde_json::json!({
                    "plugin": name,
                    "operation": resolved_op,
                    "valid": problems.is_empty(),
                    "problems": problems,
                    "warnings": warnings,
                    "input": echoed_input,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&dry).unwrap_or_else(|_| dry.to_string())
                );
                return Ok(());
            }
            if validate && !problems.is_empty() {
                let _ = host.shutdown().await;
                bail!(
                    "invalid input for `{name}.{resolved_op}` ({} problem(s); --no-validate to invoke anyway):\n  - {}",
                    problems.len(),
                    problems.join("\n  - ")
                );
            }
            let result = host.call_with_host(&resolved_op, input, &caps).await;
            let _ = host.shutdown().await;
            let mut value = result.map_err(|e| {
                anyhow::anyhow!(
                    "plugin `{name}` op `{resolved_op}`: {}",
                    scrub_plugin_error(&manifest, &resolved_op, e.to_string())
                )
            })?;
            // C-312 — the credential boundary, before the value is echoed. This path is an ingest
            // surface exactly like the projected-tool path: `flux plugin call connectors.…` is how
            // an operator pokes a connectors deployment by hand, and a raw `println!` of the
            // response puts whatever it carries into terminal scrollback and shell history.
            if let Some(refusal) = refuse_platform_response(&value, &manifest, &resolved_op) {
                bail!("{refusal}");
            }
            // Mask the op's declared secret-like result fields (GL-031) before echoing — a variable
            // write's response carries the value back and must not leak into scrollback/logs.
            redact_plugin_echo(&mut value, &manifest, &resolved_op);
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
            );
            let n = backend.len();
            if n > 0 {
                eprintln!("{}", style::dim(&format!("({n} record(s) contributed)")));
            }
            Ok(())
        }
        PluginAction::Install {
            names,
            all,
            dir: local_dir,
            git,
            tag,
            rev,
            branch,
            bin,
            force,
        } => {
            // The `--git` ref/bin/force flags are `requires = "git"` at the clap layer, but clap
            // skips that requirement when it would collide with a present conflicting positional
            // (`install gitlab --tag t`), so guard it at runtime too — a ref/bin/force flag without
            // `--git` is a misuse, not a silent remote/`--dir` install that ignores it.
            if git.is_none()
                && (tag.is_some() || rev.is_some() || branch.is_some() || bin.is_some() || force)
            {
                bail!(
                    "`--tag`/`--rev`/`--branch`/`--bin`/`--force` apply only to a `--git <url>` \
                     source install"
                );
            }
            if let Some(url) = git {
                return run_plugin_git_install(dir, &url, tag, rev, branch, bin.as_deref(), force)
                    .await;
            }
            match local_dir {
                Some(bin_dir) => {
                    if !names.is_empty() || all {
                        bail!(
                            "`--dir` (local scan) cannot be combined with plugin names or `--all` \
                             (remote pack install) — pick one mode"
                        );
                    }
                    let bin_dir = std::path::PathBuf::from(bin_dir);
                    let binaries = plugin_binaries_in(&bin_dir)
                        .with_context(|| format!("scan {}", bin_dir.display()))?;
                    let mut installed = 0usize;
                    for (name, program) in &binaries {
                        flux_plugin::add_descriptor(
                            dir,
                            name,
                            &flux_plugin::PluginDescriptor {
                                program: program.clone(),
                                args: Vec::new(),
                                pinned: None,
                                ..Default::default()
                            },
                        )
                        .with_context(|| format!("register plugin `{name}`"))?;
                        println!("installed `{name}` → {program} (local, unverified)");
                        installed += 1;
                    }
                    if installed == 0 {
                        eprintln!(
                            "no `flux-plugin-*` binaries in {} (build them first: \
                             `cd plugins && cargo build --release`)",
                            bin_dir.display()
                        );
                    } else {
                        // Prune stale local registrations from an EARLIER scan of this same dir whose
                        // binary is now absent (e.g. a plugin that failed to build in a partial pack
                        // build) — otherwise its descriptor lingers and every later command prints a
                        // "failed to load" warning (N-003). Only unverified/local descriptors whose
                        // recorded program is the `flux-plugin-<name>` binary directly inside THIS dir
                        // are eligible; verified pack installs (a recorded sha256) and plugins
                        // registered elsewhere are never touched. Gated on `installed > 0`, so a
                        // typo'd/empty `--dir` never wipes a whole set of registrations.
                        let canon_dir = bin_dir.canonicalize().unwrap_or_else(|_| bin_dir.clone());
                        let present: std::collections::HashSet<&str> =
                            binaries.iter().map(|(n, _)| n.as_str()).collect();
                        for d in flux_plugin::discover(dir) {
                            if present.contains(d.name.as_str()) || d.descriptor.sha256.is_some() {
                                continue;
                            }
                            let prog = std::path::Path::new(&d.descriptor.program);
                            let owned_here = prog.parent().is_some_and(|p| {
                                p == canon_dir.as_path() || p == bin_dir.as_path()
                            });
                            let fname = prog.file_name().and_then(|f| f.to_str()).unwrap_or("");
                            let name_matches = fname == format!("flux-plugin-{}", d.name)
                                || fname == format!("flux-plugin-{}.exe", d.name);
                            if owned_here
                                && name_matches
                                && flux_plugin::remove_descriptor(dir, &d.name).unwrap_or(false)
                            {
                                println!(
                                    "pruned stale `{}` (binary no longer in {})",
                                    d.name,
                                    bin_dir.display()
                                );
                            }
                        }
                    }
                    Ok(())
                }
                None => {
                    if names.is_empty() && !all {
                        bail!(
                            "`flux plugin install` needs plugin name(s), `--all` (remote pack \
                             install), `--dir [path]` (local scan of a built \
                             `plugins/target/release`), or `--git <url>` (build from source) — \
                             bare `install` no longer guesses"
                        );
                    }
                    if flux_plugin::pack::CURRENT_TARGET.is_empty() {
                        bail!(
                            "no prebuilt plugin pack for this platform — build from source: \
                             `git clone https://github.com/{} && cd plugins && cargo build \
                             --release && flux plugin install --dir plugins/target/release`",
                            flux_plugin::pack::DEFAULT_REPO
                        );
                    }
                    let store_root = dir.join("bin");
                    let fetcher = flux_plugin::pack::GithubFetcher::default();
                    let req = flux_plugin::pack::InstallRequest {
                        fetcher: &fetcher,
                        repo: flux_plugin::pack::DEFAULT_REPO,
                        public_key: flux_plugin::pack::PUBLIC_KEY,
                        descriptors_dir: dir,
                        store_root: &store_root,
                        target: flux_plugin::pack::CURRENT_TARGET,
                    };
                    let installed = flux_plugin::pack::install_many(&req, &names, all)
                        .await
                        .map_err(|e| anyhow::anyhow!("remote plugin install: {e}"))?;
                    for p in installed {
                        if p.already_installed {
                            println!(
                                "`{}` {} already installed (source {}) — no-op",
                                p.name, p.version, p.source
                            );
                        } else {
                            println!(
                                "installed `{}` {} → {} (verified, source {})",
                                p.name,
                                p.version,
                                p.program.display(),
                                p.source
                            );
                        }
                    }
                    Ok(())
                }
            }
        }
        PluginAction::Skill {
            install,
            global,
            out,
        } => run_plugin_skill(dir, install, global, out).await,
        PluginAction::Uninstall { name, purge } => {
            let removed = flux_plugin::remove_descriptor(dir, &name).context("uninstall plugin")?;
            let purged = if purge {
                flux_plugin::pack::purge_store(&dir.join("bin"), &name)
                    .map_err(|e| anyhow::anyhow!("purge versioned store: {e}"))?
            } else {
                false
            };
            if removed {
                println!("uninstalled plugin `{name}`");
            }
            if purged {
                println!("purged versioned store for `{name}` (all downloaded versions)");
            }
            if !removed && !purged {
                bail!("no such plugin `{name}` — nothing to uninstall");
            }
            Ok(())
        }
        PluginAction::Refresh { name } => refresh_plugin_catalog(dir, &name).await,
        PluginAction::Status { name } => {
            match name {
                Some(n) => {
                    let report = plugin_status_one(dir, &n).await?;
                    print_plugin_status_report(&report);
                }
                None => {
                    let reports = plugin_status_all(dir).await?;
                    if reports.is_empty() {
                        println!(
                            "no plugins (add one with `flux plugin add <name> <program> [args…]`)"
                        );
                    }
                    for r in reports {
                        print_plugin_status_report(&r);
                    }
                }
            }
            Ok(())
        }
    }
}

// --- plugin `refresh`: re-project the catalog from a second manifest fetch (C-310) -----

/// `flux plugin refresh <name>` — load the plugin, re-fetch its manifest over the *same* open
/// subprocess, and re-project its operations into a catalog, reporting the delta.
///
/// The load and the refresh both run here, against one process, which is what makes this the
/// operator's answer to "does the plugin advertise the new operations yet?" after a
/// `flux auth login <name>`: the second fetch is the one an already-open session would make.
/// It is also the drift check on a plugin whose manifest is not stable across two fetches — a
/// refreshed manifest that widens the granted capabilities, or re-scopes an operation under a name
/// it already used, is refused here rather than discovered at dispatch time.
async fn refresh_plugin_catalog(dir: &std::path::Path, name: &str) -> Result<()> {
    let desc = flux_plugin::load_descriptor(dir, name)
        .context("load plugin descriptor")?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no such plugin `{name}` — add it with `flux plugin add`/`install` first"
            )
        })?;
    // The same guarded boundary + datasource bridge `flux plugin call` uses, over a scratch index.
    let cwd = std::env::current_dir()?;
    let cfg = flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
    let system = Arc::new(System::from_env(&cwd).map_err(|e| anyhow::anyhow!("{e}"))?);
    let backend: Arc<dyn flux_capabilities::DatasourceBackend> =
        Arc::new(flux_capabilities::MemoryBackend::new());
    let private_hosts = effective_plugin_private_hosts(&cfg, name);
    let grant_source = private_net_grant_source_for(name);
    let caps_system = system.clone();

    let mut loaded = flux_plugin::load_plugin_tools(&system, name, &desc, move |manifest| {
        Arc::new(flux_capabilities::DatasourceHostCaps::new(
            flux_plugin::SystemHostCaps::new(caps_system)
                .with_manifest(manifest)
                .with_private_net_grants(private_hosts)
                .with_grant_source(grant_source),
            backend,
        ))
    })
    .await
    .with_context(|| format!("load plugin `{name}` ({})", desc.program))?;

    let source = format!("plugin:{name}");
    let mut registry = flux_runtime::ToolRegistry::new();
    for tool in &loaded.tools {
        registry
            .try_register_from(source.clone(), tool.clone())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    // The refresh itself, through the entry point that moves the registry and the plugin together.
    // A refusal must not read as a crash: the catalog the plugin loaded with is still intact, and
    // saying so is the actionable half of the message.
    let report = loaded
        .refresh_into(&mut registry, &source)
        .await
        .map(|refresh| {
            format_refresh_report(
                name,
                &refresh.added,
                &refresh.removed,
                &refresh.retained,
                &refresh.coherence_warnings,
            )
        });

    // Release the tools' shared host references before shutting the subprocess down.
    let flux_plugin::LoadedPlugin { tools, host, .. } = loaded;
    drop(tools);
    drop(registry);
    if let Ok(host) = Arc::try_unwrap(host) {
        let _ = host.into_inner().shutdown().await;
    }

    match report {
        Ok(text) => {
            println!("{text}");
            Ok(())
        }
        Err(e) => bail!(
            "{e}\nthe catalog is unchanged — `{name}`'s operations are still the ones it loaded with"
        ),
    }
}

/// Render a completed refresh for the operator. Pure so the wording is testable without a
/// subprocess: the delta first (what appeared, what was withdrawn), then any C-191 coherence
/// warnings, which — as at load — describe operations that still loaded.
pub(super) fn format_refresh_report(
    plugin: &str,
    added: &[String],
    removed: &[String],
    retained: &[String],
    warnings: &[String],
) -> String {
    let total = added.len() + retained.len();
    let mut out = if added.is_empty() && removed.is_empty() {
        format!("plugin `{plugin}`: catalog refreshed — no change ({total} operation(s))")
    } else {
        format!(
            "plugin `{plugin}`: catalog refreshed — {} added, {} withdrawn, {} unchanged \
             ({total} operation(s))",
            added.len(),
            removed.len(),
            retained.len()
        )
    };
    for name in added {
        out.push_str(&format!("\n  + {name}"));
    }
    for name in removed {
        out.push_str(&format!("\n  - {name}"));
    }
    for warning in warnings {
        out.push_str(&format!(
            "\n{}",
            style::dim(&format!("  (incoherent metadata) {warning}"))
        ));
    }
    out
}

// --- plugin `status`: liveness + declared surface (D-19) -------------------------------

/// Result of probing one plugin's health + surface. `missing` is determined without spawning
/// (the binary does not resolve on `PATH`); `unloadable` means the binary spawned but its
/// manifest would not load (e.g. it is not a flux plugin); `live` means the manifest loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Liveness {
    Live,
    Missing,
    Unloadable(String),
}

#[derive(Debug, Clone)]
pub(super) struct PluginStatusReport {
    pub(super) name: String,
    pub(super) program: String,
    pub(super) args: Vec<String>,
    pub(super) pin: Option<String>,
    /// The installed version, if the descriptor carries one (remote installs only — D-47).
    pub(super) version: Option<String>,
    /// The D-48 verification outcome: the binary on disk **re-hashed** against the descriptor's
    /// recorded `sha256` — `verified`, `hash drift` (also a spawn refusal), or
    /// `unverified (local)` for hashless dev descriptors.
    pub(super) verification: flux_plugin::Verification,
    pub(super) liveness: Liveness,
    pub(super) manifest: Option<flux_plugin::PluginManifest>,
}

/// Resolve `program` (an absolute/relative path, or a bare name on `PATH`) to an existing file.
/// Used for the `missing` vs `unloadable` split in `status` without spawning a process.
pub(super) fn program_resolves(program: &str) -> bool {
    let p = std::path::Path::new(program);
    // NOTE: `parent()` is `Some("")` even for a bare one-component name, so it cannot detect
    // "has a separator" — count components instead, or the PATH search below is unreachable
    // and a bare-name plugin that spawns fine gets misreported as `missing`.
    if p.is_absolute() || p.components().count() > 1 {
        // Absolute or relative path with a separator — check the file directly.
        return p.is_file();
    }
    // Bare name — search the dirs on `PATH`.
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|d| d.join(program).is_file())
}

/// Build a status report for one plugin. A missing binary is reported without spawning (no
/// process, no manifest round-trip); a present binary is spawned and its manifest loaded so the
/// declared surface can be summarized. Never panics on a bad binary.
pub(super) async fn build_status_report(
    name: &str,
    d: flux_plugin::PluginDescriptor,
) -> Result<PluginStatusReport> {
    let binary_exists = program_resolves(&d.program);
    // Re-hash against the recorded sha256 (D-48). On drift the probe below is skipped — the
    // verified spawn path would refuse anyway; skipping keeps `status` from paying a doomed spawn.
    let verification = flux_plugin::verify_descriptor(&d);
    let (liveness, manifest) = if !binary_exists {
        (Liveness::Missing, None)
    } else if let flux_plugin::Verification::HashDrift { .. } = &verification {
        (
            Liveness::Unloadable("refused: hash drift (see verification)".into()),
            None,
        )
    } else {
        match spawn_and_load_manifest(name, &d).await {
            Ok(m) => (Liveness::Live, Some(m)),
            Err(e) => (Liveness::Unloadable(e.to_string()), None),
        }
    };
    Ok(PluginStatusReport {
        name: name.to_string(),
        program: d.program,
        args: d.args,
        pin: d.pinned,
        version: d.version,
        verification,
        liveness,
        manifest,
    })
}

/// Inspect one installed plugin by name.
pub(super) async fn plugin_status_one(
    dir: &std::path::Path,
    name: &str,
) -> Result<PluginStatusReport> {
    let d = flux_plugin::load_descriptor(dir, name)
        .with_context(|| format!("load descriptor `{name}`"))?
        .ok_or_else(|| anyhow::anyhow!("no such plugin `{name}`"))?;
    build_status_report(name, d).await
}

/// Summarize every installed plugin (sorted by name, matching `discover`).
pub(super) async fn plugin_status_all(dir: &std::path::Path) -> Result<Vec<PluginStatusReport>> {
    let mut out = Vec::new();
    for p in flux_plugin::discover(dir) {
        out.push(build_status_report(&p.name, p.descriptor).await?);
    }
    Ok(out)
}

/// Spawn the plugin and load its manifest (liveness probe). Reuses the one guarded, D-48
/// hash-verified spawn path (`PluginHost::spawn_verified` over a workspace-rooted `System`), the
/// same boundary `call` and agent discovery use.
pub(super) async fn spawn_and_load_manifest(
    name: &str,
    d: &flux_plugin::PluginDescriptor,
) -> Result<flux_plugin::PluginManifest> {
    let system = System::from_env(std::env::current_dir()?).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut host = flux_plugin::PluginHost::spawn_verified(&system, name, d)
        .await
        .with_context(|| format!("spawn `{}`", d.program))?;
    let m = host.manifest().await.context("fetch plugin manifest")?;
    let _ = host.shutdown().await;
    Ok(m)
}

/// Print one plugin's status: header (name → program args, pin) + liveness label, then the
/// declared surface (version, op/auth/endpoint/datasource counts, requested capabilities).
pub(super) fn print_plugin_status_report(r: &PluginStatusReport) {
    let liveness_label = match &r.liveness {
        Liveness::Live => style::green("ok"),
        Liveness::Missing => style::red("missing"),
        Liveness::Unloadable(msg) => style::yellow(&format!("unloadable: {msg}")),
    };
    let pin = r
        .pin
        .as_deref()
        .map(|v| format!("  (pinned {v})"))
        .unwrap_or_default();
    let ver = r
        .version
        .as_deref()
        .map(|v| format!("  v{v}"))
        .unwrap_or_default();
    let short = |h: &str| h.chars().take(12).collect::<String>();
    let verified_label = match &r.verification {
        flux_plugin::Verification::Verified => style::green("verified"),
        flux_plugin::Verification::HashDrift { expected, actual } => style::red(&format!(
            "hash drift: descriptor {}…, binary {}…",
            short(expected),
            short(actual)
        )),
        flux_plugin::Verification::UnverifiedLocal => style::dim("unverified (local)"),
        flux_plugin::Verification::UnverifiedFromSource => style::dim("from-source (unverified)"),
    };
    println!(
        "{:<16} {} {}{pin}{ver}  [{liveness_label}]  [{verified_label}]",
        r.name,
        r.program,
        r.args.join(" ")
    );
    if let Some(m) = &r.manifest {
        let mut surface = vec![format!("{} op(s)", m.operations.len())];
        if !m.auth.is_empty() {
            surface.push(format!("{} auth purpose(s)", m.auth.len()));
        }
        if !m.endpoints.is_empty() {
            surface.push(format!("{} endpoint(s)", m.endpoints.len()));
        }
        if !m.datasources.is_empty() {
            surface.push(format!("{} datasource(s)", m.datasources.len()));
        }
        if !m.discovers.is_empty() {
            surface.push(format!("discovers: {}", m.discovers.join(", ")));
        }
        let caps = &m.capabilities;
        let mut cap_flags: Vec<String> = Vec::new();
        if caps.http {
            cap_flags.push("http".to_string());
        }
        if !caps.process.is_empty() {
            cap_flags.push(format!("process({})", caps.process.len()));
        }
        if !caps.secrets.is_empty() {
            cap_flags.push(format!("secret({})", caps.secrets.len()));
        }
        if !caps.conn.is_empty() {
            cap_flags.push(format!("conn({})", caps.conn.len()));
        }
        if caps.blob {
            cap_flags.push("blob".to_string());
        }
        if caps.discover {
            cap_flags.push("endpoint.discover".to_string());
        }
        if !cap_flags.is_empty() {
            surface.push(format!("caps: {}", cap_flags.join(", ")));
        }
        let ver = if m.version.is_empty() {
            String::new()
        } else {
            format!("  v{}", m.version)
        };
        println!("    manifest:{ver}  {}", surface.join("  ·  "));
        // Version-agreement check (D-48): a manifest that reports a different version than the
        // descriptor records is reported loudly — but it is a labeling disagreement, not
        // tampering (the hash column above is the integrity statement), so it is not fatal.
        if let Some(dv) = r.version.as_deref() {
            if !m.version.is_empty() && m.version != dv {
                println!(
                    "    {}",
                    style::yellow(&format!(
                        "version mismatch: the descriptor records v{dv} but the manifest \
                         reports v{}",
                        m.version
                    ))
                );
            }
        }
        // Resolution status per declared auth purpose / endpoint — which env key (if any) is
        // set, or whether an endpoint falls back to its declared default, WITHOUT ever printing
        // a resolved secret value. Endpoint base URLs are not secret (`flux endpoint
        // show`/`resolve` already print them), so those are shown in full.
        for a in &m.auth {
            println!("    auth:      {}", describe_auth_resolution(&r.name, a));
        }
        for e in &m.endpoints {
            println!("    endpoint:  {}", describe_endpoint_resolution(e));
        }
    }
}

/// Describe how a declared auth purpose would resolve right now — a stored token (OAuth login or
/// `flux auth set`), or which env key (if any) is set — without ever printing the resolved secret
/// value. Mirrors the host's resolution order: stored token first, declared env keys second.
pub(super) fn describe_auth_resolution(plugin: &str, m: &flux_plugin::AuthMethod) -> String {
    let key = format!("plugin:{plugin}:{}", m.purpose);
    if flux_credentials::load_token(&key).is_some() {
        return if m.oauth2.is_some() {
            format!(
                "✓ {} — stored OAuth token (`flux auth login {plugin}`)",
                m.purpose
            )
        } else {
            format!(
                "✓ {} — stored token (`flux auth set {plugin} {}`)",
                m.purpose, m.purpose
            )
        };
    }
    // An EMPTY env value counts as unset — matching `resolve_manifest_endpoint`, so `status`
    // never claims "configured" for a value resolution will skip.
    for key in &m.env {
        if std::env::var(key).is_ok_and(|v| !v.is_empty()) {
            return format!("✓ {} — env ${key}", m.purpose);
        }
    }
    let configure = if m.oauth2.is_some() {
        format!("`flux auth login {plugin}`")
    } else {
        format!("`flux auth set {plugin} {}`", m.purpose)
    };
    if m.env.is_empty() {
        format!("· {} — not configured ({configure})", m.purpose)
    } else {
        format!(
            "· {} — not configured (env: {}, or {configure})",
            m.purpose,
            m.env.join(", ")
        )
    }
}

/// Describe how a declared endpoint would resolve right now. Base URLs are not secret, so the
/// resolved value itself is shown (the plugin-declared `default` fallback is likewise not secret).
pub(super) fn describe_endpoint_resolution(ep: &flux_plugin::EndpointSpec) -> String {
    // Empty counts as unset, matching `resolve_manifest_endpoint` (which falls to the default).
    for key in &ep.env {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return format!("✓ {} — {v} (env ${key})", ep.name);
            }
        }
    }
    match &ep.default {
        Some(d) => format!("· {} — env not set, defaults to {d}", ep.name),
        None if ep.env.is_empty() => format!("· {} — no env keys declared", ep.name),
        None => format!(
            "· {} — not configured (env: {})",
            ep.name,
            ep.env.join(", ")
        ),
    }
}

pub(super) fn resolve_plugin_operation_name(
    plugin: &str,
    requested: &str,
    manifest: &flux_plugin::PluginManifest,
) -> Result<String> {
    if manifest.operations.iter().any(|op| op.name == requested) {
        return Ok(requested.to_string());
    }

    let prefix = if manifest.name.trim().is_empty() {
        plugin
    } else {
        manifest.name.as_str()
    };
    let qualified = format!("{prefix}.{requested}");
    if manifest.operations.iter().any(|op| op.name == qualified) {
        return Ok(qualified);
    }

    bail!(
        "plugin `{plugin}` has no operation `{requested}` (tried `{qualified}`). Available ops: {}",
        available_plugin_operations(manifest)
    )
}

/// Mask an op's declared secret-like fields in a value `flux plugin call` is about to echo — the
/// dry-run input preview or the live result (GL-031 / D-93). Looks the op's
/// [`redact_fields`](flux_plugin::OperationSpec::redact_fields) up in the manifest and applies the
/// shared host masking so secret-like values (e.g. a CI/pipeline variable `value`) never reach
/// terminal scrollback, logs, or saved transcripts. A no-op when the op declares no secret fields.
pub(super) fn redact_plugin_echo(
    value: &mut Value,
    manifest: &flux_plugin::PluginManifest,
    op: &str,
) {
    if let Some(fields) = manifest
        .operations
        .iter()
        .find(|o| o.name == op)
        .map(|o| o.redact_fields.as_slice())
    {
        flux_plugin::redact_secret_fields(value, fields);
    }
}

/// C-312 — apply the credential boundary to a `flux plugin call` response, or accept it.
///
/// The same check the projected-tool path runs, on the same declaration
/// ([`OperationSpec::platform`](flux_plugin::OperationSpec)), so the two cannot drift: an op that
/// is refused when the agent calls it is refused when the operator calls it by hand.
///
/// **The redactor here is fresh, and that is a real difference from the session path.** A one-shot
/// `flux plugin call` has no session to inherit registered secret values from, so the
/// registered-value pass cannot fire and only shape-based material is caught. That is the weaker
/// half of the check, not the load-bearing one: on this seam the credential flux must never hold is
/// the *vendor's*, which flux never sees and therefore could never have registered. The value the
/// registered pass would add — catching the deployment session bearer echoed back — is genuinely
/// missing on this path, and is recorded here rather than papered over.
///
/// **An op the manifest does not describe is refused, not skipped.** Today `resolved_op` is
/// resolved out of this same manifest, so the miss is unreachable — but that is a property of the
/// current caller, not of this function, and it is the first thing a refactor invalidates. The only
/// safe reading of "no declaration" is that the op cannot be shown to be local, and a boundary that
/// answers `None` there fails open silently: the response prints and nothing records that the check
/// never ran.
fn refuse_platform_response(
    value: &Value,
    manifest: &flux_plugin::PluginManifest,
    op: &str,
) -> Option<String> {
    let Some(spec) = manifest.operations.iter().find(|o| o.name == op) else {
        return Some(format!(
            "plugin `{}` returned a response for operation `{op}`, which its manifest does not \
             declare. Whether the credential boundary applies is read from that declaration, so an \
             undeclared op cannot be shown to be a local one — the response was discarded rather \
             than printed unchecked.",
            manifest.name
        ));
    };
    flux_plugin::credential_boundary::refuse_response(
        spec.platform,
        &manifest.name,
        op,
        value,
        &flux_secret::Redactor::new(),
    )
}

/// C-312 — the failure path of the same seam: a platform-sourced op's error message is discarded
/// when it carries credential material, rather than being printed.
///
/// The undeclared op is discarded here too, for the reason given on [`refuse_platform_response`]:
/// an error body is the ingest surface most likely to carry a raw vendor response, and passing it
/// through because the op could not be found is the fail-open shape in its worst position.
fn scrub_plugin_error(manifest: &flux_plugin::PluginManifest, op: &str, message: String) -> String {
    let Some(spec) = manifest.operations.iter().find(|o| o.name == op) else {
        return format!(
            "plugin `{}` operation `{op}` failed, and its manifest does not declare that operation \
             — so whether the credential boundary applies to its error message could not be \
             determined. The message was discarded rather than printed unchecked.",
            manifest.name
        );
    };
    flux_plugin::credential_boundary::scrub_error(
        spec.platform,
        &manifest.name,
        op,
        message,
        &flux_secret::Redactor::new(),
    )
}

// ---------------------------------------------------------------------------
// `flux plugin call/run` — schema-coerced `--arg` input building (Track A1).
//
// Mirrors the fluxplane `operation invoke` ergonomics: build the op input from `--arg key=value`
// flags, coercing each value to the field's declared `input_schema` type, then validate required
// fields. `<json-input>` is the base object; `--arg` values merge over it. `--dry-run` validates
// locally and prints the coerced input without spawning the plugin.
// ---------------------------------------------------------------------------

/// Resolve a property's JSON-schema node, following `$ref` → `definitions` and `anyOf`
/// (schemars' nullable-Option form) to the concrete field schema.
pub(super) fn resolve_field_schema<'a>(node: &'a Value, defs: &'a Value) -> &'a Value {
    if let Some(obj) = node.as_object() {
        if let Some(r) = obj.get("$ref").and_then(|v| v.as_str()) {
            if let Some(name) = r.strip_prefix("#/definitions/") {
                return defs.get(name).unwrap_or(node);
            }
        }
        if let Some(any) = obj.get("anyOf").and_then(|v| v.as_array()) {
            for m in any {
                if m.get("type").and_then(|v| v.as_str()) != Some("null") {
                    return resolve_field_schema(m, defs);
                }
            }
        }
    }
    node
}

/// The base JSON-Schema "type" of a resolved field, ignoring schemars' nullable wrapping
/// (`type: ["string","null"]` → `"string"`). Returns `None` if the field has no `type`.
pub(super) fn field_base_type(node: &Value) -> Option<String> {
    match node.get("type") {
        Some(Value::Array(arr)) => arr
            .iter()
            .find(|v| v.as_str() != Some("null"))
            .and_then(|v| v.as_str())
            .map(String::from),
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Coerce a raw `--arg` string value to the type declared by `field_schema`. Returns the coerced
/// JSON value or an error message describing the coercion failure (surfaced as a validation
/// problem by the caller).
pub(super) fn coerce_arg_value(field_schema: &Value, defs: &Value, raw: &str) -> Result<Value> {
    let resolved = resolve_field_schema(field_schema, defs);
    let ty = field_base_type(resolved).unwrap_or_else(|| "string".to_string());
    match ty.as_str() {
        "integer" => raw
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| anyhow::anyhow!("expected an integer, got `{raw}`")),
        "number" => raw
            .trim()
            .parse::<f64>()
            .map(Value::from)
            .map_err(|_| anyhow::anyhow!("expected a number, got `{raw}`")),
        "boolean" => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(anyhow::anyhow!(
                "expected a boolean (true/false), got `{raw}`"
            )),
        },
        "array" => {
            // A JSON array literal is parsed verbatim; otherwise comma-split into trimmed
            // strings (the common CLI ergonomics for a list arg).
            let trimmed = raw.trim();
            if trimmed.starts_with('[') {
                serde_json::from_str(trimmed)
                    .map_err(|e| anyhow::anyhow!("expected a JSON array, got `{raw}` ({e})"))
            } else {
                let items: Vec<Value> = trimmed
                    .split(',')
                    .map(|s| Value::String(s.trim().to_string()))
                    .filter(|v| !v.as_str().unwrap_or("").is_empty())
                    .collect();
                Ok(Value::Array(items))
            }
        }
        "object" => serde_json::from_str(raw.trim())
            .map_err(|e| anyhow::anyhow!("expected a JSON object, got `{raw}` ({e})")),
        _ => {
            // string (default). Validate enum membership if the field declares one.
            if let Some(en) = resolved.get("enum").and_then(|v| v.as_array()) {
                if !en.iter().any(|v| v.as_str() == Some(raw)) {
                    let allowed: Vec<String> = en
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    return Err(anyhow::anyhow!(
                        "`{raw}` is not one of: {}",
                        allowed.join(", ")
                    ));
                }
            }
            Ok(Value::String(raw.to_string()))
        }
    }
}

/// Build the op input from a base JSON object (the positional `<json-input>`) plus `--arg key=value`
/// flags, coercing each arg to its declared schema type and merging over the base. Returns the
/// coerced input plus a list of validation problems (unknown fields, type-coercion failures,
/// missing required fields). `validate: false` skips coercion (args pass through as strings) and
/// the required-field check — degraded discovery must never block a valid call.
pub(super) fn build_invoke_input(
    schema: &Value,
    base: Option<Value>,
    args: &[String],
    validate: bool,
) -> (Value, Vec<String>) {
    let mut problems: Vec<String> = Vec::new();
    let mut input = match base {
        Some(Value::Object(m)) => m,
        Some(other) => {
            problems.push(format!("<json-input> must be a JSON object, got {other}"));
            serde_json::Map::new()
        }
        None => serde_json::Map::new(),
    };
    let defs = schema
        .get("definitions")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let properties = schema.get("properties").and_then(|v| v.as_object());

    for arg in args {
        let eq = match arg.find('=') {
            Some(i) => i,
            None => {
                problems.push(format!("--arg `{arg}` is not `key=value`"));
                continue;
            }
        };
        let key = arg[..eq].to_string();
        let raw_val = arg[eq + 1..].to_string();
        let Some(props) = properties else {
            // No schema properties: pass through as a string (lenient).
            input.insert(key.clone(), Value::String(raw_val));
            continue;
        };
        let Some(field_schema) = props.get(&key) else {
            // Unknown field. Under validation, flag it; still insert as a string (handlers may
            // read leniently, like the flux runtime).
            if validate {
                problems.push(format!("--arg `{key}` is not a declared field"));
            }
            input.insert(key.clone(), Value::String(raw_val));
            continue;
        };
        let value = if validate {
            match coerce_arg_value(field_schema, &defs, &raw_val) {
                Ok(v) => v,
                Err(e) => {
                    problems.push(format!("--arg `{key}`: {e}"));
                    // Insert the raw string so the call can still proceed under --no-validate
                    // or so the user sees the value in --dry-run.
                    Value::String(raw_val)
                }
            }
        } else {
            Value::String(raw_val)
        };
        input.insert(key.clone(), value);
    }

    if validate {
        let required: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        for req in required {
            if !input.contains_key(req) {
                problems.push(format!("missing required field `{req}`"));
            }
        }
    }

    (Value::Object(input), problems)
}

pub(super) fn available_plugin_operations(manifest: &flux_plugin::PluginManifest) -> String {
    let mut names: Vec<&str> = manifest
        .operations
        .iter()
        .map(|op| op.name.as_str())
        .collect();
    names.sort_unstable();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

/// `flux skill [type] [--install] [--global]`: render or install the generated Flux skills.
/// (`--global` without `--install` is a clap-level `requires` error, not checked here.)
pub(super) async fn run_skill(
    type_: Option<skill_cmd::SkillType>,
    install: bool,
    global: bool,
) -> Result<()> {
    if !install {
        let rendered = match type_ {
            Some(kind) => render_generated_skill(kind).await?,
            None => skill_cmd::render_root_skill(),
        };
        print!("{}", rendered.skill_md);
        if !rendered.references.is_empty() {
            eprintln!(
                "{}",
                style::dim(&format!(
                    "({} reference file(s) omitted on stdout; rerun with --install to write them)",
                    rendered.references.len()
                ))
            );
        }
        return Ok(());
    }

    let root = skills_root_dir(global)?;
    let mut rendered = vec![skill_cmd::render_root_skill()];
    match type_ {
        Some(kind) => rendered.push(render_generated_skill(kind).await?),
        None => {
            for kind in skill_cmd::SkillType::all() {
                rendered.push(render_generated_skill(kind).await?);
            }
        }
    }

    std::fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
    let mut paths = Vec::new();
    for skill in &rendered {
        paths.push(write_generated_skill(&root, skill)?);
    }
    println!(
        "installed {} generated skill(s) → {}",
        paths.len(),
        root.display()
    );
    Ok(())
}

pub(super) async fn render_generated_skill(
    kind: skill_cmd::SkillType,
) -> Result<skill_cmd::RenderedSkill> {
    match kind {
        skill_cmd::SkillType::Cli => Ok(skill_cmd::render_cli_skill(Cli::command())),
        skill_cmd::SkillType::Lang => Ok(skill_cmd::render_lang_skill()),
        skill_cmd::SkillType::Plugin => {
            let dir = plugins_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
            let plugins = load_plugin_manifests(&dir).await?;
            Ok(skill_cmd::render_plugin_skill(&plugins))
        }
        skill_cmd::SkillType::Ops => {
            let (registry, groups) = skill_ops_registry()?;
            Ok(skill_cmd::render_ops_skill(&registry, &groups))
        }
    }
}

/// Build the operation catalog that can be rendered without starting providers or plugin hosts.
pub(super) fn skill_ops_registry() -> Result<(ToolRegistry, Vec<flux_evidence::ToolGroup>)> {
    let mut registry = ToolRegistry::new();
    flux_tools::try_register_builtins(&mut registry)?;
    flux_eval::try_register_eval_ops(&mut registry)?;
    flux_tools::try_register_reflect(&mut registry)?;
    // Native web ops for the catalog render (no egress config / audit — this registry never fetches).
    flux_web::try_register_web(&mut registry, &flux_web::WebOptions::default())?;
    flux_capabilities::try_register_datasource_ops(
        &mut registry,
        Arc::new(flux_capabilities::MemoryBackend::new()),
    )?;

    let cwd = std::env::current_dir()?;
    let mut groups = flux_tools::groups::builtin_groups();
    groups.push(flux_eval::eval_group());
    groups.push(flux_web::browser_group());
    let groups = flux_config::merge_groups(
        groups,
        flux_runtime::metadata::load_groups(&cwd).context("load .flux/groups.toml")?,
    );
    Ok((registry, groups))
}

/// The generated skill root directory: project `.flux/skills`, or global `~/.claude/skills`.
pub(super) fn skills_root_dir(global: bool) -> Result<std::path::PathBuf> {
    if global {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        Ok(home.join(".claude").join("skills"))
    } else {
        Ok(std::env::current_dir()?.join(".flux").join("skills"))
    }
}

pub(super) fn write_generated_skill(
    root: &std::path::Path,
    skill: &skill_cmd::RenderedSkill,
) -> Result<std::path::PathBuf> {
    let dir = root.join(&skill.name);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    } else if dir.exists() {
        std::fs::remove_file(&dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let skill_file = dir.join("SKILL.md");
    std::fs::write(&skill_file, &skill.skill_md)
        .with_context(|| format!("write {}", skill_file.display()))?;
    write_skill_references(&dir.join("references"), &skill.references)?;
    Ok(dir)
}

/// Spawns each plugin only to fetch its manifest (no op call); a plugin that fails to spawn/manifest
/// is skipped with a note rather than aborting the whole catalog.
pub(super) async fn load_plugin_manifests(
    dir: &std::path::Path,
) -> Result<Vec<(String, flux_plugin::PluginManifest)>> {
    let mut plugins: Vec<(String, flux_plugin::PluginManifest)> = Vec::new();
    // Plugins launch through the one guarded spawn path, which needs a workspace-rooted System.
    let system = System::from_env(std::env::current_dir()?).map_err(|e| anyhow::anyhow!("{e}"))?;
    // Same stale-registration handling as the agent-startup loops: dead descriptors get ONE
    // aggregated line instead of a doomed spawn attempt + per-plugin noise each.
    let (discovered, stale) = split_stale_plugins(flux_plugin::discover(dir));
    warn_stale_plugins(&stale);
    for p in discovered {
        match flux_plugin::PluginHost::spawn_verified(&system, &p.name, &p.descriptor).await {
            Ok(mut host) => {
                match host.manifest().await {
                    Ok(m) => plugins.push((p.name.clone(), m)),
                    Err(e) => eprintln!(
                        "{}",
                        style::dim(&format!("skip `{}`: manifest error: {e}", p.name))
                    ),
                }
                let _ = host.shutdown().await;
            }
            Err(e) => eprintln!(
                "{}",
                style::dim(&format!("skip `{}`: spawn error: {e}", p.name))
            ),
        }
    }
    Ok(plugins)
}

/// Legacy alias for `flux skill plugin`: render the generated plugin skill from installed manifests.
pub(super) async fn run_plugin_skill(
    dir: &std::path::Path,
    install: bool,
    global: bool,
    out: Option<String>,
) -> Result<()> {
    let plugins = load_plugin_manifests(dir).await?;
    let rendered = skill_cmd::render_plugin_skill(&plugins);

    if let Some(out) = out {
        let out = std::path::PathBuf::from(out);
        std::fs::write(&out, &rendered.skill_md)
            .with_context(|| format!("write {}", out.display()))?;
        let refdir = out
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("references");
        write_skill_references(&refdir, &rendered.references)?;
        println!(
            "wrote {} (+ {} reference(s))",
            out.display(),
            rendered.references.len()
        );
        return Ok(());
    }

    if install {
        let base = skills_root_dir(global)?;
        std::fs::create_dir_all(&base).with_context(|| format!("create {}", base.display()))?;
        let dir = write_generated_skill(&base, &rendered)?;
        println!(
            "installed flux-plugin skill → {} ({} plugin(s), {} reference(s))",
            dir.display(),
            plugins.len(),
            rendered.references.len()
        );
        return Ok(());
    }

    print!("{}", rendered.skill_md);
    Ok(())
}

/// Write each generated `references/<plugin>.md` into `dir` (created on demand).
pub(super) fn write_skill_references(
    dir: &std::path::Path,
    refs: &[(String, String)],
) -> Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    for (name, md) in refs {
        let f = dir.join(format!("{name}.md"));
        std::fs::write(&f, md).with_context(|| format!("write {}", f.display()))?;
    }
    Ok(())
}

/// Whether a `--git` source build is pre-approved non-interactively — `FLUX_ALLOW_SOURCE_BUILD`
/// truthy (mirrors [`private_net_cli_override`]/`FLUX_ALLOW_PRIVATE_NET`). Building unverified
/// source is code execution, so an SSRF-style env gate is the non-interactive consent channel.
pub(super) fn source_build_preapproved() -> bool {
    flux_system::env_truthy("FLUX_ALLOW_SOURCE_BUILD")
}

/// The D-87 trust gate: building unverified remote source is arbitrary code execution, so disclose
/// the resolved commit and require explicit consent BEFORE the build. `FLUX_ALLOW_SOURCE_BUILD=1`
/// pre-approves non-interactively; otherwise a `[y/N]` confirm (off a terminal without the flag,
/// [`read_choice`] declines on EOF — fail-safe).
pub(super) async fn confirm_source_build(url: &str, ref_desc: &str, commit: &str) -> bool {
    let short = &commit[..commit.len().min(12)];
    if source_build_preapproved() {
        eprintln!(
            "{}",
            style::dim(&format!(
                "building `{url}` ({ref_desc}) at commit {short} — pre-approved (FLUX_ALLOW_SOURCE_BUILD)"
            ))
        );
        return true;
    }
    let prompt = format!(
        "\n{} `{url}` ({ref_desc}) at commit {short}?\n  This BUILDS and installs unverified \
         source — arbitrary code execution on this machine. [y]es / [N]o: ",
        style::yellow("build + install"),
    );
    matches!(
        read_choice(prompt, ApprovalChoice::Deny).await,
        ApprovalChoice::Allow
    )
}

/// `flux plugin install --git <url> …` (D-87): the source-build install source. Maps the
/// `--tag`/`--rev`/`--branch` ref, wires the guarded [`SystemSourceBuilder`] (clone + build through
/// [`System`], never a raw `Command`) and the trust gate, and delegates the resolve → consent →
/// build → register orchestration to `flux_plugin::pack::install_from_git`.
pub(super) async fn run_plugin_git_install(
    dir: &std::path::Path,
    url: &str,
    tag: Option<String>,
    rev: Option<String>,
    branch: Option<String>,
    requested_bin: Option<&str>,
    force: bool,
) -> Result<()> {
    let git_ref = match (tag, rev, branch) {
        (Some(t), None, None) => flux_plugin::pack::GitRef::Tag(t),
        (None, Some(r), None) => flux_plugin::pack::GitRef::Rev(r),
        (None, None, Some(b)) => flux_plugin::pack::GitRef::Branch(b),
        (None, None, None) => flux_plugin::pack::GitRef::Default,
        _ => bail!("`--tag`, `--rev`, and `--branch` are mutually exclusive — pick one ref"),
    };
    let ref_desc = git_ref.describe();
    let builder = SystemSourceBuilder;
    let src_root = dir.join("src");
    let store_root = dir.join("bin");
    let req = flux_plugin::pack::GitInstallRequest {
        builder: &builder,
        descriptors_dir: dir,
        src_root: &src_root,
        store_root: &store_root,
    };
    let installed = flux_plugin::pack::install_from_git(
        &req,
        url,
        &git_ref,
        requested_bin,
        force,
        |commit: &str| {
            let url = url.to_string();
            let ref_desc = ref_desc.clone();
            let commit = commit.to_string();
            async move { Ok(confirm_source_build(&url, &ref_desc, &commit).await) }
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("git plugin install: {e}"))?;

    if installed.already_installed {
        println!(
            "`{}` already installed from {} at commit {} — no-op (--force to rebuild)",
            installed.name, installed.git_url, installed.git_commit
        );
    } else {
        println!(
            "installed `{}` → {} (from-source, unverified; {} @ {})",
            installed.name,
            installed.program.display(),
            installed.git_url,
            installed.git_commit
        );
    }
    Ok(())
}

/// The production [`flux_plugin::pack::SourceBuilder`] (D-87): drives `git` and `cargo` through the
/// guarded [`System`] — argv-only, cleared + allow-listed env, **never** a shell string and never a
/// raw `std::process::Command`. `System::build_command` pins a spawn's cwd to the workspace root, so
/// the clean answer to "the clone lives outside the caller's workspace" is a **second `System`
/// rooted AT the clone directory** ([`Self::system_at`]) — no cwd override, one guarded process
/// path. Network steps (`git clone`/`fetch`, cargo's registry fetch + build) use
/// [`System::run_with_env_exempt`], the trusted-host exemption that skips only the OS child sandbox
/// while keeping argv-only + env-clear intact.
pub(super) struct SystemSourceBuilder;

impl SystemSourceBuilder {
    /// A `System` rooted at `dir` (which must already exist) so guarded git/cargo steps run with
    /// their cwd pinned there.
    fn system_at(dir: &std::path::Path) -> flux_core::Result<System> {
        System::from_env(dir)
            .map_err(|e| flux_core::Error::Other(format!("workspace at {}: {e}", dir.display())))
    }

    /// Run one guarded git/cargo step (network-exempt). A non-zero exit fails with a **trimmed**
    /// stderr tail — never a raw multi-screen cargo dump.
    async fn run_step(
        sys: &System,
        argv: &[String],
        timeout: std::time::Duration,
        ctx: &str,
    ) -> flux_core::Result<flux_system::ProcessOutput> {
        let out = sys.run_with_env_exempt(argv, &[], timeout).await?;
        if out.exit_code != 0 {
            return Err(flux_core::Error::Other(format!(
                "{ctx} failed (exit {}): {}",
                out.exit_code,
                last_lines(&out.stderr, 12)
            )));
        }
        Ok(out)
    }
}

/// Build an owned argv from string slices (the program is `parts[0]`).
pub(super) fn to_argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

/// The last `n` non-blank lines of `text`, joined — trims a cargo/git failure into an actionable
/// tail instead of dumping the whole build log.
pub(super) fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

pub(super) const GIT_STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
pub(super) const CARGO_METADATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
pub(super) const CARGO_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

#[async_trait::async_trait]
impl flux_plugin::pack::SourceBuilder for SystemSourceBuilder {
    async fn clone_and_resolve(
        &self,
        url: &str,
        git_ref: &flux_plugin::pack::GitRef,
        clone_dir: &std::path::Path,
    ) -> flux_core::Result<String> {
        use flux_plugin::pack::GitRef;
        std::fs::create_dir_all(clone_dir).map_err(flux_core::Error::Io)?;
        let sys = Self::system_at(clone_dir)?;

        let fresh = !clone_dir.join(".git").is_dir();
        if fresh {
            Self::run_step(
                &sys,
                &to_argv(&["git", "clone", "--quiet", url, "."]),
                GIT_STEP_TIMEOUT,
                "git clone",
            )
            .await?;
        } else {
            Self::run_step(
                &sys,
                &to_argv(&[
                    "git", "fetch", "--quiet", "--tags", "--force", "--prune", "origin",
                ]),
                GIT_STEP_TIMEOUT,
                "git fetch",
            )
            .await?;
        }

        // Detached-head checkouts (tag/rev) shouldn't print git's advice banner into stderr.
        match git_ref {
            GitRef::Tag(t) => {
                Self::run_step(
                    &sys,
                    &to_argv(&[
                        "git",
                        "-c",
                        "advice.detachedHead=false",
                        "checkout",
                        "--quiet",
                        t,
                    ]),
                    GIT_STEP_TIMEOUT,
                    &format!("git checkout tag `{t}`"),
                )
                .await?;
            }
            GitRef::Rev(r) => {
                Self::run_step(
                    &sys,
                    &to_argv(&[
                        "git",
                        "-c",
                        "advice.detachedHead=false",
                        "checkout",
                        "--quiet",
                        r,
                    ]),
                    GIT_STEP_TIMEOUT,
                    &format!("git checkout rev `{r}`"),
                )
                .await?;
            }
            GitRef::Branch(b) => {
                // Fetch the branch, then repoint a local branch at the remote head deterministically.
                Self::run_step(
                    &sys,
                    &to_argv(&["git", "fetch", "--quiet", "origin", b]),
                    GIT_STEP_TIMEOUT,
                    &format!("git fetch branch `{b}`"),
                )
                .await?;
                Self::run_step(
                    &sys,
                    &to_argv(&[
                        "git",
                        "checkout",
                        "--quiet",
                        "-B",
                        b,
                        &format!("origin/{b}"),
                    ]),
                    GIT_STEP_TIMEOUT,
                    &format!("git checkout branch `{b}`"),
                )
                .await?;
            }
            GitRef::Default => {
                // A cache hit refreshes the default branch to its remote head; a fresh clone is
                // already at it. Best-effort (a detached default is rare and left as-is).
                if !fresh {
                    let _ = Self::run_step(
                        &sys,
                        &to_argv(&["git", "reset", "--hard", "--quiet", "@{upstream}"]),
                        GIT_STEP_TIMEOUT,
                        "git reset to upstream",
                    )
                    .await;
                }
            }
        }

        let out = Self::run_step(
            &sys,
            &to_argv(&["git", "rev-parse", "HEAD"]),
            GIT_STEP_TIMEOUT,
            "git rev-parse HEAD",
        )
        .await?;
        let commit = out.stdout.trim().to_string();
        if commit.is_empty() {
            return Err(flux_core::Error::Other(
                "git rev-parse HEAD returned no commit".into(),
            ));
        }
        Ok(commit)
    }

    async fn build(
        &self,
        clone_dir: &std::path::Path,
        requested_bin: Option<&str>,
    ) -> flux_core::Result<flux_plugin::pack::BuiltPlugin> {
        let sys = Self::system_at(clone_dir)?;

        // Detect the flux-plugin bin target WITHOUT building (a clear, actionable error, not a raw
        // cargo dump). `--no-deps` reads only the workspace manifests → no dependency resolution.
        let meta = Self::run_step(
            &sys,
            &to_argv(&["cargo", "metadata", "--no-deps", "--format-version", "1"]),
            CARGO_METADATA_TIMEOUT,
            "cargo metadata",
        )
        .await
        .map_err(|e| {
            flux_core::Error::Other(format!(
                "`{}` is not a readable Rust project ({e}) — a flux plugin is a cargo crate with a \
                 `[[bin]] flux-plugin-<name>` target (see plugins/AUTHORING.md)",
                clone_dir.display()
            ))
        })?;
        let meta_json: serde_json::Value = serde_json::from_str(&meta.stdout)
            .map_err(|e| flux_core::Error::Other(format!("parse cargo metadata: {e}")))?;

        let mut bins: Vec<String> = Vec::new();
        if let Some(pkgs) = meta_json.get("packages").and_then(|p| p.as_array()) {
            for pkg in pkgs {
                let Some(targets) = pkg.get("targets").and_then(|t| t.as_array()) else {
                    continue;
                };
                for tgt in targets {
                    let is_bin = tgt
                        .get("kind")
                        .and_then(|k| k.as_array())
                        .is_some_and(|ks| ks.iter().any(|k| k.as_str() == Some("bin")));
                    let name = tgt.get("name").and_then(|n| n.as_str());
                    if let (true, Some(name)) = (is_bin, name) {
                        if name.starts_with("flux-plugin-") {
                            bins.push(name.to_string());
                        }
                    }
                }
            }
        }
        bins.sort();
        bins.dedup();

        let bin_name = match requested_bin {
            Some(req) => {
                let want_full = if req.starts_with("flux-plugin-") {
                    req.to_string()
                } else {
                    format!("flux-plugin-{req}")
                };
                if bins.contains(&want_full) {
                    want_full
                } else {
                    return Err(flux_core::Error::Other(format!(
                        "`--bin {req}` is not a flux-plugin bin target in this repo (found: {})",
                        if bins.is_empty() {
                            "none".to_string()
                        } else {
                            bins.join(", ")
                        }
                    )));
                }
            }
            None => match bins.as_slice() {
                [] => {
                    return Err(flux_core::Error::Other(format!(
                        "`{}` is not a flux plugin: no `flux-plugin-*` binary target found. A flux \
                         plugin is a cargo crate declaring a `[[bin]]` named `flux-plugin-<name>` \
                         (see plugins/AUTHORING.md)",
                        clone_dir.display()
                    )))
                }
                [only] => only.clone(),
                many => {
                    return Err(flux_core::Error::Other(format!(
                        "this repo has several flux-plugin bin targets ({}) — pick one with \
                         `--bin <name>`",
                        many.join(", ")
                    )))
                }
            },
        };

        Self::run_step(
            &sys,
            &to_argv(&[
                "cargo",
                "build",
                "--release",
                "--locked",
                "--bin",
                &bin_name,
            ]),
            CARGO_BUILD_TIMEOUT,
            &format!("cargo build --bin {bin_name}"),
        )
        .await?;

        let binary = clone_dir.join("target").join("release").join(&bin_name);
        if !binary.is_file() {
            return Err(flux_core::Error::Other(format!(
                "cargo build reported success but `{}` is missing",
                binary.display()
            )));
        }
        Ok(flux_plugin::pack::BuiltPlugin { bin_name, binary })
    }
}

/// Find every `flux-plugin-<name>` (or, on Windows, `flux-plugin-<name>.exe`) executable in `dir`,
/// returning `(name, absolute-program-path)` pairs sorted by name. Skips sidecar files (e.g.
/// `*.d`). Missing dir is an error (the caller reports).
pub(super) fn plugin_binaries_in(dir: &std::path::Path) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(rest) = file.strip_prefix("flux-plugin-") else {
            continue;
        };
        // `flux-plugin-<name>` with no further extension, or `flux-plugin-<name>.exe` on Windows —
        // anything else with a `.` is a sidecar (`*.d`, etc.) and is skipped.
        let name = match rest.strip_suffix(".exe") {
            Some(base) if !base.is_empty() && !base.contains('.') => base,
            Some(_) => continue,
            None if !rest.is_empty() && !rest.contains('.') => rest,
            None => continue,
        };
        let name = name.to_string(); // own it before `path` is moved below
        let program = path
            .canonicalize()
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        out.push((name, program));
    }
    out.sort();
    Ok(out)
}

/// Split discovered plugins into loadable descriptors and STALE registrations — an ABSOLUTE
/// recorded `program` whose binary is POSITIVELY confirmed absent (a deleted checkout, a pruned
/// pack store). Stale ones are skipped before any spawn attempt and reported by
/// [`warn_stale_plugins`] as ONE aggregated line, so a pile of dead descriptors doesn't print a
/// warning per plugin on every command. Anything this can't confirm absent defers to the spawn
/// (whose real error still gets its own detailed line): relative paths (they'd resolve against
/// whatever the CURRENT cwd is), bare PATH-resolved names, and stat errors (permissions, a
/// transient mount) — and on Windows a program recorded without `.exe` counts as present when the
/// `.exe` sibling exists (CreateProcess appends it).
pub(super) fn split_stale_plugins(
    discovered: Vec<flux_plugin::DiscoveredPlugin>,
) -> (Vec<flux_plugin::DiscoveredPlugin>, Vec<String>) {
    fn confirmed_absent(program: &str) -> bool {
        let prog = std::path::Path::new(program);
        if !prog.is_absolute() {
            return false;
        }
        let absent = |p: &std::path::Path| matches!(p.try_exists(), Ok(false));
        absent(prog) && (!cfg!(windows) || absent(&prog.with_extension("exe")))
    }
    let (loadable, stale): (Vec<_>, Vec<_>) = discovered
        .into_iter()
        .partition(|p| !confirmed_absent(&p.descriptor.program));
    (loadable, stale.into_iter().map(|p| p.name).collect())
}

/// One dim stderr line covering every stale plugin registration (empty → silence), with the
/// remedy: `flux plugin status <name>` shows the recorded (missing) path; rebuild/reinstall the
/// binary, or unregister the plugin.
pub(super) fn warn_stale_plugins(stale: &[String]) {
    if stale.is_empty() {
        return;
    }
    eprintln!(
        "{}",
        style::dim(&format!(
            "({} plugin registration(s) skipped — binary missing: {}; `flux plugin status <name>` shows the recorded path; rebuild/reinstall, or `flux plugin uninstall <name>` to unregister)",
            stale.len(),
            stale.join(", ")
        ))
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C-312: `flux plugin call` is an ingest surface too. The boundary is wired onto the same
    /// manifest declaration the projected-tool path reads, so an op the agent may not receive a
    /// credential from is one the operator may not print by hand either.
    ///
    /// A vendor token spelling joined at compile time (C-325), well past every length floor.
    #[test]
    fn plugin_call_applies_the_credential_boundary_to_a_platform_sourced_response() {
        let vendor = concat!("xoxb", "-3141592653-2718281828-abcdefghijklmnopqrstuvwx");
        let manifest = flux_plugin::PluginManifest {
            name: "connectors".into(),
            operations: vec![
                flux_plugin::OperationSpec {
                    name: "dispatch".into(),
                    platform: flux_plugin::PlatformSourcing::Operation,
                    ..Default::default()
                },
                flux_plugin::OperationSpec {
                    name: "whoami".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let leaky = serde_json::json!({ "audit": { "header": format!("Bearer {vendor}") } });

        let refusal = refuse_platform_response(&leaky, &manifest, "dispatch")
            .expect("a platform-sourced response carrying a credential must be refused");
        assert!(
            !refusal.contains(vendor),
            "the refusal quoted it: {refusal}"
        );
        // Scoped to the declaration: a local op of the same plugin is untouched.
        assert_eq!(refuse_platform_response(&leaky, &manifest, "whoami"), None);
        // And an ordinary payload of the platform-sourced op still prints.
        let clean = serde_json::json!({ "ticket": { "id": 4711, "status": "open" } });
        assert_eq!(
            refuse_platform_response(&clean, &manifest, "dispatch"),
            None
        );

        // The failure path is the same seam.
        let raw = format!("vendor said: token {vendor} expired");
        let scrubbed = scrub_plugin_error(&manifest, "dispatch", raw.clone());
        assert!(!scrubbed.contains(vendor), "{scrubbed}");
        assert_eq!(scrub_plugin_error(&manifest, "whoami", raw.clone()), raw);
    }

    /// C-312 rework: the boundary's **miss** branch refuses rather than skips.
    ///
    /// An op the manifest does not describe is unreachable on today's call path — `resolved_op` is
    /// resolved out of this same manifest a few lines earlier. That is an argument about the
    /// current caller, not about the function, and it is exactly the argument that stops being true
    /// after a refactor. A boundary whose "I could not tell" branch returns `None` fails open: the
    /// response prints, and nothing anywhere says the check did not run. So the unknown op is
    /// treated as maximally suspect — refused on the success path, and its error message discarded
    /// on the failure path — because an op with no declaration cannot be shown to be local.
    #[test]
    fn an_op_missing_from_the_manifest_is_refused_rather_than_skipped() {
        let manifest = flux_plugin::PluginManifest {
            name: "connectors".into(),
            operations: vec![flux_plugin::OperationSpec {
                name: "whoami".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        // Nothing credential-shaped in either payload: the refusal is about the *missing
        // declaration*, not about the content.
        let clean = serde_json::json!({ "ticket": { "id": 4711 } });
        let refusal = refuse_platform_response(&clean, &manifest, "ghost")
            .expect("an op absent from the manifest must be refused, not accepted");
        assert!(
            refusal.contains("ghost") && refusal.contains("connectors"),
            "the refusal must name the plugin and the op: {refusal}"
        );

        let raw = "vendor said: 401 Unauthorized".to_string();
        let scrubbed = scrub_plugin_error(&manifest, "ghost", raw.clone());
        assert_ne!(
            scrubbed, raw,
            "an absent op's error message must be discarded, not passed through"
        );
        assert!(scrubbed.contains("ghost"), "{scrubbed}");

        // The declared local op is still accepted — the refusal is scoped to the miss, and this is
        // the control that keeps it from being a closed path.
        assert_eq!(refuse_platform_response(&clean, &manifest, "whoami"), None);
        assert_eq!(scrub_plugin_error(&manifest, "whoami", raw.clone()), raw);
    }

    /// C-310: the operator-facing summary names every op that appeared and every one that was
    /// withdrawn — a count alone would not tell them whether the op they authenticated for is the
    /// one that showed up.
    #[test]
    fn refresh_report_names_the_catalog_delta() {
        let report = format_refresh_report(
            "connectors",
            &["connectors.zendesk.ticket.create".to_string()],
            &["connectors.placeholder".to_string()],
            &["connectors.whoami".to_string()],
            &[],
        );
        assert!(
            report.contains("1 added, 1 withdrawn, 1 unchanged"),
            "{report}"
        );
        assert!(report.contains("2 operation(s)"), "{report}");
        assert!(
            report.contains("+ connectors.zendesk.ticket.create"),
            "{report}"
        );
        assert!(report.contains("- connectors.placeholder"), "{report}");
        // A retained op is not noise in the delta — it is only counted.
        assert!(!report.contains("+ connectors.whoami"), "{report}");
    }

    /// A refresh that changed nothing says so rather than printing an empty delta.
    #[test]
    fn refresh_report_states_when_nothing_changed() {
        let report = format_refresh_report("drift", &[], &[], &["drift.alpha".to_string()], &[]);
        assert!(report.contains("no change (1 operation(s))"), "{report}");
    }

    /// C-191 warnings travel with the refreshed catalog exactly as they do at load: surfaced, not
    /// fatal.
    #[test]
    fn refresh_report_surfaces_coherence_warnings() {
        let report = format_refresh_report(
            "drift",
            &["drift.delta".to_string()],
            &[],
            &[],
            &["I2 (destructive floor): `drift.delta` declares …".to_string()],
        );
        assert!(report.contains("incoherent metadata"), "{report}");
        assert!(report.contains("I2 (destructive floor)"), "{report}");
    }

    /// D-190: `write_generated_skill` is the only place `references/` are written for a `flux skill
    /// … --install` skill. Prove the round trip end to end — generate a `flux-plugin` skill with a
    /// real reference (mirroring `plugin_skill::tests::fixture`), install it into a project
    /// `.flux/skills` root, discover it through the same production path the engine uses
    /// (`flux_runtime::metadata::discover_skills`), and confirm the discovered skill's `source`
    /// resolves to a directory whose `references/<plugin>.md` is the file just written — i.e. the
    /// path D-190 discloses in the `<skill>` tag actually anchors a `read` of the generated
    /// reference.
    #[test]
    fn installed_plugin_skill_references_are_reachable_from_its_discovered_source() {
        let sequence = std::sync::atomic::AtomicU64::new(0);
        let n = sequence.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let project = std::env::temp_dir().join(format!(
            "flux-skill-round-trip-{}-{n}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&project).unwrap();

        let manifest = flux_plugin::PluginManifest {
            name: "gitlab".into(),
            version: "0.1.0".into(),
            operations: vec![flux_plugin::OperationSpec {
                name: "gitlab.project.list".into(),
                description: "List projects".into(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rendered = skill_cmd::render_plugin_skill(&[("gitlab".into(), manifest)]);
        assert!(
            !rendered.references.is_empty(),
            "fixture must actually exercise the references/ path"
        );

        let skills_root = project.join(".flux").join("skills");
        std::fs::create_dir_all(&skills_root).unwrap();
        let installed_dir = write_generated_skill(&skills_root, &rendered).unwrap();

        // C-393: pinned to an empty home, so the operator's `~/.claude/skills` cannot join (or
        // shadow) the round trip this test is measuring.
        let discovered = flux_runtime::metadata::discover_skills_in(
            &project,
            &[],
            &flux_runtime::metadata::DiscoveryEnv::empty(),
        )
        .unwrap()
        .skills;
        let skill = discovered
            .iter()
            .find(|s| s.name == "flux-plugin")
            .expect("the installed skill is discovered");
        let source = skill.source.as_ref().expect("discovery captures `source`");
        assert_eq!(
            source,
            &installed_dir.join("SKILL.md"),
            "source must point at the installed SKILL.md"
        );

        // Mirror flux-flow's disclosure rule (D-190): a SKILL.md-backed skill discloses its
        // directory, so `references/<name>.md` must be reachable directly beneath it.
        let disclosed_dir = source.parent().expect("SKILL.md has a parent directory");
        for (name, expected_md) in &rendered.references {
            let reference_path = disclosed_dir.join("references").join(format!("{name}.md"));
            let on_disk = std::fs::read_to_string(&reference_path).unwrap_or_else(|e| {
                panic!(
                    "reference {} not reachable at {reference_path:?}: {e}",
                    name
                )
            });
            assert_eq!(&on_disk, expected_md);
        }

        std::fs::remove_dir_all(&project).ok();
    }
}
