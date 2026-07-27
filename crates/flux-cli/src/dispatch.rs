use super::*;

/// Export the C-21 filesystem-access policy to `FLUX_ADD_DIRS` / `FLUX_ALLOW_ALL` from the CLI flags +
/// `[workspace]` config, so `Workspace::from_env` (used at every production construction site) picks it
/// up. Sources are **additive**: `--add-dir` flags, `[workspace] add_dirs`, and any pre-set `FLUX_ADD_DIRS`
/// all contribute; `--allow-all-paths`, `[workspace] allow_all`, or `FLUX_ALLOW_ALL` each enable the hatch.
pub(super) fn apply_workspace_access_env(cli: &Cli, cfg: &flux_config::Config) {
    let cwd = std::env::current_dir().unwrap_or_default();
    // Absolutize each dir against the cwd so downstream canonicalization is stable regardless of cwd.
    let abs = |p: &std::path::Path| -> String {
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        };
        full.to_string_lossy().into_owned()
    };
    let mut dirs: Vec<String> = Vec::new();
    if let Ok(existing) = std::env::var("FLUX_ADD_DIRS") {
        dirs.extend(
            existing
                .split(':')
                .filter(|s| !s.is_empty())
                .map(String::from),
        );
    }
    dirs.extend(cli.add_dir.iter().map(|p| abs(p)));
    dirs.extend(cfg.workspace_add_dirs().iter().map(|p| abs(p)));
    dirs.sort();
    dirs.dedup();
    if !dirs.is_empty() {
        std::env::set_var("FLUX_ADD_DIRS", dirs.join(":"));
    }

    // Name the source that actually disabled the sandbox, so the operator knows what to remove.
    let allow_all_source = if cli.allow_all_paths {
        Some("--allow-all-paths")
    } else if cfg.workspace_allow_all() {
        Some("[workspace] allow_all in .flux/config.toml")
    } else if flux_system::env_truthy("FLUX_ALLOW_ALL") {
        Some("FLUX_ALLOW_ALL")
    } else {
        None
    };
    if let Some(source) = allow_all_source {
        std::env::set_var("FLUX_ALLOW_ALL", "1");
        eprintln!(
            "{} filesystem sandbox disabled ({source}): the agent can read AND write anywhere \
             on disk",
            style::red("warning:")
        );
    }

    // Ephemeral private-network egress grant for this invocation (D-96). Exported so surfaces that do
    // not receive the `Cli` (e.g. `flux plugin call`, `app run`) observe the same override. A truthy
    // pre-set FLUX_ALLOW_PRIVATE_NET (e.g. inherited from a parent flux) gets the same warning — the
    // grant is live either way, and staying silent about open private-net egress is worse than
    // repeating the note in a child process.
    if cli.allow_private_net || private_net_cli_override() {
        let source = if cli.allow_private_net {
            "--allow-private-net"
        } else {
            "FLUX_ALLOW_PRIVATE_NET"
        };
        std::env::set_var("FLUX_ALLOW_PRIVATE_NET", "1");
        eprintln!(
            "{} private-network egress allowed for this run ({source}): plugins may reach \
             the private hosts their manifest declares, and web.fetch may reach any private/loopback \
             address (incl. cloud metadata). Prefer a scoped [private_net.plugins] grant for recurring use.",
            style::red("warning:")
        );
    }
}

/// Export the D-130 sandbox posture to `FLUX_SANDBOX` / `FLUX_SANDBOX_NET` / `FLUX_SANDBOX_WRITABLE`
/// from the CLI flags + `[sandbox]` config, so `Sandbox::resolve` (consulted by every
/// `System::from_env` production site) picks it up and child flux invocations (`app run`, eval
/// sub-agents, `plugin call`) inherit it — the same channel pattern as
/// [`apply_workspace_access_env`].
///
/// Posture is resolved **tightest-wins**, NOT by a precedence chain: the strictest of
/// `Require > On > Off` across every source that asks for confinement is what takes effect, so a
/// laxer source can never silently downgrade a stricter one. Sources: `--sandbox` contributes `On`;
/// a pre-set `FLUX_SANDBOX` contributes `Require`/`On` for those values (anything unrecognized —
/// empty string, a typo like `requird` — contributes NOTHING and, if non-empty, earns a warning,
/// rather than dropping to `Off`); config contributes `Require` when `[sandbox] require`, else `On`
/// when `[sandbox] enabled`. The one exception is the explicit kill switch — `--no-sandbox`, or a
/// pre-set `FLUX_SANDBOX=off` — which forces `Off` outright, mirroring `FLUX_OP_CACHE=off`. There is
/// no `--require-sandbox` flag; `require` comes only from config or `FLUX_SANDBOX=require`.
///
/// When the resolved mode isn't `off`, this also runs the startup preflight: `require` + no usable
/// backend is a hard startup error (fail-closed, mirroring `Sandbox::ensure_available`'s per-spawn
/// backstop); otherwise an unavailable backend prints ONE styled warning naming the reason, in the
/// same style as this function's `--allow-all-paths` warning above. A *nested* run (already confined
/// by an outer flux sandbox → `Backend::AlreadyConfined`) is neither: it satisfies `require` and is
/// not "unavailable", so no warning fires.
pub(super) fn apply_sandbox_env(cli: &Cli, cfg: &flux_config::Config) -> Result<()> {
    use flux_system::sandbox::SandboxMode;

    // Tightest-wins resolution: rank the postures so the strictest confinement request across every
    // source takes effect (`Off` = 0). A laxer source must never be able to downgrade a stricter one
    // (findings 6/7) — the sole override is the explicit kill switch handled below.
    fn rank(m: SandboxMode) -> u8 {
        match m {
            SandboxMode::Off => 0,
            SandboxMode::On => 1,
            SandboxMode::Require => 2,
        }
    }
    let stricter = |a: SandboxMode, b: SandboxMode| if rank(a) >= rank(b) { a } else { b };

    let preset = std::env::var("FLUX_SANDBOX").ok();
    let preset_lc = preset.as_deref().map(str::to_ascii_lowercase);
    // The explicit kill switch still wins outright (mirrors `FLUX_OP_CACHE=off`): `--no-sandbox`, or
    // a pre-set `FLUX_SANDBOX=off`, forces `Off` regardless of any confinement request.
    let explicit_off = cli.no_sandbox || preset_lc.as_deref() == Some("off");

    let mode = if explicit_off {
        SandboxMode::Off
    } else {
        let mut mode = SandboxMode::Off;
        // `--sandbox` asks for (at least) `On`.
        if cli.sandbox {
            mode = stricter(mode, SandboxMode::On);
        }
        // A pre-set env: recognized values raise the floor; `"off"` is the kill switch (handled
        // above); ANYTHING else (empty / typo) contributes NOTHING — it must never downgrade a
        // stricter source. A non-empty unrecognized value is almost certainly a typo, so warn.
        match preset_lc.as_deref() {
            Some("require") => mode = stricter(mode, SandboxMode::Require),
            Some("on") => mode = stricter(mode, SandboxMode::On),
            Some("off") | None => {}
            Some(other) => {
                if !other.is_empty() {
                    eprintln!(
                        "{} unrecognized FLUX_SANDBOX={:?} (expected off|on|require); ignoring it \
                         for sandbox posture resolution — set one of those values to change it.",
                        style::red("warning:"),
                        preset.as_deref().unwrap_or_default()
                    );
                }
            }
        }
        // Config: `require` (fail-closed) if set, else `enabled` (soft). `sandbox_require()` implies
        // `sandbox_enabled()`, so the `else if` is exact.
        if cfg.sandbox_require() {
            mode = stricter(mode, SandboxMode::Require);
        } else if cfg.sandbox_enabled() {
            mode = stricter(mode, SandboxMode::On);
        }
        mode
    };
    std::env::set_var(
        "FLUX_SANDBOX",
        match mode {
            SandboxMode::Off => "off",
            SandboxMode::On => "on",
            SandboxMode::Require => "require",
        },
    );

    // Network: a pre-set env wins over config; an explicit narrowing to closed is only ever
    // exported when it actually narrows (mirrors FLUX_ADD_DIRS/FLUX_ALLOW_ALL's "only set what
    // changes" style) — the default stays open with nothing exported.
    let network = std::env::var("FLUX_SANDBOX_NET")
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or_else(|| cfg.sandbox_network().unwrap_or(true));
    if !network {
        std::env::set_var("FLUX_SANDBOX_NET", "0");
    }

    // Writable extras: additive like FLUX_ADD_DIRS, absolutized against cwd.
    let cwd = std::env::current_dir().unwrap_or_default();
    let abs = |p: &std::path::Path| -> String {
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        };
        full.to_string_lossy().into_owned()
    };
    let mut writable: Vec<String> = Vec::new();
    if let Ok(existing) = std::env::var("FLUX_SANDBOX_WRITABLE") {
        writable.extend(
            existing
                .split(':')
                .filter(|s| !s.is_empty())
                .map(String::from),
        );
    }
    writable.extend(cfg.sandbox_writable().iter().map(|p| abs(p)));
    writable.sort();
    writable.dedup();
    if !writable.is_empty() {
        std::env::set_var("FLUX_SANDBOX_WRITABLE", writable.join(":"));
    }

    if mode == SandboxMode::Off {
        return Ok(());
    }

    let sandbox = resolved_sandbox();
    sandbox
        .ensure_available()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if !sandbox.is_active() {
        if let Some(reason) = sandbox.reason() {
            eprintln!(
                "{} OS sandbox requested but unavailable ({reason}): shell/plugin processes run \
                 WITHOUT OS-level confinement this run. Set `[sandbox] require = true` (or \
                 `FLUX_SANDBOX=require`) to fail closed instead.",
                style::red("warning:")
            );
        } else if sandbox.confined_by_parent() {
            // A nested flux run: an outer sandbox already confines this whole process tree, so this
            // process adds no wrapper of its own — that satisfies `require` and is NOT an
            // "unavailable" state, so the warning above (reason() == None here) rightly stays
            // silent. A one-line dim note just makes the inherited confinement legible.
            eprintln!(
                "{}",
                style::dim("sandbox: already confined by the outer flux run (nested).")
            );
        }
    }
    Ok(())
}

/// Whether `--allow-private-net` is in effect for this process. It is propagated as
/// `FLUX_ALLOW_PRIVATE_NET` by [`apply_workspace_access_env`], so surfaces that never receive the
/// [`Cli`] (notably `flux plugin call`) observe it too. Truthy-value semantics (not mere presence):
/// `FLUX_ALLOW_PRIVATE_NET=0` keeps private-net egress CLOSED — an SSRF-relevant grant must never
/// turn on because an operator set the variable to an explicit "off" value.
pub(super) fn private_net_cli_override() -> bool {
    flux_system::env_truthy("FLUX_ALLOW_PRIVATE_NET")
}

/// The per-plugin private-net host grant, widened to `*` when `--allow-private-net` is active. This
/// only widens the *operator grant* side; `SystemHostCaps::private_net_allow` still intersects it with
/// the plugin's manifest-declared `private_hosts`, so a plugin declaring none stays refused — the
/// deny-by-default envelope (D-20) is preserved, this is just an ephemeral grant equivalent to config.
pub(super) fn effective_plugin_private_hosts(cfg: &flux_config::Config, name: &str) -> Vec<String> {
    if private_net_cli_override() {
        vec!["*".to_string()]
    } else {
        cfg.plugin_private_hosts(name)
    }
}

/// The family-wide `web`-scope private-net host grant (native `flux-web` ops: `http.request`,
/// `web.fetch`, `browser.*`), widened to `*` when `--allow-private-net` is active.
pub(super) fn effective_web_private_hosts(cfg: &flux_config::Config) -> Vec<String> {
    if private_net_cli_override() {
        vec!["*".to_string()]
    } else {
        cfg.web_private_hosts()
    }
}

/// The `grant_source` recorded in a native-web `PrivateNetAdmit` audit: the CLI-flag label when
/// `--allow-private-net` is active, else the `web`-scope config source.
pub(super) fn web_grant_source() -> String {
    if private_net_cli_override() {
        "cli:--allow-private-net".to_string()
    } else {
        "config:web".to_string()
    }
}

/// The `grant_source` recorded in the `PrivateNetAdmit` audit for a plugin caller: the CLI-flag label
/// when `--allow-private-net` is active, else the normal per-plugin config source (`config:plugin/<name>`,
/// matching [`SystemHostCaps::with_manifest`]'s default).
pub(super) fn private_net_grant_source_for(name: &str) -> String {
    if private_net_cli_override() {
        "cli:--allow-private-net".to_string()
    } else {
        format!("config:plugin/{name}")
    }
}

/// Restore the default `SIGPIPE` disposition (`SIG_DFL`) that Rust's std overrides to `SIG_IGN` at
/// startup, so a broken pipe ends the process the conventional Unix way instead of panicking on EPIPE
/// (A-61 / F-006). Called once at the top of `main`.
#[cfg(unix)]
pub(super) fn reset_sigpipe() {
    // SAFETY: setting a signal disposition to SIG_DFL is a process-global libc call with no data race,
    // and SIG_DFL installs no handler, so there is no async-signal-safety concern.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// Sync entry point: everything that must happen BEFORE the tokio runtime exists lives here —
/// signal disposition, the rustls provider, clap, and every process-env export. `setenv` racing a
/// concurrent `getenv` (any worker thread resolving DNS or reading config) is undefined behavior
/// on glibc — the reason Rust 2024 marks `set_var` unsafe — so the env mutation happens while this
/// is still the only thread, and only then does the runtime spin up worker threads.
pub(super) fn run() -> Result<()> {
    // A-61 / F-006: Rust's std sets SIGPIPE to SIG_IGN at startup, so writing to a closed pipe returns
    // EPIPE and `println!`/`writeln!` panic ("failed printing to stdout: Broken pipe"). Piping a
    // streaming subcommand into `head`/`less`/`grep -q` is routine, so restore the default disposition
    // — the OS then ends the process the conventional Unix way on a broken pipe instead of a panic +
    // backtrace. Genuine write errors to a real file/terminal are unaffected.
    #[cfg(unix)]
    reset_sigpipe();
    // With the `slack` feature the dependency tree pulls rustls with BOTH crypto providers
    // (slack-morphism's hyper-rustls brings aws-lc-rs; reqwest/tungstenite bring ring), so rustls
    // cannot pick a process-level default on its own and panics on first TLS use. Install one
    // explicitly, once, before any TLS client (the Slack socket or a provider HTTP call) is created.
    #[cfg(feature = "slack")]
    {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
    // Install a colored error formatter so top-level anyhow errors use the same style as inline
    // `eprintln!("{} {e:#}", style::red("error:"))` calls rather than a bare `Error: …` line.
    // We do this before `style::init` so even parse errors (before color flags are known) get color
    // when stderr is a tty — safe because `style::init` defaults to auto.
    style::init(style::ColorChoice::Auto);
    // One clap parse handles every subcommand + `--help`/`-h`/`--version`/`help`. The top level carries
    // only `--color` (global) + the command list; the agent (turn) flags live on the agent-path
    // subcommands (`run`/`plan`/`tui`/`fork`/`app run`). With no subcommand, `flux` opens the REPL.
    let cli = Cli::parse();
    style::init(cli.color);
    // C-21: export the filesystem-access policy (extra read-only roots + the unconfined hatch) to the
    // environment so every workspace — including `app run` and subprocess paths — inherits it via
    // `Workspace::from_env`.
    // Load once, before exporting any config-derived policy. A malformed config is a hard startup
    // error: replacing it with `Config::default()` can erase a requested `[sandbox] require = true`
    // posture and let spawn-capable commands such as `plugin status` execute native code
    // unconfined. Clap handles `--help`/`--version` before this point, so those remain available even
    // when the project config needs repair.
    // D-179: `--store <DIR>` redirects the session store for this invocation. Exported here,
    // pre-runtime, for the same single-thread reason as the agent env signals below (`set_var` must
    // not race worker-thread `getenv`s).
    if let Some(dir) = cli.store.as_ref() {
        std::env::set_var("FLUX_STORE_DIR", dir);
    }
    let cwd = std::env::current_dir().context("resolve current directory")?;
    let cfg = flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
    apply_workspace_access_env(&cli, &cfg);
    // D-130: export the OS-sandbox posture the same way, then run the startup preflight (hard
    // error under `require` + unavailable; otherwise a one-line warning).
    apply_sandbox_env(&cli, &cfg)?;
    // The per-turn env signals (`FLUX_VERBOSE`/`FLUX_SHOW_LOOP`/`FLUX_TRACE_LOOP`) the agent-path
    // subcommands honor — exported here, pre-runtime, for the same single-thread reason.
    if let Some(flags) = cli.command.as_ref().and_then(Commands::agent_flags) {
        apply_agent_env(flags);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?
        .block_on(async_main(cli))
}

/// The async dispatch — runs on the runtime `main` builds after all env exports are done.
pub(super) async fn async_main(cli: Cli) -> Result<()> {
    let run = async {
        match cli.command {
            // The agent-path subcommands.
            Some(Commands::Run { agent, prompt }) => {
                // `flux run <app.flux>` runs a multi-agent program; `flux run <prompt…>` runs a turn.
                // Program mode keys on the `.flux` extension ONLY — matching any existing file would
                // hijack prompts that happen to start with a filename (`flux run Cargo.toml explain …`
                // must be a turn about Cargo.toml, not a parse of it as a Program).
                if prompt.first().is_some_and(|p| p.ends_with(".flux")) {
                    return run_app_cmd(prompt, &agent).await;
                }
                // `flux run` with no prompt drops into the REPL (with the given agent flags).
                if prompt.is_empty() {
                    return run_repl(agent).await;
                }
                run_prompt(agent, prompt).await
            }
            Some(Commands::Tui { agent }) => run_tui(agent).await,
            Some(Commands::Fork {
                session,
                at,
                inject,
                edit,
                replan,
                prompt,
                agent,
            }) => run_fork(&session, at, inject, edit, replan, prompt, &agent).await,
            // Non-agent subcommands.
            Some(Commands::A2a { url, prompt, token }) => run_a2a(url, prompt, token).await,
            Some(Commands::Eval {
                adapter,
                model,
                tasks,
                members,
                limit,
                trials,
                report,
                watch,
            }) => run_eval_cmd(adapter, tasks, members, limit, trials, report, watch, model).await,
            Some(Commands::App {
                action:
                    AppAction::Run {
                        agent,
                        program,
                        serve,
                    },
            }) => run_app(program.as_deref(), &agent, serve).await,
            Some(Commands::Flow {
                action: FlowAction::List,
            }) => run_flow_list(),
            Some(Commands::Flow {
                action:
                    FlowAction::Run {
                        target,
                        inputs,
                        args,
                        map_inputs,
                        model,
                        yes,
                        resumable,
                        resume,
                        resume_value,
                    },
            }) => {
                run_flow(
                    &target,
                    inputs,
                    args,
                    map_inputs,
                    model,
                    yes,
                    resumable,
                    resume,
                    resume_value,
                )
                .await
            }
            Some(Commands::Render { file, view, out }) => {
                run_render(&file, view, out.as_deref()).await
            }
            Some(Commands::Review {
                flags,
                files,
                format,
                fail_on,
            }) => run_review(&flags, files, format, fail_on).await,
            Some(Commands::Loop { action }) => run_loop_cmd(action).await,
            Some(Commands::Sessions { prune }) => run_sessions(prune),
            Some(Commands::Usage(args)) => run_usage(args),
            Some(Commands::Replay {
                session,
                turn,
                sub_agents,
                json,
            }) => run_replay(&session, turn.map(|t| t as usize), sub_agents, json).await,
            Some(Commands::Record {
                name,
                prompt,
                dir,
                agent,
            }) => run_record(&name, prompt, dir, &agent).await,
            Some(Commands::Test { name, dir, json }) => run_test(name, dir, json).await,
            Some(Commands::Diff { a, b, json }) => run_diff_cmd(&a, &b, json),
            Some(Commands::Auth { action }) => run_auth(action).await,
            Some(Commands::Plugin { action }) => run_plugin(action).await,
            Some(Commands::Endpoint { action }) => run_endpoint(action),
            Some(Commands::Skill {
                type_,
                install,
                global,
            }) => run_skill(type_, install, global).await,
            Some(Commands::Completion { shell }) => run_completion(shell),
            Some(Commands::Changelog {
                version,
                all,
                unreleased,
            }) => changelog::run(version.as_deref(), all, unreleased),
            Some(Commands::Preset { args }) => preset::run_preset(&args).await,
            // No subcommand → interactive REPL (the one implicit entry point).
            None => run_repl(AgentFlags::from_model_yes(None, false)).await,
        }
    };
    if let Err(e) = run.await {
        eprintln!("{} {e:#}", style::red("error:"));
        std::process::exit(1);
    }
    Ok(())
}

/// Export the per-turn env signals (`FLUX_VERBOSE`, `FLUX_SHOW_LOOP`, `FLUX_TRACE_LOOP`) the
/// agent-path subcommands honor.
pub(super) fn apply_agent_env(flags: &AgentFlags) {
    if flags.verbose {
        std::env::set_var("FLUX_VERBOSE", "1");
    }
    if flags.show_loop {
        std::env::set_var("FLUX_SHOW_LOOP", "1");
    }
    if flags.trace_loop {
        std::env::set_var("FLUX_TRACE_LOOP", "1");
    }
}

/// `flux completion <shell>` — print a shell completion script to stdout and exit. Pure output, no
/// side effects: a shell sources this as you type, so it must never touch the network or start a
/// turn. The shell is a clap `ValueEnum` (bash/elvish/fish/powershell/zsh), so an unknown value is
/// rejected at parse time; defaults to fish.
pub(super) fn run_completion(shell: Option<clap_complete::Shell>) -> Result<()> {
    use clap::CommandFactory;
    let shell = shell.unwrap_or(clap_complete::Shell::Fish);
    clap_complete::generate(shell, &mut Cli::command(), "flux", &mut std::io::stdout());
    Ok(())
}
