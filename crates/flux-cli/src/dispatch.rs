use super::*;

/// Whether a `flux run …` invocation hands off to the multi-agent **program** host
/// (`run_app_cmd` → `run_app`) instead of running a turn.
///
/// `flux run <app.flux>` and `flux app run <program>` are the same daemon reached by two spellings,
/// so [`unattended_sandbox_surface`] has to classify them alike — and the only thing that decides
/// which one `flux run` is happens to be this predicate. [`async_main`] routes on the very same
/// call, so the classifier cannot come to a different conclusion than the router: they are one
/// function. Splitting them back into two copies of `ends_with(".flux")` is how the daemon would
/// quietly slip back out of the floor.
pub(super) fn run_targets_a_program(
    entry: Option<&String>,
    stream_json: bool,
    stream_json_input: bool,
    prompt: &[String],
) -> bool {
    // `--entry` selects a named flow; both machine-output modes reject a `.flux` argument outright
    // (see `async_main`), so neither ever reaches program mode.
    entry.is_none()
        && !stream_json
        && !stream_json_input
        && prompt.first().is_some_and(|p| p.ends_with(".flux"))
}

/// Why this invocation must use the fail-closed unattended sandbox profile — and, for every
/// subcommand that must not, why not.
///
/// The profile covers work with **no human approval boundary to fall back on**: auto-approved
/// turns, serving listeners, and direct headless invocation. Everything else retains the
/// operator-visible `off`/`on`/`require` contract.
///
/// **The match is deliberately exhaustive — do not add a `_` arm.** The defect C-410 removed was a
/// hand-maintained enumeration silently drifting from `Commands`: a `_ => None` fallback classified
/// `flux plugin call` (a headless plugin invocation with no approver and no dispatcher) as
/// interactive, purely because nobody had written an arm for it. Without a wildcard, a new
/// subcommand is a *compile error* until someone decides which side of the floor it belongs on, and
/// flux-codegate's `the_unattended_classifier_covers_every_commands_variant` fails if the wildcard
/// comes back or a variant stops being named here.
pub(super) fn unattended_sandbox_surface(cli: &Cli) -> Option<&'static str> {
    match cli.command.as_ref()? {
        // ---------------------------------------------------------------------------------------
        // Pinned to the fail-closed profile (C-262).
        // ---------------------------------------------------------------------------------------
        Commands::Run { agent, .. } if agent.yes => Some("auto-approved `flux run --yes`"),
        Commands::Fork { agent, .. } if agent.yes => Some("auto-approved `flux fork --yes`"),
        Commands::Record { agent, .. } if agent.yes => Some("auto-approved `flux record --yes`"),
        Commands::Flow {
            action: FlowAction::Run { yes: true, .. },
        } => Some("auto-approved `flux flow run --yes`"),
        Commands::App {
            action: AppAction::Run { serve: Some(_), .. },
        } => Some("HTTP/A2A serving surface"),
        Commands::App {
            action: AppAction::Run { agent, .. },
        } if agent.yes => Some("auto-approved `flux app run --yes`"),
        // C-410: **program mode is a daemon, flagged or not.** A `<program.flux>` runs until Ctrl-C
        // serving its declared channels, and cron / webhook / Slack triggers fire turns with no
        // operator attached — which is precisely C-262's criterion ("no human approval boundary to
        // fall back on"), and it does not become false because the run was started without `--yes`.
        //
        // An earlier draft of this story exempted the unflagged form on the grounds that it
        // installs `DenyApprover`. Review killed that, correctly, and the tree agrees twice over:
        // `run_app` calls `assemble_integrations` at startup, which **spawns every installed plugin
        // binary** before any journey runs and never consults an approver at all; and a program
        // that declares no capability policy is dispatched under `LEGACY_JOURNEY_ALLOW`
        // (flux-app's `app.rs`), whose eight pre-authorised ops return `PermDecision::Allow` and so
        // never reach the approver either (`approval_sensitive`, flux-runtime). A measured probe
        // put numbers on it: under the unflagged form a plugin subprocess reached the network
        // (`curl` exit 0) and wrote outside the workspace; under `--yes` the same spawn was refused
        // both (exit 6, write denied). Pinning removes that split.
        //
        // Both spellings land here, via one shared predicate — see [`run_targets_a_program`].
        Commands::App {
            action: AppAction::Run {
                program: Some(_), ..
            },
        } => Some("`flux app run <program>` channel daemon"),
        Commands::Run {
            entry,
            stream_json,
            stream_json_input,
            prompt,
            ..
        } if run_targets_a_program(entry.as_ref(), *stream_json, *stream_json_input, prompt) => {
            Some("`flux run <program.flux>` channel daemon")
        }
        // `flux app run` with neither a program nor `--serve` is a usage error: `run_app` bails
        // with the usage line before building anything. Nothing executes, so there is nothing to
        // confine — and failing it on a missing sandbox backend instead of on the real mistake
        // would tell the operator the wrong thing. (The exhaustiveness check found this arm; the
        // classification was not obvious from the enum.)
        Commands::App {
            action: AppAction::Run { program: None, .. },
        } => None,
        Commands::Preset { args }
            if args.iter().any(|arg| arg == "--run")
                && args.iter().any(|arg| arg == "--yes" || arg == "-y") =>
        {
            Some("auto-approved `flux preset --run --yes`")
        }
        Commands::Review { .. } => Some("auto-approved `flux review` strict-review flow"),
        Commands::System {
            action: SystemAction::Serve { .. },
        } => Some("remote execution-system serving surface"),
        // C-410: `flux plugin call <name> <op>` invokes a plugin operation directly — no
        // interactive approver, and (per this crate's own scoping rule) outside `Executor::dispatch`
        // entirely. It spawns exactly the native code an auto-approved turn's plugin tool call
        // spawns, so it inherits the same floor instead of running at the `Off` default.
        Commands::Plugin {
            action: Some(PluginAction::Call { .. }),
        } => Some("headless `flux plugin call`"),

        // ---------------------------------------------------------------------------------------
        // Explicitly exempt. Each group states what stands in for the floor.
        // ---------------------------------------------------------------------------------------
        // One operator, present for the whole run, answering a `StdinApprover` prompt per call
        // (`resolve_permissions`, execution.rs) — C-262's documented interactive contract, kept
        // deliberately: these are single foreground turns a human typed and is watching, not
        // daemons. The program-mode spellings of `run` and `app run` were lifted out above; what is
        // left of `Commands::Run` here is a prompt turn or the REPL.
        //
        // What this exemption does NOT claim: that nothing native runs unconfined. `build_agent_with`
        // (execution.rs) calls the same `assemble_integrations` `run_app` does, so an interactive
        // turn spawns every installed plugin binary at startup too, and a probe confirms those
        // children reach the network and write outside the workspace. That is the accepted cost of
        // C-262's interactive contract — plugin binaries are trusted dependencies (AGENTS.md), and
        // an operator is present — not an oversight this arm is papering over.
        Commands::Run { .. }
        | Commands::Fork { .. }
        | Commands::Record { .. }
        | Commands::Tui { .. }
        | Commands::Flow { .. }
        | Commands::Preset { .. } => None,
        // `flux eval` runs its suites by spawning child `flux … --yes` processes
        // (`flux-eval`'s `runner.rs`); each child re-enters this classifier on its own argv and
        // gets the floor there. Pinning the parent would confine the orchestrator, not the work.
        Commands::Eval { .. } => None,
        // A2A talks to a *remote* agent over HTTP: the turn, its tools and its spawns all happen on
        // the other side, under that deployment's own posture. There is no local execution to
        // confine.
        Commands::A2a { .. } => None,
        // The rest of `flux plugin …` is management, not operation invocation — that is the whole
        // of the reason, and the concession it carries is stated rather than buried: `status`,
        // `refresh` and `skill` DO spawn the plugin binary (`spawn_verified` /
        // `load_plugin_manifests`, plugin_cmd.rs), and those spawns stay unconfined.
        //
        // The line is drawn at what the spawn is *for*. Those three send one protocol-defined
        // `manifest` request and read the answer; `call` dispatches an arbitrary declared operation,
        // with whatever egress, process runs and writes the plugin does on the operator's behalf,
        // and it does so outside `Executor::dispatch` entirely. Confining the manifest read would
        // additionally make plugin inspection impossible on a host with no backend — `flux doctor`
        // is then the only thing left that can report on plugins, and it does not spawn them.
        //
        // Deliberately bounded, not resolved: a plugin binary is a trusted dependency (AGENTS.md),
        // so a hostile one is outside this envelope's threat model whichever subcommand starts it.
        // If that assumption is ever revisited, these three spawns are where to look first.
        Commands::Plugin { .. } => None,
        // Hermetic replay of an already-recorded world: no model call, no live IO, side effects
        // never re-fired (`flux replay`), and `flux test` re-runs the real agent against the
        // cassette under a deny-all approver and a never-called provider.
        Commands::Replay { .. } | Commands::Test { .. } => None,
        // Operator-facing reads, reports and local file writes. None of them starts a turn, none is
        // reachable from a model, and each runs in the foreground on argv the operator typed —
        // there is no autonomous execution here for the profile to bound.
        Commands::Render { .. }
        | Commands::Loop { .. }
        | Commands::Sessions { .. }
        | Commands::Wakeups { .. }
        | Commands::Usage(..)
        | Commands::Diff { .. }
        | Commands::Export { .. }
        | Commands::Auth { .. }
        | Commands::Endpoint { .. }
        | Commands::Policy { .. }
        | Commands::Catalog { .. }
        | Commands::Skill { .. }
        | Commands::Changelog { .. }
        | Commands::Docs { .. }
        | Commands::Completion { .. }
        | Commands::Doctor { .. } => None,
        // Read-only prompt-provenance inspection; no provider, plugin, or operation is invoked.
        Commands::Context { .. } => None,
        // One foreground, tool-free provider request over already-redacted local facts. No agent
        // turn, plugin, operation dispatch, or process launch exists for the OS sandbox to bound.
        Commands::Insights { .. } => None,
    }
}

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
/// when `[sandbox] enabled`. Auto-approved noninteractive and serving surfaces contribute
/// `Require` automatically. The one exception is the explicit kill switch — `--no-sandbox`, or a
/// pre-set `FLUX_SANDBOX=off` — which forces `Off` outright, mirroring `FLUX_OP_CACHE=off`. There is
/// no `--require-sandbox` flag; outside the unattended profile, `require` comes only from config or
/// `FLUX_SANDBOX=require`.
///
/// When the resolved mode isn't `off`, this also runs the startup preflight: `require` + no usable
/// backend is a hard startup error (fail-closed, mirroring `Sandbox::ensure_available`'s per-spawn
/// backstop); otherwise an `on`-mode run with no usable backend emits its **resolved-posture
/// disclosure** (C-217) — ONE styled stderr line naming the posture that actually took effect
/// (running unconfined) and the reason, in the same style as this function's `--allow-all-paths`
/// warning above. The line is composed and latched once-per-process at L2 by
/// `Sandbox::take_posture_disclosure`. A *nested* run (already confined by an outer flux sandbox →
/// `Backend::AlreadyConfined`) satisfies `require`, but the marker is only an assertion from the
/// parent environment, not independently verifiable here. It therefore emits a prominent audit
/// warning naming that trust decision instead of silently becoming a second escape hatch.
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
    let unattended = unattended_sandbox_surface(cli);
    // The explicit kill switch still wins outright (mirrors `FLUX_OP_CACHE=off`): `--no-sandbox`, or
    // a pre-set `FLUX_SANDBOX=off`, forces `Off` regardless of any confinement request.
    let explicit_off = cli.no_sandbox || preset_lc.as_deref() == Some("off");

    let mode = if explicit_off {
        SandboxMode::Off
    } else {
        // C-262: unattended/auto-approved execution starts at Require. Every other source may
        // tighten that posture but cannot silently soften it.
        let mut mode = if unattended.is_some() {
            SandboxMode::Require
        } else {
            SandboxMode::Off
        };
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

    if let Some(surface) = unattended.filter(|_| explicit_off) {
        let source = if cli.no_sandbox {
            "--no-sandbox"
        } else {
            "FLUX_SANDBOX=off"
        };
        eprintln!(
            "{} unattended sandbox profile BYPASSED by {source}: {surface} is running UNCONFINED. \
             Sandbox network controls cannot apply; provide equivalent isolation in an outer \
             container/VM and retain this startup line in operator audit logs.",
            style::red("warning:")
        );
    }

    // Network: unattended confinement defaults CLOSED. An exact truthy env or explicit
    // `[sandbox] network = true` may open it; unknown env values narrow to closed, never widen.
    // Interactive/local operation retains the pre-C-262 unrestricted default.
    let network_env = std::env::var("FLUX_SANDBOX_NET").ok();
    let network = network_env
        .as_deref()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or_else(|| {
            cfg.sandbox_network()
                .unwrap_or_else(|| unattended.is_none())
        });
    if unattended.is_some() {
        std::env::set_var("FLUX_SANDBOX_NET", if network { "1" } else { "0" });
    } else if !network {
        std::env::set_var("FLUX_SANDBOX_NET", "0");
    }
    if let Some(surface) = unattended.filter(|_| network && !explicit_off) {
        let source = if network_env.is_some() {
            "FLUX_SANDBOX_NET"
        } else {
            "[sandbox] network = true"
        };
        eprintln!(
            "{} unattended sandbox network opened explicitly by {source} for {surface}; spawned \
             processes are confined but may reach the network.",
            style::red("warning:")
        );
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
    sandbox.ensure_available().map_err(|e| {
        if let Some(surface) = unattended {
            anyhow::anyhow!(
                "unattended sandbox profile refused to start {surface}: {e}. Install a supported \
                 sandbox backend, or run flux inside an outer container/VM that provides equivalent \
                 filesystem and network isolation. To accept unconfined operation explicitly, use \
                 --no-sandbox (recorded as a prominent startup warning)."
            )
        } else {
            anyhow::anyhow!("{e}")
        }
    })?;
    if !sandbox.is_active() {
        // C-217: the resolved-posture disclosure. The line itself is composed at L2, next to the
        // facts that determine it (`Sandbox::posture_disclosure`), and latched there to once per
        // process; this surface decides only *where* it goes.
        //
        // It goes to stderr, unconditionally — including under `--stream-json`/`--json`. That is the
        // channel this CLI already reserves for diagnostics precisely so stdout stays machine-
        // parseable ("the stream is `jq`-parseable with no filtering", see `--stream-json`'s docs),
        // so the disclosure structurally cannot corrupt a parse. It is deliberately NOT suppressed
        // under machine-readable modes: an unattended/daemon deployment is exactly the operator who
        // must not be left believing they are confined when they are not, and suppression would also
        // mean enumerating every per-subcommand `--json` flag — a list that silently rots. Eval runs
        // stay quiet on their own merits: nothing turns the sandbox on for them, and an `Off`
        // sandbox has nothing to disclose.
        if let Some(disclosure) = sandbox.take_posture_disclosure() {
            eprintln!("{} {disclosure}", style::red("warning:"));
        } else if sandbox.confined_by_parent() {
            // A nested flux run normally inherits this from a wrapper flux created. The marker is
            // still ambient process state, though, and this process cannot independently verify the
            // claimed container/VM boundary. Make accepting it a prominent, auditable trust event.
            eprintln!(
                "{} sandbox: trusting FLUX_SANDBOXED=1 as an explicit OUTER-CONFINEMENT assertion. \
                 This process cannot verify the parent container/VM boundary; retain this startup \
                 line in operator audit logs.",
                style::red("warning:")
            );
        }
    } else if let Some(surface) = unattended {
        // C-410: the disclosure the *succeeding* case owed and never paid. Every other line in this
        // function fires when confinement is absent or was opted out of; a run that is genuinely
        // confined said nothing at all. That silence is what turns this profile into a support
        // problem: the first symptom an operator sees is a child process failing with
        // `curl: (6) Could not resolve host` or a refused write under `$HOME`, with nothing in the
        // output naming the sandbox as the cause. `--no-sandbox` gets a loud line for the opposite
        // choice, so the confined side should not be the quiet one.
        //
        // Both narrowings are named because both bite: the network is CLOSED by default here, and
        // writes are limited to the workspace, `$TMPDIR` and the toolchain caches — so a plugin
        // that keeps state in `~/.config/<vendor>` is refused. stderr, once per process, same
        // channel and same reasoning as the C-217 disclosure above.
        eprintln!(
            "{} sandbox: {surface} is CONFINED — spawned processes have network {} and may write \
             only to the workspace, $TMPDIR and toolchain caches. Open the network with \
             `[sandbox] network = true` (or FLUX_SANDBOX_NET=1), widen writes with \
             `[sandbox] writable`, or opt out with --no-sandbox.",
            style::dim("note:"),
            if network { "OPEN" } else { "CLOSED" }
        );
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
    let cwd = std::env::current_dir().context("resolve current directory")?;
    // D-179: `--store <DIR>` redirects the session store for this invocation. Exported here,
    // pre-runtime, for the same single-thread reason as the agent env signals below (`set_var` must
    // not race worker-thread `getenv`s).
    //
    // D-185: absolutize a relative `--store` against THIS process's cwd before exporting it — a
    // subprocess (`app run`, a plugin) can have a different cwd, and a bare relative path would
    // then resolve against that different directory instead of the one the user actually meant.
    // Only when the flag is given: with no `--store`, `FLUX_STORE_DIR` is left untouched so a
    // user-set env var (or none at all) is never clobbered.
    if let Some(dir) = cli.store.as_ref() {
        let abs = if dir.is_absolute() {
            dir.clone()
        } else {
            cwd.join(dir)
        };
        std::env::set_var("FLUX_STORE_DIR", abs);
    }
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
            Some(Commands::Run {
                agent,
                stream_json,
                stream_json_input,
                entry,
                inputs,
                args,
                prompt,
            }) => {
                if let Some(entry) = entry {
                    let target = named_entry_target(&prompt, &entry)?;
                    return run_flow_entry(target, &entry, inputs, args, &agent).await;
                }
                // C-160: the machine-output modes are Run-only and don't compose with the
                // multi-agent-program path — `flux app run` already has its own JSON-shaped
                // surfaces (`--serve`); a single line protocol over one adaptive turn doesn't map
                // onto a Program's event bus.
                if stream_json_input {
                    if prompt.first().is_some_and(|p| p.ends_with(".flux")) {
                        bail!("`--stream-json-input` is not supported for `flux run <app.flux>` programs");
                    }
                    return run_stream_json_conversation(agent, prompt).await;
                }
                if stream_json {
                    if prompt.first().is_some_and(|p| p.ends_with(".flux")) {
                        bail!(
                            "`--stream-json` is not supported for `flux run <app.flux>` programs"
                        );
                    }
                    if prompt.is_empty() {
                        bail!(
                            "`--stream-json` needs a prompt (use `--stream-json-input` to read the \
                             first turn from stdin instead)"
                        );
                    }
                    return run_stream_json(agent, prompt).await;
                }
                // `flux run <app.flux>` runs a multi-agent program; `flux run <prompt…>` runs a turn.
                // Program mode keys on the `.flux` extension ONLY — matching any existing file would
                // hijack prompts that happen to start with a filename (`flux run Cargo.toml explain …`
                // must be a turn about Cargo.toml, not a parse of it as a Program).
                //
                // `entry`/`stream_json`/`stream_json_input` are already `None`/false by the time
                // control reaches here (each returned or bailed above), so passing them to the
                // shared predicate changes nothing — it is spelled this way so that the sandbox
                // classifier and this router are literally the same decision (C-410).
                if run_targets_a_program(None, false, false, &prompt) {
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
                        remote_approval,
                    },
            }) => run_app(program.as_deref(), &agent, serve, remote_approval).await,
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
                progress,
                fail_on,
            }) => run_review(&flags, files, format, progress, fail_on).await,
            Some(Commands::Loop { action }) => run_loop_cmd(action).await,
            Some(Commands::Sessions {
                prune,
                query,
                file,
                since,
                until,
            }) => run_sessions(prune, query, file, since, until),
            Some(Commands::Wakeups { action }) => run_wakeups(action),
            Some(Commands::Usage(args)) => run_usage(args),
            Some(Commands::Insights { model }) => run_insights(model).await,
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
            Some(Commands::Export { run, out }) => run_export(&run, out.as_deref()),
            Some(Commands::Auth { action }) => run_auth(action).await,
            Some(Commands::Plugin { action }) => run_plugin(action).await,
            Some(Commands::Endpoint { action }) => run_endpoint(action),
            Some(Commands::Policy { action }) => run_policy(action),
            Some(Commands::Catalog { action }) => run_catalog(action),
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
            Some(Commands::Docs { bind, model }) => run_docs(bind, model).await,
            Some(Commands::Preset { args }) => preset::run_preset(&args).await,
            Some(Commands::Doctor { json }) => run_doctor(json).await,
            Some(Commands::Context { action }) => run_context(action).await,
            Some(Commands::System { action }) => run_system(action).await,
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
