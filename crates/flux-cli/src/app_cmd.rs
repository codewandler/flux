use super::*;

/// C-126: how often the built-in coding agent's `flux app run --serve` daemon nudges
/// [`flux_events::EventStore::checkpoint`] — see the wiring in [`run_app`]. Periodic rather than
/// true idle-detection: simpler, and a `TRUNCATE` checkpoint attempt is already a non-blocking,
/// non-erroring no-op when there is nothing safe to reclaim (busy → skip), so ticking during
/// active traffic costs nothing beyond the attempt itself.
const WAL_CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// `flux run <app.flux>` — load and run a multi-agent flux **Program** through the `flux-app` host
/// (event bus + triggers + journeys). A bare single-flow file is accepted too. The provider is
/// best-effort: a program built only from pure ops runs without credentials; model-backed ops need a
/// resolvable `provider/model` (defaulting like the prompt path) and degrade with a clear note.
pub(super) async fn run_app_cmd(prompt: Vec<String>, flags: &AgentFlags) -> Result<()> {
    // The `.flux` path is the first token; `-m`/`--yes` were parsed as global flags.
    let path = prompt
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("usage: flux run <app.flux> [-m provider/model] [--yes]"))?;
    // A program takes no trailing words — dropping them silently would swallow what the user
    // clearly meant to pass (`flux run app.flux with these inputs`).
    if prompt.len() > 1 {
        bail!(
            "`flux run {path}` runs the program and takes no further arguments (got: {}) — to run \
             a prompt that starts with a `.flux` filename, quote the whole prompt",
            prompt[1..].join(" ")
        );
    }
    run_app(Some(path), flags, None).await
}

/// A [`flux_plugin::SystemSource`] over the session's [`flux_runtime::WorkspaceContext`] (C-122):
/// plugin host capabilities resolve the guarded system through the same handle the worktree tools
/// drive, so `process.run`/`process.spawn` from a plugin execute in the context's *active* root.
/// Lives at the surface because flux-plugin deliberately has no runtime dependency.
pub(super) struct WorkspaceSystemSource(pub(super) flux_runtime::WorkspaceContext);

impl flux_plugin::SystemSource for WorkspaceSystemSource {
    fn system(&self) -> Arc<System> {
        self.0.active()
    }
}

/// Build one plugin's [`HostCapabilities`](flux_plugin::HostCapabilities) for every CLI surface:
/// the guarded `System` + datasource bridge + endpoint-broker fan-out, with shared egress-audit,
/// cross-plugin resolver, and redactor-backed secret sink hooks. A resolved credential is
/// registered with `secret_sink` so it never appears raw in model-visible tool output, and a
/// private-net admission is recorded through `audit`.
#[allow(clippy::too_many_arguments)]
pub(super) fn integration_plugin_caps(
    system: Arc<dyn flux_plugin::SystemSource>,
    backend: Arc<dyn flux_capabilities::DatasourceBackend>,
    datasource_bridge: bool,
    manifest: &flux_plugin::PluginManifest,
    private_hosts: Vec<String>,
    resolver: Arc<dyn flux_plugin::ReferenceResolver>,
    audit: Arc<dyn flux_plugin::EgressAudit>,
    secret_sink: Arc<dyn flux_plugin::SecretSink>,
    broker: Arc<flux_capabilities::EndpointBroker>,
) -> Arc<dyn flux_plugin::HostCapabilities> {
    // Inject the broker as the resolver (ref-based IO + the `credential` capability) and the
    // redactor-backed secret sink BEFORE optionally adding the App-only datasource bridge and the
    // endpoint-broker host-caps. The flag preserves the pre-consolidation surface contract: plain
    // coding-agent plugins never had App datasource declarations to contribute into.
    let system_caps = flux_plugin::SystemHostCaps::from_source(system)
        .with_manifest(manifest)
        .with_private_net_grants(private_hosts)
        .with_grant_source(private_net_grant_source_for(&manifest.name))
        .with_egress_audit(audit)
        .with_resolver(resolver)
        .with_secret_sink(secret_sink);
    let inner: Arc<dyn flux_plugin::HostCapabilities> = if datasource_bridge {
        Arc::new(flux_capabilities::DatasourceHostCaps::new(
            system_caps,
            backend,
        ))
    } else {
        Arc::new(system_caps)
    };
    // Compose the endpoint broker OVER the datasource caps so this plugin's `endpoint.discover`
    // calls fan out (deny-by-default, gated by `discover`).
    Arc::new(flux_capabilities::EndpointBrokerHostCaps::new(
        inner,
        broker,
        manifest.name.clone(),
        manifest.capabilities.discover,
    )) as Arc<dyn flux_plugin::HostCapabilities>
}

/// Surface-neutral result of assembling endpoint/plugin integrations once.
pub(super) struct IntegrationAssembly {
    pub(super) tools: Vec<(String, Arc<dyn flux_runtime::Tool>)>,
    pub(super) groups: Vec<flux_evidence::ToolGroup>,
    pub(super) ambient_signals: Vec<String>,
}

/// Assemble the shared endpoint broker, datasource bridge, plugin host capabilities, audit sinks,
/// and redactor-backed secret sink for both the interactive agent and `flux app run`.
///
/// The caller owns policy decisions and the final registry. This function owns the mechanical
/// integration graph so the two CLI surfaces cannot drift into different resolver/audit/root
/// wiring. Every capability receives the same explicit guarded `system`, datasource backend,
/// event stream, and redactor.
#[allow(clippy::too_many_arguments)] // one assembly seam; every input is a distinct session object
pub(super) async fn assemble_integrations(
    system: Arc<System>,
    system_source: Arc<dyn flux_plugin::SystemSource>,
    backend: Arc<dyn flux_capabilities::DatasourceBackend>,
    datasource_bridge: bool,
    cfg: &flux_config::Config,
    events: Arc<EventStore>,
    stream: &str,
    redactor: &flux_secret::Redactor,
) -> Result<IntegrationAssembly> {
    let mut assembly = IntegrationAssembly {
        tools: Vec::new(),
        groups: Vec::new(),
        ambient_signals: Vec::new(),
    };
    let Some(dir) = plugins_dir() else {
        return Ok(assembly);
    };

    let plugin_registry = Arc::new(flux_capabilities::PluginRegistry::new());
    let endpoint_registry = Arc::new(flux_capabilities::EndpointRegistry::with_path(
        flux_capabilities::EndpointRegistry::default_path().unwrap_or_default(),
    ));
    if let Err(error) = endpoint_registry.load() {
        eprintln!(
            "{}",
            style::dim(&format!("(endpoints store not loaded: {error})"))
        );
    }
    merge_static_endpoints(&endpoint_registry, cfg);
    assembly.ambient_signals = session_ambient_signals(&endpoint_registry);

    let invoker = Arc::new(flux_capabilities::HostProviderInvoker::new(
        plugin_registry.clone(),
    ));
    let static_resolver = Arc::new(flux_capabilities::StaticResolver::new(
        system.clone(),
        endpoint_registry.config_bindings(),
    ));
    let cross_plugin_audit: Arc<dyn flux_capabilities::CrossPluginAudit> =
        Arc::new(EventStoreCrossPluginAudit {
            store: events.clone(),
            stream: stream.to_string(),
        });
    let broker = Arc::new(
        flux_capabilities::EndpointBroker::new(
            invoker,
            plugin_registry.clone(),
            endpoint_registry.clone(),
        )
        .with_static_resolver(static_resolver)
        .with_cross_plugin_grants(flux_capabilities::CrossPluginGrants::new(
            cfg.endpoint.cross_plugin_credentials.clone(),
        ))
        .with_cross_plugin_audit(cross_plugin_audit),
    );
    assembly.tools.extend(
        flux_capabilities::endpoint_tools(broker.clone(), endpoint_registry)
            .into_iter()
            .map(|tool| ("cli endpoint integration".to_string(), tool)),
    );

    let (plugins, stale) = split_stale_plugins(flux_plugin::discover(&dir));
    warn_stale_plugins(&stale);
    let loads: Vec<_> = plugins
        .into_iter()
        .map(|plugin| {
            let system = system.clone();
            let caps_system = system_source.clone();
            let backend = backend.clone();
            let cfg = cfg.clone();
            let broker_for_caps = broker.clone();
            let resolver = broker.clone() as Arc<dyn flux_plugin::ReferenceResolver>;
            let audit: Arc<dyn flux_plugin::EgressAudit> = Arc::new(EventStoreEgressAudit {
                store: events.clone(),
                stream: stream.to_string(),
            });
            let secret_sink = Arc::new(RedactorSecretSink {
                redactor: redactor.clone(),
            }) as Arc<dyn flux_plugin::SecretSink>;
            let make_caps = move |manifest: &flux_plugin::PluginManifest| {
                integration_plugin_caps(
                    caps_system,
                    backend,
                    datasource_bridge,
                    manifest,
                    effective_plugin_private_hosts(&cfg, &manifest.name),
                    resolver,
                    audit,
                    secret_sink,
                    broker_for_caps,
                )
            };
            async move {
                let name = plugin.name.clone();
                let loaded = flux_plugin::load_plugin_tools(
                    &system,
                    &plugin.name,
                    &plugin.descriptor,
                    make_caps,
                )
                .await;
                (name, loaded)
            }
        })
        .collect();
    let mut loaded_plugins = collect_bounded(loads, PLUGIN_LOAD_CONCURRENCY).await?;
    loaded_plugins.sort_by(|left, right| left.0.cmp(&right.0));
    for (plugin_name, loaded) in loaded_plugins {
        match loaded {
            Ok(loaded) => {
                plugin_registry.register(
                    loaded.manifest.name.clone(),
                    flux_capabilities::ProviderEntry {
                        manifest: Arc::new(loaded.manifest.clone()),
                        host: loaded.host.clone(),
                        caps: loaded.caps.clone(),
                    },
                );
                let specs: Vec<flux_spec::ToolSpec> =
                    loaded.tools.iter().map(|tool| tool.spec()).collect();
                assembly.groups.extend(loaded.manifest.groups.clone());
                if let Some(group) = implicit_plugin_group(&loaded.manifest, &specs) {
                    assembly.groups.push(group);
                }
                assembly.tools.extend(
                    loaded
                        .tools
                        .into_iter()
                        .map(|tool| (format!("plugin:{plugin_name}"), tool)),
                );
            }
            Err(error) => eprintln!(
                "{}",
                style::dim(&format!("(plugin `{plugin_name}` failed to load: {error})"))
            ),
        }
    }
    Ok(assembly)
}

/// Build and run a multi-agent program together with its declared **channels**, the shared body behind
/// both `flux run <app.flux>` (auto-detect) and `flux app run [program.flux]`. Cron/webhook/Slack
/// channels start as background tasks that deliver events into the program's bus (→ triggers → journeys)
/// until Ctrl-C; a program with a `cli` channel — or none at all — keeps the interactive stdin loop. By
/// default destructive ops are DENIED (no human at a prompt); `--yes` opts into allow-all. The provider
/// is best-effort: a pure-op program runs without credentials.
///
/// `serve` exposes an agent over the HTTP/A2A API. With a `path`, it adds a synthetic `a2a` channel
/// bound to the program's sole agent. With **no** `path`, it serves flux's built-in coding agent
/// directly — the former `flux serve` (requires `--yes`; non-loopback needs `FLUX_SERVER_TOKEN`).
/// Resolve the provider for a served/app program from a model spec. Honors `-m mock` the same way the
/// non-served CLI paths (`build_agent`/`provider_for`/REPL) do — A-60 / F-014: without the mock guard
/// `mock` falls into `build_provider`'s Anthropic short-alias arm, so `app run --serve -m mock`
/// silently used the Anthropic path (failing on low credits) instead of the offline mock. Returns the
/// provider (`None` if unbuildable, e.g. missing credentials — model-backed ops then unavailable) and
/// the resolved model label.
pub(super) fn app_provider_for(spec: &str) -> (Option<std::sync::Arc<dyn Provider>>, String) {
    if spec == "mock" || spec.starts_with("mock/") {
        return (
            Some(std::sync::Arc::new(MockCliProvider::default()) as std::sync::Arc<dyn Provider>),
            "mock".to_string(),
        );
    }
    match build_provider(spec) {
        Ok((native, _provider_name, resolved)) => (Some(std::sync::Arc::new(native)), resolved),
        Err(e) => {
            eprintln!(
                "{}",
                style::dim(&format!(
                    "(no provider for `{spec}`: {e}; model-backed cognition ops will be unavailable)"
                ))
            );
            let m = spec
                .split_once('/')
                .map(|(_, m)| m)
                .unwrap_or(spec)
                .to_string();
            (None, m)
        }
    }
}

pub(super) async fn run_app(
    path: Option<&str>,
    flags: &AgentFlags,
    serve: Option<String>,
) -> Result<()> {
    use flux_lang::program::{ChannelDecl, Module, Program};

    // No program + `--serve`: serve the built-in coding agent over HTTP/A2A (the old `flux serve`).
    let Some(path) = path else {
        let addr = serve.ok_or_else(|| {
            anyhow::anyhow!(
                "usage: flux app run <program.flux>  (or `flux app run --serve <addr>` to serve the \
                 built-in coding agent over HTTP/A2A)"
            )
        })?;
        if !flags.yes {
            bail!(
                "`flux app run --serve` (no program) requires `--yes` (HTTP requests have no \
                   interactive approver)"
            );
        }
        // The coding agent auto-approves every tool call, so an unauthenticated listener is remote code
        // execution. Require authentication for any non-loopback bind: per-request principal auth
        // when `[server] introspect_url` is configured (D-69), else a bearer token (`FLUX_SERVER_TOKEN`).
        let auth = server_auth_from_config()?;
        if matches!(auth, flux_server::ServerAuth::Open) && !addr_is_loopback(&addr) {
            bail!(
                "refusing to serve on a non-loopback address ({addr}) without authentication — set \
                 FLUX_SERVER_TOKEN to require `Authorization: Bearer <token>` (or configure \
                 `[server] introspect_url` for per-request principal auth), or bind 127.0.0.1"
            );
        }
        // C-183: this is the whole surface of the old `flux serve` — `build_agent` here is the exact
        // same call the interactive CLI path uses, so it already resolves + installs `[tools]
        // disable` (C-162's `build_agent_with`) with no separate wiring needed. `flux_server::serve`
        // just runs turns on the returned `agent`; it derives no second executor. Covered by C-162's
        // own tests (`resolve_disabled_*` in flux-runtime, `disabled_ops_*` in flux-flow, and the
        // real-binary `tools_disable_unmatched_entry_warns_at_startup` in flux-cli's mock_smoke.rs —
        // that test drives `flux run`, which shares this exact function).
        let (agent, _session_id, _spec, _spawner) = build_agent(flags).await?;
        // C-126: this is the ONE `flux app run --serve` shape that shares the persistent,
        // file-backed `~/.flux/events.db` with occasional CLI turns on the same host (design doc
        // R1's "daemon + occasional CLI turns" topology, C-25's scenario) — a long-lived process
        // that always holds read snapshots can defer SQLite's own WAL checkpoint indefinitely
        // (design doc `docs/designs/event-store-concurrent-use.md` §4.3), growing `events.db-wal`
        // without bound. Program-mode `app run` uses an in-memory `app_events` store (see
        // `run_app` below), so it has no sidecar to bound and gets no periodic task. The
        // interactive CLI is a short-lived process that already checkpoints on close — unaffected.
        let checkpoint_store = agent.events.clone();
        let checkpoint_task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(WAL_CHECKPOINT_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                // Never a turn-visible failure (R6/C-126): `checkpoint` itself already turns a
                // busy contended attempt into `Ok(())`, so an `Err` here means something ELSE
                // (a real I/O error) — worth a trace, never worth killing the daemon over.
                if let Err(e) = checkpoint_store.checkpoint() {
                    eprintln!("(WAL checkpoint attempt failed: {e})");
                }
            }
        });
        let result = flux_server::serve(&addr, agent, auth).await;
        checkpoint_task.abort();
        return result;
    };

    // Program mode runs the program's OWN agents: the built-in coding agent's session/turn flags
    // have nothing to attach to, so reject them instead of accepting-and-ignoring (they all work
    // on `flux run`/`flux tui` and on `app run --serve` without a program).
    if flags.continue_ || flags.resume {
        bail!("`flux app run <program>` starts the program fresh — `--continue`/`--resume` don't apply");
    }
    if flags.dev {
        bail!("`--dev` only applies to the built-in coding agent, not `flux app run <program>`");
    }
    if !flags.skill_dirs.is_empty() || !flags.skills.is_empty() {
        bail!(
            "`--skill`/`--skill-dir` only apply to the built-in coding agent, not `flux app run <program>`"
        );
    }
    if flags.turn_budget.is_some() {
        bail!("`--turn-budget` only applies to the built-in coding agent, not `flux app run <program>`");
    }
    if flags.max_model_calls.is_some() {
        bail!("`--max-model-calls` only applies to the built-in coding agent, not `flux app run <program>`");
    }
    if flags.max_iterations.is_some() {
        bail!("`--max-iterations` only applies to the built-in coding agent, not `flux app run <program>`");
    }
    if flags.agent_loop.is_some() {
        bail!("`--loop` only applies to the built-in coding agent, not `flux app run <program>`");
    }

    let auto_approve = flags.yes;
    // The bare `sonnet` alias, so the default model has ONE owner
    // (`flux_providers::anthropic::resolve_model`) — `app_provider_for` resolves it below.
    let spec = flags.model.clone().unwrap_or_else(|| "sonnet".to_string());
    let (provider, model) = app_provider_for(&spec);

    // `strict-review` is a built-in program name (no file): the L-13 `review_code` journey, wrapping
    // the ONE checked-in `examples/strict_review.flux` protocol as a composite op
    // (`flux_app::review::strict_review_program`) — the same construction the hermetic
    // `crates/flux-app/tests/strict_review_journey.rs` test drives. `flux review --files …` (the
    // direct/CLI surface) runs the identical embedded source through a different path
    // (`FlowClient::run_flow`), never a second hand-written copy.
    let is_builtin_strict_review = path == "strict-review";
    let mut program = if is_builtin_strict_review {
        flux_app::review::strict_review_program().map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        let src = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| anyhow::anyhow!("read program `{path}`: {e}"))?;
        match Module::parse_str(&src).map_err(|e| anyhow::anyhow!("{e}"))? {
            Module::Program(p) => p,
            Module::Flow(flow) => Program {
                flows: vec![flow],
                ..Default::default()
            },
        }
    };
    // Resolve `secret "ENV_NAME"` references in declaration settings from the environment (plaintext is
    // never inline) before any of those settings reach a channel/datasource/agent. Every resolved value
    // seeds the ONE redactor the app's journey + agent-target executors redact with (C-13), alongside
    // the known provider credential env vars.
    let redactor = flux_secret::Redactor::new();
    seed_provider_env_secrets(&redactor);
    flux_app::resolve_secrets(&mut program, &redactor).map_err(|e| anyhow::anyhow!("{e}"))?;

    // `--serve <addr>` injects a synthetic `a2a` channel bound to the program's sole agent, so the
    // serving path is identical to a declared `channel … { kind = "a2a" }`. An ambiguous (multi-agent)
    // or agent-less program must declare the channel explicitly instead.
    if let Some(addr) = &serve {
        let agent = match program.agents.as_slice() {
            [only] => only.name.clone(),
            [] => bail!("`--serve` needs an agent to serve, but `{path}` declares none"),
            _ => bail!(
                "`--serve` is ambiguous — `{path}` declares multiple agents; declare an `a2a` channel \
                 with an explicit `agent` instead"
            ),
        };
        let token = std::env::var("FLUX_SERVER_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());
        program.channels.push(ChannelDecl {
            name: "serve".to_string(),
            kind: "a2a".to_string(),
            settings: serde_json::json!({ "addr": addr, "agent": agent, "token": token }),
        });
    }

    // Assemble the knowledge + integration tools the program's agent target (`trigger.agent`) and its
    // journeys can drive — the D-09 registry wiring. A guarded `System` rooted at the cwd backs both.
    let cwd = std::env::current_dir()?;
    // A `datasource … path "./docs"` resolves against the PROGRAM FILE's directory, not the launch cwd,
    // so `flux app run <dir>/support-bot.flux` indexes the `./docs` shipped beside the program from ANY
    // working directory (`build_datasources` joins relative paths against this). `strict-review` is a
    // built-in with no file → fall back to cwd. We also register that directory as a read-only root so the
    // walk/read is permitted when the program lives OUTSIDE cwd; when it's under cwd (the in-repo case)
    // the primary root already covers it and this is a harmless duplicate.
    let program_dir = if is_builtin_strict_review {
        cwd.clone()
    } else {
        std::path::Path::new(path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| cwd.clone())
    };
    let mut workspace = Workspace::from_env(&cwd).map_err(|e| anyhow::anyhow!("{e}"))?;
    // A missing/invalid program dir is skipped, not fatal (mirrors `FLUX_ADD_DIRS`); a datasource that
    // then can't be read surfaces its own clear error below.
    let _ = workspace.add_read_root(&program_dir);
    let system = Arc::new(System::new(workspace).with_sandbox(resolved_sandbox()));
    // Scoped SSRF egress opt-in, off by default. Program-serving plugin hosts use per-plugin grants.
    // A *missing* config is fine (the safe default), but a *malformed* one is a hard error rather
    // than a silent `unwrap_or_default()` (finding 7): `app run` is a real workload whose security
    // (private-net grants, `[sandbox]` posture) is config-driven, so silently discarding a broken
    // config and running with an empty one is fail-open. This matches the `run`/`plan`/`tui` agent
    // paths, which already load the config with `?`. (The sandbox posture itself was already
    // resolved and exported at startup, so the `resolved_sandbox()` on the `System` above reflects it.)
    let cfg = flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
    // The knowledge datasource: build the program's declared datasources, and SHARE the backend so
    // integration plugins' contributed records (via the DatasourceHostCaps bridge) land in the same
    // index the `search`/`get`/`list`/`relation`/`batch_get`/`sources` ops read.
    let backend = build_datasources(&program.datasources, &program_dir, &system).await?;
    let mut extra_tools: Vec<(String, Arc<dyn flux_runtime::Tool>)> =
        flux_capabilities::datasource_tools(backend.clone())
            .into_iter()
            .map(|tool| ("app datasource integration".to_string(), tool))
            .collect();
    // The app-path event store + this run's stream identity (D-65): built here, BEFORE `App`, so the
    // plugin/endpoint wiring below can install the SAME audit/secret-sink hooks the `build_agent` path
    // installs (`with_egress_audit`/`with_cross_plugin_audit`/the credential secret sink) — then handed
    // to `App::with_events` further down so this wiring's audit trail lands in the SAME log as
    // everything else the app records (agent-target session memory, sub-agent spawn audit), rather than
    // a second, disconnected store.
    let app_events = Arc::new(
        EventStore::in_memory().map_err(|e| anyhow::anyhow!("app: in-memory event store: {e}"))?,
    );
    let app_run_stream = app_events
        .create_session(&model)
        .map_err(|e| anyhow::anyhow!("app: open run stream: {e}"))?;
    // C-122: one workspace handle for the whole app run, bound into BOTH the plugin caps (so
    // plugin ops follow a worktree transition) and the execution environment below.
    let app_workspace = flux_runtime::WorkspaceContext::new(system.clone());
    let integrations = assemble_integrations(
        system.clone(),
        Arc::new(WorkspaceSystemSource(app_workspace.clone())),
        backend,
        true,
        &cfg,
        app_events.clone(),
        &app_run_stream,
        &redactor,
    )
    .await?;
    extra_tools.extend(integrations.tools);

    let channel_decls = program.channels.clone();
    // The built-in `strict-review` program's `review_code` journey calls `strict_review`, which fans
    // out to reviewer sub-agents via `task` — the same `build_review_sub_agents` helper `flux review`
    // uses, so the two surfaces delegate through the identical envelope, never a re-derived one.
    let sub_agents = is_builtin_strict_review
        .then(|| build_review_sub_agents(&cwd, &spec, model.clone(), flags.max_tokens))
        .transpose()?;
    let mut integration_registry = ToolRegistry::new();
    for (source, tool) in extra_tools {
        integration_registry.try_register_from(source, tool)?;
    }
    let approver: Arc<dyn Approver> = if auto_approve {
        Arc::new(AllowApprover)
    } else {
        Arc::new(flux_runtime::DenyApprover)
    };
    let environment = ExecutionEnvironment::new(
        system,
        integration_registry,
        PermissionManager::new(),
        approver,
        ExecutionAuthorization::local(),
    )
    .with_workspace(app_workspace)
    .with_redactor(redactor);
    // C-183: same `[tools] disable` config C-162 wires into the interactive path — the raw
    // patterns are handed through so `flux_app::Engine` can resolve them (via
    // `ToolRegistry::resolve_disabled`, C-162's one implementation) once against its own
    // fully-assembled registry and install the result on every journey's and every per-agent
    // engine's executor.
    let app = std::sync::Arc::new(flux_app::App::try_with_execution_environment(
        program,
        provider,
        model,
        environment,
        sub_agents,
        app_events,
        flux_app::HostPermissionRules {
            allow: cfg.permissions.allow.clone(),
            deny: cfg.permissions.deny.clone(),
        },
        cfg.tools.disable.clone(),
    )?);
    let channels = flux_channels::build_channels(&channel_decls)?;
    // Serve stdin when an interactive `cli` channel is declared, or when the program declares no
    // channels at all (preserving the plain read-eval-print behavior).
    let run_stdin = channel_decls.is_empty() || channel_decls.iter().any(|c| c.kind == "cli");
    let cancel = tokio_util::sync::CancellationToken::new();
    flux_channels::serve(app, channels, run_stdin, cancel).await
}

/// Resolve the server's auth mode (D-69). `[server] introspect_url` in the layered config turns
/// on per-request principal auth (RFC 7662 introspection + caching); otherwise `FLUX_SERVER_TOKEN`
/// selects the shared-secret mode, and no configuration at all is the open, loopback-only mode.
/// The introspection client secret is sourced from the env var NAMED by
/// `introspect_client_secret_env` — the secret itself never lives in a config file.
pub(super) fn server_auth_from_config() -> Result<flux_server::ServerAuth> {
    let token = std::env::var("FLUX_SERVER_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let cwd = std::env::current_dir()?;
    let server = flux_runtime::metadata::load_config(&cwd)?.server;
    let Some(url) = server.introspect_url else {
        // Shared-secret (or open) mode. Advertise `[server] external_url` on the card when set, so
        // a non-loopback shared-secret deployment isn't exposed to Host-poisoning of its card.
        return Ok(flux_server::ServerAuth::shared_secret(
            token,
            server.external_url,
        ));
    };
    let external_url = server.external_url.ok_or_else(|| {
        anyhow::anyhow!(
            "[server] external_url is required with introspect_url — in principal mode the agent \
             card advertises where clients send bearer tokens, so it must come from config, never \
             the request's Host header"
        )
    })?;
    // The client secret is sourced from the env var NAMED by `introspect_client_secret_env` — the
    // secret itself never lives in a committed config file.
    let client = match (
        server.introspect_client_id,
        server.introspect_client_secret_env,
    ) {
        (Some(id), Some(env_name)) => {
            let secret = std::env::var(&env_name).map_err(|_| {
                anyhow::anyhow!("env var `{env_name}` (the introspection client secret) is not set")
            })?;
            Some((id, secret))
        }
        (Some(_), None) => anyhow::bail!(
            "[server] introspect_client_secret_env is required with introspect_client_id"
        ),
        (None, Some(_)) => anyhow::bail!(
            "[server] introspect_client_secret_env is set without introspect_client_id — the \
             client secret would be silently ignored; set introspect_client_id or remove it"
        ),
        (None, None) => None,
    };
    let auth = flux_server::PrincipalAuth::from_introspection(flux_server::IntrospectionParams {
        endpoint: url,
        client,
        allow_http: server.introspect_allow_http.unwrap_or(false),
        account_claim: server.introspect_account_claim,
        roles_claim: server.introspect_roles_claim,
        require_account: server.introspect_require_account.unwrap_or(false),
        external_url,
    })
    .map_err(|e| anyhow::anyhow!("[server] introspection config: {e}"))?;
    if token.is_some() {
        eprintln!(
            "(FLUX_SERVER_TOKEN ignored: `[server] introspect_url` enables per-request principal auth)"
        );
    }
    Ok(flux_server::ServerAuth::Principal(auth))
}

/// Whether `addr` (host:port or bare host) binds only the loopback interface.
pub(super) fn addr_is_loopback(addr: &str) -> bool {
    use std::net::{IpAddr, SocketAddr};
    if let Ok(sa) = addr.parse::<SocketAddr>() {
        return sa.ip().is_loopback();
    }
    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

/// Launch the ratatui chat TUI. The TUI installs its own modal approver unless `--yes` was passed,
/// in which case all tool calls are auto-approved (no modal).
pub(super) struct CliTuiModelResolver;

impl flux_tui::ModelResolver for CliTuiModelResolver {
    fn resolve(&self, spec: &str) -> anyhow::Result<flux_tui::ResolvedModel> {
        if spec == "mock" || spec.starts_with("mock/") {
            return Ok(flux_tui::ResolvedModel {
                provider: Arc::new(MockCliProvider::default()),
                wire_model: "mock".into(),
                model_spec: "mock".into(),
            });
        }
        let (provider, provider_name, model) = build_provider(spec)?;
        Ok(flux_tui::ResolvedModel {
            provider: Arc::new(provider),
            wire_model: model.clone(),
            model_spec: format!("{provider_name}/{model}"),
        })
    }
}

/// Built-in TUI slash commands (D-186): a file command sharing one of these names is dropped at
/// load (with a warning) rather than shadowing it — mirrors `flux-tui`'s `BUILTIN_COMMANDS` names.
const TUI_BUILTIN_COMMANDS: &[&str] = &[
    "help", "usage", "clear", "new", "model", "effort", "quit", "exit", "compact", "shell",
    "tools", "evidence", "session", "sessions", "resume", "queue",
];

pub(super) async fn run_tui(flags: AgentFlags) -> Result<()> {
    let auto_approve = flags.yes;
    let (agent, session_id, model_spec, _spawner) = build_agent(&flags).await?;
    let initial_rules = agent.executor.allow_rules();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut options = flux_tui::TuiRunOptions::new(auto_approve, Some(model_spec));
    options.model_resolver = Some(Arc::new(CliTuiModelResolver));
    options.file_commands = load_command_files(&cwd, TUI_BUILTIN_COMMANDS);
    // C-104: the persisted theme choice (user-level, project override wins per the merge rules).
    options.theme = flux_runtime::metadata::load_config(&cwd)
        .ok()
        .and_then(|cfg| cfg.theme);
    // Persist even when the TUI returns an error: an earlier "always allow" choice remains a user
    // decision and must not vanish because terminal restoration or a later turn failed.
    let executor = agent.executor.clone();
    let result = flux_tui::run_with_options(agent, session_id, options).await;
    persist_new_rules(&initial_rules, &executor.allow_rules());
    result
}
