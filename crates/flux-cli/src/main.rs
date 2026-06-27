//! The `flux` binary.
//!
//! M0 surface: a one-shot mode that streams a single Anthropic response to stdout. The
//! interactive REPL and TUI land in M2; this establishes the end-to-end path
//! (CLI → provider → stream → render).

mod preset;
mod style;

use std::io::{IsTerminal, Write};

use anyhow::{bail, Context, Result};
use clap::Parser;
use futures::StreamExt;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_agent::{AgentSink, DEFAULT_SYSTEM_PROMPT};
use flux_anthropic::anthropic_from_env;
use flux_context::{EnvContext, GitContext, ProjectFiles, Projector, RepoSignal};
use flux_core::{Chunk, ContentBlock, StopReason, Usage};
use flux_flow::engine::FlowEngine;
use flux_flow::state::FlowStore;
use flux_openai::{openai_from_env, openrouter_from_env};
use flux_orchestrate::{LocalSpawner, ProviderFactory, Role, RoleRegistry, TaskTool};
use flux_provider::{ChunkStream, Effort, NativeProvider, Provider, Request};
use flux_runtime::{
    AllowApprover, ApprovalChoice, Approver, Executor, PermissionManager, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_session::SessionStore;
use flux_spec::IntentSet;
use flux_system::{System, Workspace};
use reedline::{FileBackedHistory, Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};
use std::borrow::Cow;

/// flux — the LLM plans, the runtime runs.
#[derive(Parser, Debug)]
#[command(
    name = "flux",
    version,
    about = "flux — the LLM plans, the runtime runs"
)]
struct Cli {
    /// The prompt (joined with spaces if given as multiple words).
    prompt: Vec<String>,

    /// (Hidden) Non-interactive print mode — a bare prompt is already one-shot, so this is a no-op alias.
    #[arg(short = 'p', long = "print", hide = true)]
    print: bool,

    /// Fully-qualified `provider/model` spec. Provider must be one of:
    ///   `anthropic` (API key), `claude` (OAuth/subscription), `openai`, `codex`, `openrouter`.
    /// Short aliases `sonnet`, `opus`, `haiku` are convenience shorthands for `anthropic/<model>`.
    /// Examples: `claude/claude-sonnet-4-6`, `openai/gpt-4o`, `openrouter/anthropic/claude-sonnet-4-5`.
    /// Overrides `model` in `.flux/config.toml`; falls back to `sonnet` (= `anthropic/claude-sonnet-4-6`).
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// (Hidden) Adaptive thinking — only wired on the `-p` raw path; a no-op for the engine for now.
    #[arg(long, hide = true)]
    think: bool,

    /// (Hidden) Reasoning effort — only wired on the `-p` raw path; a no-op for the engine for now.
    #[arg(long, value_enum, hide = true)]
    effort: Option<EffortArg>,

    /// Maximum tokens to generate. The planner must fit the entire `emit_plan` graph in this budget,
    /// so it is generous by default; a turn truncated here fails loudly rather than silently stopping.
    #[arg(long, default_value_t = 16384)]
    max_tokens: u32,

    /// (Hidden) Print token usage — only wired on the `-p` raw path.
    #[arg(long, hide = true)]
    usage: bool,

    /// (Hidden, deprecated) The Flux-Lang engine is the default for a bare prompt; this is a no-op.
    #[arg(long, hide = true)]
    agent: bool,

    /// Auto-approve every tool call (headless). Without it, unmatched calls prompt for approval.
    #[arg(long)]
    yes: bool,

    /// Show tool output in full (no truncation). Plans and tool inputs are always shown in full; this
    /// also un-caps tool *output* (e.g. large file reads). Also enabled by `FLUX_VERBOSE`.
    #[arg(short = 'v', long)]
    verbose: bool,

    /// When to colorize output: auto (a terminal, `NO_COLOR` unset), always, or never.
    #[arg(long, value_enum, default_value_t)]
    color: style::ColorChoice,

    /// Launch the ratatui chat TUI (requires a real terminal). Tool calls raise a y/a/N modal;
    /// pass `--yes` to auto-approve all calls without a modal.
    #[arg(long)]
    tui: bool,

    /// Continue the most recent session instead of starting a new one.
    #[arg(short = 'c', long)]
    continue_: bool,

    /// Resume the most recent session (equivalent to --continue; used by hot-reload).
    #[arg(long)]
    resume: bool,

    /// Dev mode: enables hot-reload (`flux_reload` tool) and other developer tools.
    #[arg(long)]
    dev: bool,

    /// Bind a long-running HTTP API daemon at this address (e.g. 127.0.0.1:8787).
    #[arg(long)]
    serve: Option<String>,

    /// Plan mode: compile the prompt to a Flux-Lang plan and show it. On a terminal it then asks
    /// `run it? [y/N]`; piped or with `-o json|yaml` it just prints the plan and exits (never runs).
    #[arg(long, alias = "compile-only")]
    plan: bool,

    /// Plan output format for `--plan` when not running it: json, yaml, or pretty (default).
    #[arg(short = 'o', long, value_enum)]
    output: Option<OutputFormat>,
}

/// Reasoning effort, as a CLI value-enum mirroring [`Effort`].
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum EffortArg {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl From<EffortArg> for Effort {
    fn from(e: EffortArg) -> Self {
        match e {
            EffortArg::Low => Effort::Low,
            EffortArg::Medium => Effort::Medium,
            EffortArg::High => Effort::High,
            EffortArg::Xhigh => Effort::Xhigh,
            EffortArg::Max => Effort::Max,
        }
    }
}

/// Output format for `--compile-only`.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum OutputFormat {
    Json,
    Yaml,
    #[default]
    Pretty,
}

/// Resolve a friendly Anthropic alias to a concrete model id (pass-through otherwise).
fn resolve_anthropic_alias(alias: &str) -> String {
    match alias {
        "sonnet" => "claude-sonnet-4-6",
        "opus" => "claude-opus-4-8",
        "haiku" => "claude-haiku-4-5-20251001",
        other => other,
    }
    .to_string()
}

/// Resolve the model spec with precedence: `--model` flag > config `model` > `sonnet`.
fn resolve_model_spec(cli_model: &Option<String>, cfg: &flux_config::Config) -> String {
    cli_model
        .clone()
        .or_else(|| cfg.model.clone())
        .unwrap_or_else(|| "sonnet".to_string())
}

/// Persist newly "always-allow"ed permission rules back to the project config, if any changed.
fn persist_new_rules(initial: &[String], current: &[String]) {
    if current == initial {
        return;
    }
    if let Ok(cwd) = std::env::current_dir() {
        match flux_config::persist_allow_rules(&cwd, current) {
            Ok(()) => eprintln!(
                "{}",
                style::dim("(saved updated permissions to .flux/config.toml)")
            ),
            Err(e) => eprintln!(
                "{}",
                style::dim(&format!("(could not save permissions: {e})"))
            ),
        }
    }
}

const KNOWN_PROVIDERS: &[&str] = &["anthropic", "claude", "openai", "codex", "openrouter"];

/// Parse a fully-qualified `provider/model` spec and build the matching provider from environment
/// credentials. Provider must be an explicit prefix (`anthropic/`, `claude/`, `openai/`, `codex/`,
/// `openrouter/`). Bare short aliases (`sonnet`, `opus`, `haiku`) are implicitly `anthropic/<alias>`.
/// Any other bare string (no `/`) is an error — use `anthropic/` or `claude/` to disambiguate.
fn build_provider(spec: &str) -> Result<(NativeProvider, String)> {
    let (provider, model) = match spec.split_once('/') {
        Some((p, m)) if KNOWN_PROVIDERS.contains(&p) => (p.to_string(), m.to_string()),
        Some((p, _)) => bail!(
            "unknown provider `{p}` — use one of: {}",
            KNOWN_PROVIDERS.join(", ")
        ),
        None => {
            // Allow bare short aliases only; everything else requires an explicit provider prefix.
            match spec {
                "sonnet" | "opus" | "haiku" | "mock" => ("anthropic".to_string(), spec.to_string()),
                other => bail!(
                    "model spec `{other}` has no provider prefix — use `provider/model`, e.g. \
                     `anthropic/{other}` or `claude/{other}` (providers: {})",
                    KNOWN_PROVIDERS.join(", ")
                ),
            }
        }
    };

    let native = match provider.as_str() {
        "anthropic" => anthropic_from_env().context("anthropic provider")?,
        "openai" => openai_from_env().context("openai provider")?,
        "openrouter" => openrouter_from_env().context("openrouter provider")?,
        "claude" => {
            let ts = flux_credentials::claude_token_source().context("claude provider")?;
            flux_anthropic::claude_oauth(ts)
        }
        "codex" => {
            let ts = flux_credentials::codex_token_source().context("codex provider")?;
            flux_openai::codex_oauth(ts)
        }
        other => bail!(
            "unknown provider `{other}` (known: {})",
            KNOWN_PROVIDERS.join(", ")
        ),
    };

    let model = match provider.as_str() {
        "anthropic" | "claude" => resolve_anthropic_alias(&model),
        _ => model,
    };
    Ok((native, model))
}

/// Build a keyword index of the workspace's documentation files (markdown/text), for the `search`
/// tool. Deliberately cheap: doc extensions only, capped file count and size — code search is
/// served by `grep`, not this. Errors are swallowed (an empty index just yields "no matches").
async fn build_doc_index(system: &System) -> flux_datasource::Index {
    const DOC_EXTS: &[&str] = &[".md", ".txt", ".rst", ".adoc", ".mdx"];
    const MAX_DOCS: usize = 200;
    const MAX_BYTES: usize = 100_000;
    let mut index = flux_datasource::Index::new();
    let Ok(files) = system.walk_files(".", 4000).await else {
        return index;
    };
    for f in files {
        if index.len() >= MAX_DOCS {
            break;
        }
        if !DOC_EXTS.iter().any(|e| f.ends_with(e)) {
            continue;
        }
        if let Ok(text) = system.read_file(&f).await {
            if text.len() <= MAX_BYTES {
                index.add(f, text);
            }
        }
    }
    index
}

/// Session size (serialized chars) past which the agent summarizes old turns. Override with
/// `FLUX_COMPACT_CHARS` (`0` disables compaction).
fn compact_threshold() -> usize {
    std::env::var("FLUX_COMPACT_CHARS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(48_000)
}

/// Discover skills from `.flux/skills` (project) and `~/.flux/skills` (user). Their triggers gate
/// per-turn activation in the agent loop.
fn load_skills(cwd: &std::path::Path) -> Vec<flux_skill::Skill> {
    let mut dirs = vec![cwd.join(".flux").join("skills")];
    if let Some(home) = std::env::var_os("HOME") {
        let user = std::path::PathBuf::from(home).join(".flux").join("skills");
        if !dirs.contains(&user) {
            dirs.push(user); // avoid scanning the same dir twice when HOME == project root
        }
    }
    // De-duplicate by skill name (project dir is scanned first → wins on a name clash).
    let mut seen = std::collections::HashSet::new();
    flux_skill::discover(&dirs)
        .into_iter()
        .filter(|s| seen.insert(s.name.clone()))
        .collect()
}

/// The plugin descriptor directory `~/.flux/plugins` (None if `HOME` is unset).
fn plugins_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".flux").join("plugins"))
}

/// A coarse "… ago" string from a millisecond epoch timestamp (for session listings).
fn fmt_age(created_at_ms: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(created_at_ms);
    let secs = ((now - created_at_ms) / 1000).max(0);
    match secs {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// `flux sessions` — list recent sessions (newest first).
/// `flux sessions --prune` — delete all zero-message (abandoned) sessions.
fn run_sessions() -> Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    let prune = argv.get(2).map(|s| s == "--prune").unwrap_or(false);
    let store = open_session_store()?;
    if prune {
        let n = store.prune_empty()?;
        if n == 0 {
            eprintln!("no empty sessions to prune");
        } else {
            eprintln!("pruned {n} empty session{}", if n == 1 { "" } else { "s" });
        }
        return Ok(());
    }
    let sessions = store.list(30)?;
    if sessions.is_empty() {
        eprintln!("no sessions yet — start one with `flux` or `flux --agent`");
        return Ok(());
    }
    for s in &sessions {
        let active_ts = if s.updated_at_ms > s.created_at_ms {
            format!("active {}", fmt_age(s.updated_at_ms))
        } else {
            fmt_age(s.created_at_ms)
        };
        println!(
            "{}  {:>3} msg  {:<22} {}",
            s.id, s.messages, s.model, active_ts
        );
    }
    Ok(())
}

/// Open the session store under `~/.flux/sessions.db`.
fn open_session_store() -> Result<SessionStore> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    let dir = home.join(".flux");
    std::fs::create_dir_all(&dir)?;
    SessionStore::open(dir.join("sessions.db")).context("open session store")
}

/// Open flux-flow's own store under `~/.flux/flow.db` (values, symbols, run-event trace).
fn open_flow_store() -> Result<FlowStore> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    let dir = home.join(".flux");
    std::fs::create_dir_all(&dir)?;
    FlowStore::open(dir.join("flow.db")).context("open flow store")
}

/// Build a fresh boxed provider for a model spec (used by the sub-agent factory).
fn provider_for(spec: &str) -> Result<Box<dyn Provider>> {
    if spec == "mock" || spec.starts_with("mock/") {
        Ok(Box::<MockCliProvider>::default())
    } else {
        let (native, _model) = build_provider(spec).map_err(|e| {
            anyhow::anyhow!(
            "sub-agent provider: {e} (hint: the parent --model spec is forwarded to sub-agents)"
        )
        })?;
        Ok(Box::new(native))
    }
}

/// Built-in sub-agent roles (used when `.flux/agents/*.md` doesn't define them).
const DEFAULT_ROLES: &[(&str, &str, &str)] = &[
    (
        "scout",
        "Fast read-only codebase reconnaissance",
        "You are a scout. Quickly investigate the codebase with read-only tools and return a \
         compressed summary of relevant findings. Do not modify anything.",
    ),
    (
        "planner",
        "Produce a structured implementation plan",
        "You are a planner. Analyze the task and return a concise, ordered list of concrete \
         subtasks with any open questions. Do not modify files.",
    ),
    (
        "worker",
        "Execute a single well-scoped subtask",
        "You are a worker. Execute the given subtask precisely using the available tools, then \
         report what you changed.",
    ),
    (
        "reviewer",
        "Review changes for correctness",
        "You are a reviewer. Inspect the described changes for bugs and issues and report your \
         findings. Read-only.",
    ),
    (
        "evaluator",
        "Judge whether a goal is satisfied",
        "You are a strict evaluator. Given a goal and the latest result, reply with exactly \
         `SATISFIED` if the goal is fully met, otherwise `CONTINUE: <one concrete next \
         instruction>`. Do not do the work yourself.",
    ),
    (
        "summarizer",
        "Condense a transcript",
        "You are a summarizer. Condense the conversation so far into a compact set of durable \
         facts, decisions, and open threads. Preserve file paths, names, and numbers. Be terse.",
    ),
];

/// Load agent roles from `.flux/agents` (project + home), seeding the built-in roles when absent.
fn load_roles(cwd: &std::path::Path) -> RoleRegistry {
    let mut dirs = vec![cwd.join(".flux").join("agents")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(std::path::PathBuf::from(home).join(".flux").join("agents"));
    }
    let mut reg = RoleRegistry::load(&dirs);
    for (name, desc, prompt) in DEFAULT_ROLES {
        if reg.get(name).is_none() {
            reg.insert(Role {
                name: (*name).to_string(),
                description: (*desc).to_string(),
                model: None,
                tools: None, // built-in roles inherit the parent's full toolset
                prompt: (*prompt).to_string(),
            });
        }
    }
    reg
}

/// Agentic mode: run a tool-enabled, policy-gated, session-persisted turn.
/// Build a tool-enabled agent (provider + safety envelope + session) for agentic mode / the REPL.
async fn build_agent(cli: &Cli) -> Result<(FlowEngine, String, Arc<dyn flux_runtime::Spawner>)> {
    // Guarded system rooted at the current directory; layered config loaded from it.
    let cwd = std::env::current_dir().context("current dir")?;
    let cfg = flux_config::load(&cwd).context("load .flux/config.toml")?;
    let model_spec = resolve_model_spec(&cli.model, &cfg);

    // The built-in `mock` provider lets the full agentic loop be exercised offline via the CLI.
    let (provider, model): (Box<dyn Provider>, String) =
        if model_spec == "mock" || model_spec.starts_with("mock/") {
            (Box::<MockCliProvider>::default(), "mock".to_string())
        } else {
            let (native, m) = build_provider(&model_spec)?;
            (Box::new(native), m)
        };

    let system = Arc::new(System::new(Workspace::new(&cwd).context("workspace")?));

    // Project context folded into the system prompt: environment, git working-tree state, repo
    // shape/stack, and project conventions (CLAUDE.md/AGENTS.md) — so the agent isn't cold-starting.
    let system_prompt = Projector::new()
        .with(Box::new(EnvContext::new(cwd.clone())))
        .with(Box::new(GitContext::new(cwd.clone())))
        .with(Box::new(RepoSignal::new(cwd.clone())))
        .with(Box::new(ProjectFiles::new(cwd.clone())))
        .system_prompt(DEFAULT_SYSTEM_PROMPT)
        .await;

    // Authorization policy floor (built-in local grants + any config grants) and resolved
    // identity — shared by the top-level agent and the sub-agents it spawns.
    let mut policy = flux_policy::default_local_grants();
    if let Some(extra) = cfg.policy.clone() {
        policy.grants.extend(extra.grants);
    }
    let (caller, trust) =
        flux_auth::IdentityProvider::resolve(&flux_auth::LocalIdentity::current());

    // Sub-agent spawner (multi-agent orchestration): the `task` tool delegates to roles, each run
    // as an isolated sub-agent — bounded by the same authorization policy (no blanket allow).
    let roles = Arc::new(load_roles(&cwd));
    let mut sub_registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut sub_registry);
    let factory: ProviderFactory = {
        let spec = model_spec.clone();
        Arc::new(move || provider_for(&spec).map_err(|e| flux_core::Error::Other(e.to_string())))
    };
    let spawner: Arc<dyn flux_runtime::Spawner> = Arc::new(
        LocalSpawner::new(
            factory,
            roles,
            sub_registry,
            system.clone(),
            model.clone(),
            cli.max_tokens,
        )
        .with_authorization(policy.clone(), caller.clone(), trust.clone()),
    );

    // Tools + permissions: from config (deny/allow rules); if no allow rules are configured,
    // reads are pre-allowed by default so the common case needs no config. Mutating tools prompt
    // (unless --yes) and "always-allow" choices are persisted back by the caller.
    let mut registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut registry);
    if cli.dev {
        flux_tools::register_dev_builtins(&mut registry);
    }
    registry.register(Arc::new(TaskTool));

    // Model-backed cognition ops (ai.extract/rank/judge/reason, synth, ai.rewrite): the L3
    // CognitionPack, advertised on the real CLI path so a plan can call the model as a typed op.
    // `CognitionPack` needs an `Arc<dyn Provider>`, but `provider` is moved into the `FlowEngine`
    // below, so build a sibling provider instance from the same spec for the pack to own (for
    // `mock` this is a fresh, hermetic `MockCliProvider`). If the sibling can't be built we skip the
    // pack rather than fail startup — the rest of the agent is unaffected.
    match provider_for(&model_spec) {
        Ok(cog_provider) => {
            flux_cognition::CognitionPack::new(Arc::from(cog_provider), model.clone())
                .register(&mut registry);
        }
        Err(e) => eprintln!(
            "{}",
            style::dim(&format!("(cognition pack not wired: {e})"))
        ),
    }

    // Eval / self-improvement ops (the ones the improve flows orchestrate). Registered on the
    // top-level registry only — never on `sub_registry`, so worker sub-agents can't run eval/git ops.
    flux_eval::register_eval_ops(&mut registry);

    // Guarded web access (policy-gated as network egress; private/loopback per config).
    registry.register(Arc::new(
        flux_browser::WebFetchTool::default().allow_private(cfg.allow_private_net),
    ));

    // Auto-index workspace docs (markdown/text, capped & cheap) for the `search` tool.
    let index = Arc::new(build_doc_index(&system).await);
    registry.register(Arc::new(flux_datasource::SearchTool::new(index)));

    // Discover subprocess plugins (~/.flux/plugins/*.toml) and project their operations as tools.
    // Each plugin's host capabilities are the guarded System (same boundary as built-in tools).
    if let Some(dir) = plugins_dir() {
        for p in flux_plugin::discover(&dir) {
            // Build host capabilities from the plugin's own manifest declaration, so each plugin
            // gets only the process/secret/http access it asked for (and nothing by default).
            let system = system.clone();
            let allow_private = cfg.allow_private_net;
            let make_caps = move |m: &flux_plugin::PluginManifest| {
                Arc::new(
                    flux_plugin::SystemHostCaps::new(system)
                        .allow_private_net(allow_private)
                        .with_grants(m.capabilities.clone()),
                ) as Arc<dyn flux_plugin::HostCapabilities>
            };
            match flux_plugin::load_plugin_tools(
                &p.descriptor.program,
                &p.descriptor.args,
                make_caps,
            )
            .await
            {
                Ok((tools, _host)) => {
                    // The registered tools hold the host alive for the session.
                    for t in tools {
                        registry.register(t);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        style::dim(&format!("(plugin `{}` failed to load: {e})", p.name))
                    )
                }
            }
        }
    }

    // Read-only tools are pre-allowed by default so the common case needs no config; network/
    // mutating tools still gate. A configured allow-list replaces this default entirely.
    let mut allow = cfg.permissions.allow.clone();
    if allow.is_empty() {
        allow.extend(["read", "glob", "grep", "search"].map(String::from));
    }
    let perms = PermissionManager::from_rules(&allow, &cfg.permissions.deny);
    let approver: Arc<dyn Approver> = if cli.yes {
        Arc::new(AllowApprover)
    } else {
        Arc::new(StdinApprover)
    };
    // JS pre-tool hooks (observe/modify/deny) from `.flux/hooks/*.js`.
    let mut hook_dirs = vec![cwd.join(".flux").join("hooks")];
    if let Some(home) = std::env::var_os("HOME") {
        hook_dirs.push(std::path::PathBuf::from(home).join(".flux").join("hooks"));
    }
    let js_hooks = flux_hooks::JsHookEngine::load(&hook_dirs);
    let mut hook_vec: Vec<Arc<dyn flux_runtime::PreToolHook>> = Vec::new();
    if !js_hooks.is_empty() {
        hook_vec.push(Arc::new(js_hooks));
    }

    // Seed the secret redactor from known credential env vars so their values are scrubbed from
    // tool output and logs. (Credential-shaped tokens are also caught by the redactor's heuristics.)
    let mut redactor = flux_secret::Redactor::new();
    let secret_refs: Vec<flux_secret::Ref> = [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "FLUX_SECRET",
    ]
    .iter()
    .map(|k| flux_secret::Ref::env(*k))
    .collect();
    flux_runtime::SecretResolver::new().seed_redactor(&mut redactor, &secret_refs);

    let ctx = ToolContext::new(system)
        .with_spawner(spawner.clone())
        .with_redactor(redactor);
    let executor = Executor::new(registry, perms, approver, ctx)
        .with_hooks(hook_vec)
        .with_policy(policy)
        .with_identity(caller, trust);
    // Record the available toolchain as a startup observation (audit backbone).
    executor.observe(flux_evidence::Observation::new(
        "toolchain",
        flux_evidence::Phase::Startup,
        serde_json::json!({ "tools": executor.registry().names() }),
    ));

    // Evidence-gated tool groups: built-ins (git + language scaffolds) + the eval group, with
    // `.flux/groups.toml` overrides merged on top. The engine re-probes signals each turn and
    // advertises only the surfaced groups' ops; an empty manifest would disable gating.
    let mut groups = flux_tools::groups::builtin_groups();
    groups.push(flux_eval::eval_group());
    let groups = flux_config::merge_groups(groups, flux_config::load_groups(&cwd));
    // Record the current workspace signals as a startup observation (audit; per-turn resolution
    // re-probes these live so groups can surface/un-surface as the workspace changes).
    let signals: Vec<String> = flux_runtime::detect_signals(&cwd)
        .iter()
        .filter_map(|o| {
            o.data
                .get("signal")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();
    executor.observe(flux_evidence::Observation::new(
        "project.signals",
        flux_evidence::Phase::Startup,
        serde_json::json!({ "signals": signals }),
    ));

    let store = Arc::new(open_session_store()?);
    let session_id = if cli.continue_ || cli.resume {
        store
            .latest_session_id()
            .context("latest session")?
            .ok_or_else(|| anyhow::anyhow!("no session to resume"))?
    } else {
        store.create_session(&model).context("create session")?
    };

    let agent = FlowEngine {
        provider,
        executor,
        store,
        flow: open_flow_store()?,
        model,
        system_prompt,
        max_tokens: cli.max_tokens,
        max_iterations: 25,
        skills: load_skills(&cwd),
        compact_threshold_chars: compact_threshold(),
        groups,
        cwd: cwd.clone(),
    };
    Ok((agent, session_id, spawner))
}

/// One-shot agentic turn.
async fn run_agentic(cli: &Cli, prompt: String) -> Result<()> {
    let (agent, session_id, _spawner) = build_agent(cli).await?;
    eprintln!(
        "{}",
        style::dim(&format!("{} · session {session_id}", agent.model))
    );
    let initial_rules = agent.executor.allow_rules();
    let mut sink = CliSink::new(agent.max_iterations);
    agent
        .run_turn(&session_id, &prompt, &mut sink)
        .await
        .context("agent turn")?;
    persist_new_rules(&initial_rules, &agent.executor.allow_rules());
    Ok(())
}

/// `flux flow run <file.flux> [--yes] [-m <model>]` — load a checked-in Flux-Lang graph (JSON
/// `DraftAst`) and execute it directly, **skipping the NL→plan compile**. This is the thin slice of
/// flow persistence that makes the improve flows runnable; full `.flux/flows` save/load is flux-flow M6.
/// The file is validated against the live op registry (`analyze_flow`) before anything runs, and it
/// executes through the same `Executor::dispatch` envelope as every other turn (destructive ops still
/// escalate; `--yes` auto-approves).
async fn run_flow(args: &[String]) -> Result<()> {
    let mut iter = args.iter();
    if iter.next().map(|s| s.as_str()) != Some("run") {
        bail!("usage: flux flow run <file.flux> [--yes] [-m <model>]");
    }
    let mut file: Option<String> = None;
    let mut yes = false;
    let mut model: Option<String> = None;
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--yes" | "-y" => yes = true,
            "-m" | "--model" => {
                model = Some(
                    iter.next()
                        .cloned()
                        .ok_or_else(|| anyhow::anyhow!("`-m` needs a model spec"))?,
                )
            }
            other if !other.starts_with('-') && file.is_none() => file = Some(other.to_string()),
            other => bail!("flow run: unexpected argument {other:?}"),
        }
    }
    let file = file
        .ok_or_else(|| anyhow::anyhow!("usage: flux flow run <file.flux> [--yes] [-m <model>]"))?;

    // Synthesize a Cli for build_agent from the parsed flags (reuses all the agent wiring).
    let mut synth: Vec<String> = vec!["flux".to_string()];
    if yes {
        synth.push("--yes".to_string());
    }
    if let Some(m) = &model {
        synth.push("-m".to_string());
        synth.push(m.clone());
    }
    let cli = Cli::parse_from(&synth);
    style::init(cli.color);

    let src = std::fs::read_to_string(&file).with_context(|| format!("read flow {file}"))?;
    let ast: flux_flow::ast::DraftAst = serde_json::from_str(&src)
        .with_context(|| format!("parse {file} as a Flux-Lang DraftAst (JSON)"))?;

    run_draft_ast(&cli, &ast).await
}

/// Execute a pre-built `DraftAst` through the full envelope — the shared core behind both
/// `flux flow run <file.flux>` and `flux preset <name> --run`. Builds the agent, validates the flow
/// against the live op registry, previews risk + installs the per-op approver, runs it, and prints the
/// outcome. The only inputs are the synthesized `Cli` (model/`--yes`) and the AST itself.
pub(crate) async fn run_draft_ast(cli: &Cli, ast: &flux_flow::ast::DraftAst) -> Result<()> {
    let (mut engine, session_id, _spawner) = build_agent(cli).await?;
    eprintln!(
        "{}",
        style::dim(&format!("flow · {} · session {session_id}", engine.model))
    );

    // Validate against the live op registry before running anything.
    let oreg = flux_flow::registry::OpRegistry::new(engine.executor.registry());
    if let Err(diags) = flux_flow::analyze::analyze_flow(ast, &oreg) {
        print_diagnostics(&diags);
        bail!("flow validation failed — see diagnostics above");
    }

    // Risk preview + per-op approval (same envelope as a real turn).
    let risk = flux_flow::runtime::plan_risk(ast, engine.executor.registry());
    eprintln!(
        "\n{}  {}{}",
        style::bold("flow"),
        risk_badge(&risk.summary()),
        style::dim(&format!(" · {} op(s)", risk.ops.len()))
    );
    let fallback: Arc<dyn Approver> = if cli.yes {
        Arc::new(AllowApprover)
    } else {
        Arc::new(StdinApprover)
    };
    engine
        .executor
        .set_approver(Arc::new(flux_flow::runtime::PlanApprover::new(
            risk.ops.clone(),
            fallback,
        )));

    let mut sink = CliSink::new(0);
    let outcome = flux_flow::runtime::execute_flow(
        &engine.flow,
        &engine.executor,
        &session_id,
        ast,
        &mut sink,
    )
    .await
    .context("execute flow")?;
    if !outcome.result.trim().is_empty() {
        println!("{}", outcome.result);
    } else {
        // Always surface a closing summary so a direct flow turn never ends silently.
        eprintln!(
            "{}",
            style::dim(&format!("done \u{00b7} {} step(s)", outcome.steps))
        );
    }
    sink.turn_end(None);
    Ok(())
}

/// An `AskUser` that prompts on stdin — used by `--compile-only` when attached to a terminal.
struct CliAsk;
impl flux_flow::compile::AskUser for CliAsk {
    fn ask(&self, question: &str) -> String {
        eprint!("\n{} ", style::cyan(&format!("? {question}")));
        std::io::stderr().flush().ok();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        line.trim().to_string()
    }
}

/// The stdin `ask_user` seam, offered only when attached to a terminal (otherwise the planner runs
/// without the clarifying-question tool).
fn terminal_ask(ask: &CliAsk) -> Option<&dyn flux_flow::compile::AskUser> {
    std::io::stdin()
        .is_terminal()
        .then_some(ask as &dyn flux_flow::compile::AskUser)
}

/// `--plan` (plan mode, one-shot): compile the prompt into a Flux-Lang plan and show it. On an
/// interactive terminal it then asks `run it? [y/N]` and executes on yes; piped or with `-o json|yaml`
/// it just prints the plan and exits (never runs). The same engine drives this and a real turn, so the
/// plan you see is the plan that runs.
async fn run_plan(cli: Cli) -> Result<()> {
    let prompt = cli.prompt.join(" ");
    if prompt.trim().is_empty() {
        bail!(
            "--plan needs a prompt, e.g. `flux --plan \"summarize the README into SUMMARY.txt\"`"
        );
    }
    let (mut engine, session_id, _spawner) = build_agent(&cli).await?;
    let cli_ask = CliAsk;
    eprintln!(
        "{}",
        style::dim(&format!("plan · {} · agentic", engine.model))
    );

    let compiled = match engine
        .compile_once(&session_id, &prompt, terminal_ask(&cli_ask))
        .await
        .map_err(|e| anyhow::anyhow!("{}", flux_flow::engine::planner_error(&e)))?
    {
        flux_flow::compile::TurnOutput::Plan(c) => c,
        flux_flow::compile::TurnOutput::Chat(text) => {
            // The model answered rather than planning — show the answer, no plan.
            println!("{text}");
            return Ok(());
        }
    };

    // Non-interactive (`-o json|yaml`, or piped stdout): print the plan and exit — never run.
    if cli.output.is_some() || !std::io::stdout().is_terminal() {
        let rendered = match cli.output.unwrap_or_default() {
            OutputFormat::Json => {
                serde_json::to_string_pretty(&compiled.ast).context("render json")?
            }
            OutputFormat::Yaml => serde_norway::to_string(&compiled.ast).context("render yaml")?,
            OutputFormat::Pretty => flux_flow::render::render_pretty(&compiled.ast),
        };
        println!("{rendered}");
        print_diagnostics(&compiled.diagnostics);
        return Ok(());
    }

    // Interactive: show the highlighted plan + a risk badge, then offer to run it.
    let risk = flux_flow::runtime::plan_risk(&compiled.ast, engine.executor.registry());
    eprintln!(
        "\n{}  {}{}",
        style::bold("plan"),
        risk_badge(&risk.summary()),
        style::dim(&format!(" · {} op(s)", risk.ops.len()))
    );
    eprintln!(
        "{}",
        flux_flow::render::render_styled(&compiled.ast, &style::plan_palette())
    );
    if !compiled.diagnostics.is_empty() {
        print_diagnostics(&compiled.diagnostics);
        eprintln!(
            "{}",
            style::yellow("plan references unknown operations — not running")
        );
        return Ok(());
    }
    if risk.ops.is_empty() {
        eprintln!("{}", style::dim("empty plan — nothing to run"));
        return Ok(());
    }
    if !(cli.yes || confirm_plan(risk.ops.len())) {
        eprintln!("{}", style::dim("not run"));
        return Ok(());
    }

    // Approved → run it through the same envelope (PlanApprover: approved ops pass without a re-prompt;
    // destructive ops still escalate to the fallback — per-op confirm, or auto under --yes).
    let fallback: Arc<dyn Approver> = if cli.yes {
        Arc::new(AllowApprover)
    } else {
        Arc::new(StdinApprover)
    };
    engine
        .executor
        .set_approver(Arc::new(flux_flow::runtime::PlanApprover::new(
            risk.ops.clone(),
            fallback,
        )));
    let mut sink = CliSink::new(0);
    let outcome = flux_flow::runtime::execute_flow(
        &engine.flow,
        &engine.executor,
        &session_id,
        &compiled.ast,
        &mut sink,
    )
    .await
    .context("execute flow")?;
    if !outcome.result.trim().is_empty() {
        println!("{}", outcome.result);
    }
    sink.turn_end(None);
    Ok(())
}

/// Print analyzer diagnostics (unknown ops referenced by a plan) to stderr, if any.
fn print_diagnostics(diags: &[flux_flow::analyze::Diagnostic]) {
    if diags.is_empty() {
        return;
    }
    eprintln!(
        "{}",
        style::yellow("diagnostics — the plan references unknown operations")
    );
    for d in diags {
        eprintln!("{}", style::dim(&format!("  - {}", d.message)));
    }
}

/// One stdin `y/N` confirmation for a whole compiled plan.
fn confirm_plan(steps: usize) -> bool {
    eprint!(
        "\n{} [y/N]: ",
        style::yellow(&format!("Run this {steps}-op plan?"))
    );
    std::io::stderr().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// A minimal `reedline` prompt: a single `› ` indicator (no left/right segments).
struct FluxPrompt {
    plan_mode: bool,
}

impl Prompt for FluxPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        // A distinct indicator in plan mode, so it's obvious turns won't execute.
        Cow::Borrowed(if self.plan_mode { "plan › " } else { "› " })
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("… ")
    }
    fn render_prompt_history_search_indicator(&self, _s: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Borrowed("(reverse-search) ")
    }
}

/// `~/.flux/history.txt`, creating `~/.flux` if needed; `None` if HOME is unset.
fn repl_history_path() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("HOME")?).join(".flux");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("history.txt"))
}

/// Interactive agentic REPL (tools enabled), with slash commands.
async fn run_repl(cli: Cli) -> Result<()> {
    let (mut agent, mut session_id, spawner) = build_agent(&cli).await?;
    let initial_rules = agent.executor.allow_rules();
    eprintln!(
        "{}",
        style::dim(&format!(
            "flux · {} · session {session_id} — /help, Ctrl-C interrupts a turn, Ctrl-D exits",
            agent.model
        ))
    );

    // reedline gives line editing, persistent history, and reverse-search. Because it reads in raw
    // mode, a prompt-level Ctrl-C arrives as `Signal::CtrlC` (not a SIGINT), so it cleanly clears the
    // line instead of being swallowed by tokio's signal handler; in-turn Ctrl-C is still the SIGINT
    // caught by `run_interruptible`.
    let history: Box<dyn reedline::History> = match repl_history_path() {
        Some(p) => Box::new(
            FileBackedHistory::with_file(1000, p)
                .unwrap_or_else(|_| FileBackedHistory::new(1000).expect("in-memory history")),
        ),
        None => Box::new(FileBackedHistory::new(1000).expect("in-memory history")),
    };
    let mut editor = Reedline::create().with_history(history);

    // Plan mode (`/plan`): turns produce a plan but DON'T execute; `/run` executes the pending plan.
    let mut plan_mode = false;
    let mut pending_plan: Option<flux_flow::ast::DraftAst> = None;

    loop {
        let prompt = FluxPrompt { plan_mode };
        let line = match editor.read_line(&prompt) {
            Ok(Signal::Success(buf)) => buf,
            Ok(Signal::CtrlC) => continue, // clear the current line, reprompt
            Ok(Signal::CtrlD) => break,    // exit
            Ok(_) => continue,             // future Signal variants (non_exhaustive) → reprompt
            Err(_) => break,
        };
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if let Some(rest) = input.strip_prefix('/') {
            match rest.split_whitespace().next().unwrap_or("") {
                "exit" | "quit" => break,
                "help" => {
                    const CMDS: &[(&str, &str)] = &[
                        ("/help", "show this help"),
                        ("/plan", "toggle plan mode (show plan; /run to execute)"),
                        ("/run", "execute the pending plan from plan mode"),
                        ("/tools", "list available tools"),
                        (
                            "/model <spec>",
                            "switch model (e.g. opus, sonnet, openai/gpt-4o)",
                        ),
                        ("/session", "show current session id and model"),
                        (
                            "/sessions",
                            "list recent sessions with first-message preview",
                        ),
                        ("/sessions --prune", "delete all empty (0-message) sessions"),
                        ("/resume <id>", "switch to a previous session"),
                        ("/clear", "start a new session"),
                        ("/compact", "summarise and compact the context window"),
                        ("/pd <goal>", "plan-and-dispatch: parallel dependency waves"),
                        (
                            "/goal <cond>",
                            "drive turns toward a goal; stop when satisfied",
                        ),
                        ("/loop <n> <task>", "repeat a task up to n times"),
                        ("/exit", "quit"),
                    ];
                    eprintln!("flux REPL commands:");
                    for (cmd, desc) in CMDS {
                        eprintln!("  {:<24} {}", cmd, desc);
                    }
                    eprintln!("  Ctrl-C  interrupt a running turn   Ctrl-D  exit");
                }
                "plan" => {
                    plan_mode = !plan_mode;
                    pending_plan = None;
                    eprintln!(
                        "{}",
                        style::dim(&format!(
                            "plan mode {} — {}",
                            if plan_mode { "on" } else { "off" },
                            if plan_mode {
                                "turns show a plan; `/run` to execute, or keep chatting to refine"
                            } else {
                                "turns run normally"
                            }
                        ))
                    );
                }
                "run" => match pending_plan.take() {
                    Some(ast) => {
                        let agent_ref = &agent;
                        let sid_ref = session_id.as_str();
                        run_interruptible(move |c| async move {
                            run_pending_plan(agent_ref, sid_ref, &ast, &c).await;
                        })
                        .await;
                    }
                    None => eprintln!(
                        "{}",
                        style::dim("(no pending plan — use /plan, then describe a task)")
                    ),
                },
                "model" => {
                    let spec = rest.strip_prefix("model").unwrap_or("").trim();
                    if spec.is_empty() {
                        eprintln!(
                            "model: {} · usage: /model <provider/model | opus | sonnet | haiku>",
                            agent.model
                        );
                    } else {
                        match build_provider(spec) {
                            Ok((native, model)) => {
                                agent.provider = Box::new(native);
                                agent.model = model;
                                let _ = agent.store.set_model(&session_id, &agent.model);
                                eprintln!("switched to {}", agent.model);
                            }
                            Err(e) => eprintln!("cannot switch model: {e}"),
                        }
                    }
                }
                "pd" => {
                    let goal = rest.strip_prefix("pd").unwrap_or("").trim().to_string();
                    if goal.is_empty() {
                        eprintln!("usage: /pd <goal>");
                    } else {
                        eprintln!("{}", style::dim("plan-and-dispatch (dependency waves)…"));
                        // Interruptible: Ctrl-C cancels the token, which stops further waves and
                        // aborts the in-flight sub-agent turns.
                        let sp = spawner.clone();
                        run_interruptible(|c| async move {
                            // Prefer parallel dependency waves; fall back to the sequential flow if
                            // the planner doesn't emit a JSON subtask array.
                            let res = match flux_orchestrate::plan_and_dispatch_waves(
                                sp.as_ref(),
                                &goal,
                                &c,
                            )
                            .await
                            {
                                Ok(out) => Ok(out),
                                Err(_) => {
                                    flux_orchestrate::plan_and_dispatch(sp.as_ref(), &goal, &c)
                                        .await
                                }
                            };
                            match res {
                                Ok(out) => println!("{out}"),
                                Err(e) => eprintln!("{} {e:#}", style::red("error:")),
                            }
                        })
                        .await;
                    }
                }
                "goal" => {
                    let cond = rest.strip_prefix("goal").unwrap_or("").trim().to_string();
                    if cond.is_empty() {
                        eprintln!("usage: /goal <condition>");
                    } else {
                        run_interruptible(|c| {
                            run_goal(&agent, &session_id, spawner.as_ref(), &cond, c)
                        })
                        .await;
                    }
                }
                "loop" => {
                    let args = rest.strip_prefix("loop").unwrap_or("").trim();
                    let (n, task) = parse_loop_args(args);
                    if task.is_empty() {
                        eprintln!("usage: /loop <count> <task>");
                    } else {
                        run_interruptible(|c| run_loop(&agent, &session_id, n, &task, c)).await;
                    }
                }
                "tools" => {
                    let mut names = agent.executor.registry().names();
                    names.sort();
                    eprintln!("tools: {}", names.join(", "));
                }
                "session" => eprintln!("session {session_id} · model {}", agent.model),
                "sessions" => match agent.store.list(30) {
                    Ok(list) if !list.is_empty() => {
                        for s in &list {
                            let here = if s.id == session_id { "*" } else { " " };
                            // Try to load the first user message as a human-readable preview.
                            let preview = agent
                                .store
                                .load_messages(&s.id)
                                .ok()
                                .and_then(|msgs| {
                                    msgs.into_iter()
                                        .find(|m| m.role == flux_core::Role::User)
                                        .and_then(|m| {
                                            m.content.into_iter().find_map(|b| match b {
                                                flux_core::ContentBlock::Text { text } => {
                                                    Some(text)
                                                }
                                                _ => None,
                                            })
                                        })
                                })
                                .map(|t| {
                                    let t = t.trim().replace('\n', " ");
                                    let t: String = t.chars().take(50).collect();
                                    format!("  {}", style::dim(&t))
                                })
                                .unwrap_or_default();
                            let active_ts = if s.updated_at_ms > s.created_at_ms {
                                format!("active {}", fmt_age(s.updated_at_ms))
                            } else {
                                fmt_age(s.created_at_ms)
                            };
                            eprintln!(
                                "{here} {}  {:>3} msg  {:<20} {}{preview}",
                                s.id, s.messages, s.model, active_ts
                            );
                        }
                    }
                    Ok(_) => eprintln!("no sessions yet"),
                    Err(e) => eprintln!("error listing sessions: {e}"),
                },
                "resume" => {
                    let id = rest.strip_prefix("resume").unwrap_or("").trim();
                    if id.is_empty() {
                        eprintln!("usage: /resume <session_id>  (see /sessions)");
                    } else {
                        match agent.store.info(id) {
                            Ok(info) => {
                                let n = agent
                                    .store
                                    .load_messages(&info.id)
                                    .map(|m| m.len())
                                    .unwrap_or(0);
                                session_id = info.id;
                                eprintln!(
                                    "resumed {session_id} · created with model {} · {n} messages",
                                    info.model
                                );
                            }
                            Err(e) => eprintln!("cannot resume `{id}`: {e}"),
                        }
                    }
                }
                "compact" => {
                    eprintln!("{}", style::dim("compacting context…"));
                    let cancel = tokio_util::sync::CancellationToken::new();
                    let mut sink = CliSink::new(0);
                    match agent.maybe_compact(&session_id, &mut sink, &cancel).await {
                        Ok(()) => eprintln!("{}", style::dim("context compacted")),
                        Err(e) => eprintln!("{} {e}", style::red("compact error:")),
                    }
                }
                "clear" => {
                    session_id = agent
                        .store
                        .create_session(&agent.model)
                        .context("new session")?;
                    eprintln!("started new session {session_id}");
                }
                other => eprintln!("unknown command /{other} (try /help)"),
            }
            continue;
        }
        // Plan mode: compile + show a plan, store it for `/run`, but DON'T execute. Refine by chatting.
        if plan_mode {
            let mut sink = CliSink::new(0);
            match agent.plan_turn(&session_id, input, &mut sink).await {
                Ok(Some(ast)) => {
                    pending_plan = Some(ast);
                    eprintln!(
                        "{}",
                        style::dim("(plan ready — `/run` to execute, or send a message to refine)")
                    );
                }
                Ok(None) => {} // the model answered in prose; nothing to run
                Err(e) => eprintln!("{} {e:#}", style::red("error:")),
            }
            continue;
        }

        // Normal mode: run the turn interruptibly. The first Ctrl-C cancels it (without killing the
        // REPL); the turn unwinds cleanly and we return to the prompt. (Ctrl-D exits.)
        let agent_ref = &agent;
        let sid_ref = session_id.as_str();
        run_interruptible(move |c| async move {
            let mut sink = CliSink::new(agent_ref.max_iterations);
            if let Err(e) = agent_ref
                .run_turn_cancellable(sid_ref, input, &mut sink, &c)
                .await
            {
                eprintln!("{} {e:#}", style::red("error:"));
            }
        })
        .await;
    }
    persist_new_rules(&initial_rules, &agent.executor.allow_rules());
    Ok(())
}

/// REPL `/run`: execute a reviewed plan through the engine's existing envelope (per-op approval still
/// applies, unless the REPL was started with `--yes`). The human reviewed the whole plan already.
async fn run_pending_plan(
    agent: &FlowEngine,
    session_id: &str,
    ast: &flux_flow::ast::DraftAst,
    _cancel: &tokio_util::sync::CancellationToken,
) {
    let mut sink = CliSink::new(0);
    match flux_flow::runtime::execute_flow(&agent.flow, &agent.executor, session_id, ast, &mut sink)
        .await
    {
        Ok(outcome) => {
            if !outcome.result.trim().is_empty() {
                println!("{}", outcome.result);
            }
            sink.turn_end(None);
        }
        Err(e) => eprintln!("{} {e:#}", style::red("error:")),
    }
}

/// Run `make(cancel)` to completion, but cancel it on Ctrl-C (the token's clones are linked, so
/// cancelling here aborts the in-flight work). Used to wrap turns and autopilot loops in the REPL.
async fn run_interruptible<F, Fut>(make: F)
where
    F: FnOnce(tokio_util::sync::CancellationToken) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let cancel = tokio_util::sync::CancellationToken::new();
    let fut = make(cancel.clone());
    tokio::pin!(fut);
    let mut interrupting = false;
    loop {
        tokio::select! {
            _ = &mut fut => break,
            _ = tokio::signal::ctrl_c() => {
                if !interrupting {
                    interrupting = true;
                    cancel.cancel();
                    eprintln!("\n{}", style::dim("(interrupting…)"));
                }
            }
        }
    }
}

/// `/goal <cond>`: drive turns toward a goal, asking a cheap `evaluator` sub-agent after each turn
/// whether the goal is satisfied; stop on SATISFIED, max-iterations, or cancellation.
async fn run_goal(
    agent: &FlowEngine,
    session_id: &str,
    spawner: &dyn flux_runtime::Spawner,
    goal: &str,
    cancel: tokio_util::sync::CancellationToken,
) {
    const MAX: usize = 6;
    let mut next_input = goal.to_string();
    for i in 0..MAX {
        if cancel.is_cancelled() {
            break;
        }
        eprintln!("{}", style::dim(&format!("[goal {}/{}]", i + 1, MAX)));
        let mut sink = GoalSink::default();
        if let Err(e) = agent
            .run_turn_cancellable(session_id, &next_input, &mut sink, &cancel)
            .await
        {
            eprintln!("{} {e:#}", style::red("error:"));
            return;
        }
        if cancel.is_cancelled() {
            break;
        }
        let verdict = match spawner
            .spawn(
                "evaluator",
                &format!(
                    "Goal: {goal}\n\nLatest result:\n{}\n\nReply `SATISFIED` or `CONTINUE: <next>`.",
                    sink.text
                ),
                &cancel,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{}", style::dim(&format!("(evaluator error: {e})")));
                return;
            }
        };
        // Match only a leading verdict so "not satisfied"/"unsatisfied" don't false-positive.
        if verdict.trim().to_uppercase().starts_with("SATISFIED") {
            eprintln!("{}", style::dim("[goal satisfied]"));
            return;
        }
        next_input = verdict
            .split_once(':')
            .map(|(_, r)| r.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| goal.to_string());
    }
    eprintln!("{}", style::dim("[goal loop ended]"));
}

/// `/loop <count> <task>`: run `task` up to `count` times (stops early on cancellation).
async fn run_loop(
    agent: &FlowEngine,
    session_id: &str,
    count: usize,
    task: &str,
    cancel: tokio_util::sync::CancellationToken,
) {
    for i in 0..count {
        if cancel.is_cancelled() {
            break;
        }
        eprintln!("{}", style::dim(&format!("[loop {}/{}]", i + 1, count)));
        let mut sink = CliSink::new(0);
        if let Err(e) = agent
            .run_turn_cancellable(session_id, task, &mut sink, &cancel)
            .await
        {
            eprintln!("{} {e:#}", style::red("error:"));
            return;
        }
    }
}

/// Parse `/loop` args as `<count> <task>` (count defaults to 1 if the first token isn't a number).
fn parse_loop_args(args: &str) -> (usize, String) {
    let mut it = args.splitn(2, char::is_whitespace);
    let first = it.next().unwrap_or("");
    if let Ok(n) = first.parse::<usize>() {
        (n.max(1), it.next().unwrap_or("").trim().to_string())
    } else {
        (1, args.trim().to_string())
    }
}

/// Whether tool output is shown in full (set by `-v`/`--verbose`, which exports `FLUX_VERBOSE`).
fn verbose() -> bool {
    std::env::var_os("FLUX_VERBOSE").is_some()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let head: String = s.chars().take(n).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

/// A preview of a tool result for the CLI: continuation lines indented under the header, with a
/// trailing note when lines were elided. `full` (from `-v`/`FLUX_VERBOSE`) disables the caps and shows
/// everything. This affects only what the user sees — the model always receives the full result.
fn tool_preview(s: &str, full: bool) -> String {
    const MAX_LINES: usize = 40;
    const MAX_LINE_CHARS: usize = 500;
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= 1 {
        return if full {
            s.to_string()
        } else {
            truncate(s, MAX_LINE_CHARS)
        };
    }
    let shown = if full {
        lines.len()
    } else {
        lines.len().min(MAX_LINES)
    };
    let mut out = String::new();
    for (i, line) in lines.iter().take(shown).enumerate() {
        if i > 0 {
            out.push_str("\n  ");
        }
        let line = line.trim_end();
        out.push_str(&if full {
            line.to_string()
        } else {
            truncate(line, MAX_LINE_CHARS)
        });
    }
    let extra = lines.len() - shown;
    if extra > 0 {
        out.push_str(&format!(
            "\n  … (+{extra} more line{}; -v for full)",
            if extra == 1 { "" } else { "s" }
        ));
    }
    out
}

/// Shared between [`CliSink`] and its spinner ticker task.
struct SpinnerState {
    active: bool,
    label: String,
    frame: usize,
}

/// Render an op call as a concise, colored *semantic* label: the cyan op name padded to a gutter, then
/// a readable argument — `bash → $ cargo test`, `read → foo.rs:100-180`, `grep → "needle" in src/`. The
/// arg is capped unless `-v`; the full plan is always shown separately (the `flow.plan` tree).
fn render_call_label(name: &str, input: &Value, verbose: bool) -> String {
    // Column width: wide enough for the longest built-in op name (`web_fetch` = 9).
    const GUTTER: usize = 10;
    const ARG_CAP: usize = 120;
    let call = flux_tui::toolview::format_call(name, input);
    let verb = style::cyan(&call.verb);
    if call.arg.is_empty() {
        return verb;
    }
    let arg = if verbose {
        call.arg
    } else {
        truncate(&call.arg, ARG_CAP)
    };
    let pad = GUTTER.saturating_sub(call.verb.chars().count()).max(1);
    format!("{verb}{}{arg}", " ".repeat(pad))
}

/// A concise result summary for the execution stream: `done` for empty output, the line(s) for a
/// small result, or a tool-aware summary for larger results. `-v` shows everything.
///
/// For `grep` and `glob` results the first few matches are shown rather than a bare line count;
/// for `bash` the last non-empty line is used as a quick exit hint. Pass `tool` as `""` for the
/// generic (tool-unaware) path.
fn result_summary_for(content: &str, tool: &str, verbose: bool) -> String {
    let content = content.trim();
    if content.is_empty() {
        return "done".to_string();
    }
    if verbose {
        return tool_preview(content, true);
    }
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();

    // Tool-aware previews.
    match tool {
        "read" | "read_many" => {
            // Never dump raw file contents — show a digest: first 3 lines + count.
            if n <= 3 {
                return lines
                    .iter()
                    .map(|l| truncate(l.trim_end(), 120))
                    .collect::<Vec<_>>()
                    .join("\n    ");
            }
            let head = lines[..3]
                .iter()
                .map(|l| truncate(l.trim_end(), 120))
                .collect::<Vec<_>>()
                .join("\n    ");
            return format!("{head}\n    … ({} more lines; -v for full)", n - 3);
        }
        "grep" if n > 3 => {
            let head = lines[..3]
                .iter()
                .map(|l| truncate(l.trim_end(), 120))
                .collect::<Vec<_>>()
                .join("\n    ");
            return format!(
                "{head}\n    … (+{} more match{}; -v for full)",
                n - 3,
                if n - 3 == 1 { "" } else { "es" }
            );
        }
        "glob" if n > 5 => {
            let head = lines[..5]
                .iter()
                .map(|l| truncate(l.trim_end(), 120))
                .collect::<Vec<_>>()
                .join("\n    ");
            return format!("{head}\n    … (+{} more; -v for full)", n - 5);
        }
        "bash" if n > 1 => {
            // Show the last non-empty line as a quick exit hint.
            let last = lines
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(&lines[n - 1]);
            let last = truncate(last.trim_end(), 160);
            return format!("{n} lines · last: {last}  (-v for full)");
        }
        _ => {}
    }

    match n {
        0 => "done".to_string(),
        1 => truncate(content, 200),
        _ if n <= 6 => lines
            .iter()
            .map(|l| truncate(l.trim_end(), 200))
            .collect::<Vec<_>>()
            .join("\n    "),
        _ => format!("{n} lines · -v for full"),
    }
}

/// Color a risk summary by its leading level (`low` green, `medium` yellow, else red).
fn risk_badge(summary: &str) -> String {
    match summary.split([' ', '·']).next().unwrap_or("").trim() {
        "low" | "no-op" => style::green(summary),
        "medium" => style::yellow(summary),
        _ => style::red(summary),
    }
}

/// Renders streaming assistant text to stdout as live-rendered Markdown, and tool activity to stderr,
/// in the "Refined" style: a syntax-highlighted plan, colored `→`/`✓`/`✗` markers, a live spinner while
/// each op runs, and a completion rule with timing. All color is tty/`NO_COLOR`/`--color`-aware.
struct CliSink {
    live: markdown_terminal::LiveRenderer,
    /// Show tool output in full (no truncation) — from `-v`/`FLUX_VERBOSE`.
    verbose: bool,
    width: usize,
    stderr_tty: bool,
    steps: usize,
    turn_start: Option<std::time::Instant>,
    /// The current op's `(label, start)`, set on `tool_call` and finalized on `tool_result`.
    pending: Option<(String, std::time::Instant)>,
    spinner: Option<(
        Arc<std::sync::Mutex<SpinnerState>>,
        tokio::task::JoinHandle<()>,
    )>,
    /// Iteration counter: how many tool round-trips have completed this turn.
    iter: usize,
    /// Max iterations cap (threaded from `Agent::max_iterations` for display).
    max_iter: usize,
}

impl CliSink {
    fn new(max_iter: usize) -> Self {
        let stdout_tty = std::io::stdout().is_terminal();
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|c| c.parse::<usize>().ok())
            .filter(|&w| w >= 20)
            .unwrap_or(80);
        CliSink {
            live: markdown_terminal::LiveRenderer::new(
                markdown_terminal::Theme::auto(),
                width,
                stdout_tty,
            ),
            verbose: verbose(),
            width,
            stderr_tty: std::io::stderr().is_terminal(),
            steps: 0,
            turn_start: None,
            pending: None,
            spinner: None,
            iter: 0,
            max_iter,
        }
    }

    /// Commit any in-progress assistant render so subsequent stderr lines appear below it.
    fn commit(&mut self) {
        if self.live.is_active() {
            let mut out = std::io::stdout().lock();
            let _ = self.live.finish(&mut out);
        }
    }

    fn use_spinner(&self) -> bool {
        self.stderr_tty && style::enabled()
    }

    /// Start an animated spinner on the op's line (a background ticker rewriting it via `\r`).
    fn start_spinner(&mut self, label: String) {
        let state = Arc::new(std::sync::Mutex::new(SpinnerState {
            active: true,
            label,
            frame: 0,
        }));
        let s = state.clone();
        let start = std::time::Instant::now();
        let task = tokio::spawn(async move {
            const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            loop {
                {
                    // Hold the lock while drawing so `stop_spinner` can't interleave.
                    let mut st = s.lock().unwrap();
                    if !st.active {
                        break;
                    }
                    let frame = FRAMES[st.frame % FRAMES.len()];
                    st.frame += 1;
                    let elapsed = style::fmt_elapsed(start.elapsed());
                    eprint!(
                        "\r\x1b[K{} {}  {}",
                        style::cyan(&frame.to_string()),
                        st.label,
                        style::dim(&elapsed)
                    );
                    let _ = std::io::stderr().flush();
                }
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            }
        });
        self.spinner = Some((state, task));
    }

    /// Stop a running spinner and clear its line. Returns true if one was active.
    fn stop_spinner(&mut self) -> bool {
        if let Some((state, task)) = self.spinner.take() {
            state.lock().unwrap().active = false;
            eprint!("\r\x1b[K");
            std::io::stderr().flush().ok();
            task.abort();
            true
        } else {
            false
        }
    }
}

impl AgentSink for CliSink {
    fn text_delta(&mut self, t: &str) {
        let mut out = std::io::stdout().lock();
        let _ = self.live.push(t, &mut out);
    }
    fn thinking_delta(&mut self, t: &str) {
        // Stream extended-thinking tokens dimmed on stderr so reasoning is observable in the REPL.
        eprint!("{}", style::dim(t));
        std::io::stderr().flush().ok();
    }
    fn planning(&mut self, active: bool) {
        // Fill the otherwise-silent compile wait with a spinner; the compiled plan tree replaces it
        // (via the `flow.plan` observation) once the planner is done.
        if active {
            self.commit();
            if self.use_spinner() {
                self.start_spinner(style::dim("composing plan…"));
            }
        } else {
            self.stop_spinner();
        }
    }
    fn tool_call(&mut self, name: &str, input: &Value) {
        self.commit();
        self.steps += 1;
        self.iter += 1;
        if self.turn_start.is_none() {
            self.turn_start = Some(std::time::Instant::now());
        }
        let base_label = render_call_label(name, input, self.verbose);
        // Prefix with [N/max] iteration counter when a cap is known.
        let label = if self.max_iter > 0 {
            format!("[{}/{}] {base_label}", self.iter, self.max_iter)
        } else {
            base_label
        };
        if self.use_spinner() {
            self.start_spinner(label.clone());
        } else {
            eprintln!("\n{} {label}", style::blue("→"));
        }
        self.pending = Some((label, std::time::Instant::now()));
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        let (label, start) = self
            .pending
            .take()
            .unwrap_or_else(|| (String::new(), std::time::Instant::now()));
        // If a spinner ran, its line is cleared — reprint the call line so it stays in the scrollback.
        if self.stop_spinner() {
            eprintln!("\n{} {label}", style::blue("→"));
        }
        let elapsed = style::dim(&format!("· {}", style::fmt_elapsed(start.elapsed())));
        let body = flux_tui::toolview::format_result(name, &result.content, result.is_error)
            .unwrap_or_else(|| result_summary_for(&result.content, name, self.verbose));
        let mark = if result.is_error {
            style::red("✗")
        } else {
            style::green("✓")
        };
        eprintln!("  {mark} {body}  {elapsed}");
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.commit();
        if o.kind == flux_evidence::KIND_DESTRUCTIVE {
            eprintln!(
                "{}",
                style::yellow("⚠ destructive operation — approval required")
            );
        } else if o.kind == "skill.activated" {
            if let Some(name) = o.data.get("skill").and_then(|v| v.as_str()) {
                eprintln!("{}", style::dim(&format!("✦ skill: {name}")));
            }
        } else if o.kind == "context.compacted" {
            let from = o
                .data
                .get("from_messages")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let to = o
                .data
                .get("to_messages")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            eprintln!(
                "{}",
                style::dim(&format!("⊙ context compacted ({from} → {to} messages)"))
            );
        } else if o.kind == "turn.cancelled" {
            eprintln!("{}", style::dim("⊘ turn cancelled"));
        } else if o.kind == "flow.plan" {
            self.render_plan(o);
        }
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        self.commit();
        self.stop_spinner();
        let elapsed = self
            .turn_start
            .map(|t| style::fmt_elapsed(t.elapsed()))
            .unwrap_or_default();
        // Always print a rule so the turn boundary is visible even for prose-only replies.
        if self.steps > 0 {
            let plural = if self.steps == 1 { "" } else { "s" };
            // Build the right-hand annotation: steps + timing + inline token counts.
            let token_inline = usage
                .as_ref()
                .map(|u| {
                    if u.cache_read_input_tokens > 0 {
                        format!(
                            " · in {} out {} $cache {}",
                            u.input_tokens, u.output_tokens, u.cache_read_input_tokens
                        )
                    } else {
                        format!(" · in {} out {}", u.input_tokens, u.output_tokens)
                    }
                })
                .unwrap_or_default();
            let summary = format!("{} step{plural} · {elapsed}{token_inline}", self.steps);
            let rule_len = self.width.saturating_sub(summary.chars().count() + 2);
            eprintln!("{} {}", style::rule(rule_len), style::dim(&summary));
        } else {
            // Prose-only turn: print a minimal rule with elapsed + token stats.
            let token_inline = usage
                .as_ref()
                .map(|u| {
                    if u.cache_read_input_tokens > 0 {
                        format!(
                            " · in {} out {} $cache {}",
                            u.input_tokens, u.output_tokens, u.cache_read_input_tokens
                        )
                    } else {
                        format!(" · in {} out {}", u.input_tokens, u.output_tokens)
                    }
                })
                .unwrap_or_default();
            let summary = format!("· {elapsed}{token_inline}");
            let rule_len = self.width.saturating_sub(summary.chars().count() + 2);
            eprintln!("{} {}", style::rule(rule_len), style::dim(&summary));
        }
    }
}

impl CliSink {
    /// Render a `flow.plan` observation: the syntax-highlighted plan tree + a risk badge header.
    fn render_plan(&self, o: &flux_evidence::Observation) {
        let rendered = o
            .data
            .get("plan_ast")
            .and_then(|v| serde_json::from_value::<flux_flow::ast::DraftAst>(v.clone()).ok())
            .map(|ast| flux_flow::render::render_styled(&ast, &style::plan_palette()))
            .or_else(|| {
                o.data
                    .get("plan")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        let Some(rendered) = rendered else { return };
        let risk = o.data.get("risk").and_then(|v| v.as_str()).unwrap_or("");
        let ops = o.data.get("ops").and_then(|v| v.as_u64()).unwrap_or(0);
        eprintln!(
            "\n{}  {}{}",
            style::bold("plan"),
            risk_badge(risk),
            style::dim(&format!(" · {ops} op(s)"))
        );
        eprintln!("{rendered}");
    }
}

/// Like [`CliSink`] but also accumulates the assistant text (so `/goal`'s evaluator can read it).
#[derive(Default)]
struct GoalSink {
    text: String,
}

impl AgentSink for GoalSink {
    fn text_delta(&mut self, t: &str) {
        print!("{t}");
        std::io::stdout().flush().ok();
        self.text.push_str(t);
    }
    fn tool_call(&mut self, name: &str, input: &Value) {
        eprintln!(
            "\n{} {}",
            style::blue("→"),
            render_call_label(name, input, verbose())
        );
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        let mark = if result.is_error {
            style::red("✗")
        } else {
            style::green("✓")
        };
        let body = flux_tui::toolview::format_result(name, &result.content, result.is_error)
            .unwrap_or_else(|| result_summary_for(&result.content, name, verbose()));
        eprintln!("  {mark} {body}");
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        println!();
        if let Some(u) = usage {
            let stats = if u.cache_read_input_tokens > 0 {
                format!(
                    "in {} out {} $cache {}",
                    u.input_tokens, u.output_tokens, u.cache_read_input_tokens
                )
            } else {
                format!("in {} out {}", u.input_tokens, u.output_tokens)
            };
            eprintln!("{}", style::dim(&stats));
        }
    }
}

/// A built-in offline provider (`-m mock`): the first call emits a one-shot `emit_plan` plan that
/// writes `flux-mock.txt` (or runs `FLUX_MOCK_BASH` / calls `FLUX_MOCK_TOOL`); the engine runs it,
/// feeds the results back, and loops, so the second call answers in prose and the turn ends (the
/// standard loop-to-prose). Because the engine is pure-DAG (the model's only tool is `emit_plan`), the
/// mock must emit a *plan*, not a raw tool call. Lets the Flux-Lang engine be exercised end-to-end with
/// no network — used by the eval harness's offline slice and smoke tests.
#[derive(Default)]
struct MockCliProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for MockCliProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(&self, _req: Request) -> flux_core::Result<ChunkStream> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed);

        // Test hook: `FLUX_MOCK_HANG=1` streams one delta then never completes (only cancellation
        // can end the turn) — used to exercise Ctrl-C interruption in the REPL.
        if std::env::var("FLUX_MOCK_HANG").is_ok() {
            let s = futures::stream::once(async { Ok(Chunk::TextDelta("thinking…".into())) })
                .chain(futures::stream::pending::<flux_core::Result<Chunk>>());
            return Ok(Box::pin(s));
        }

        // Second call: the plan (emitted on the first call with no `complete`) has run and its results
        // were fed back, so the engine loops here — answer in prose, which ends the turn.
        if n > 0 {
            let chunks = vec![
                Chunk::Block(ContentBlock::Text {
                    text: "Finished.".into(),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ];
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }

        // Build a one-shot Flux-Lang plan (the engine is pure-DAG, so the model emits `emit_plan`).
        // `FLUX_MOCK_TOOL` calls any tool (input = `FLUX_MOCK_TOOL_INPUT`, passed as a lone object so
        // it maps straight to the tool's named input); `FLUX_MOCK_BASH` runs a `bash` command; the
        // default writes `flux-mock.txt`. No `complete` ⇒ the engine loops, and the second call (above)
        // ends the turn in prose.
        let ast: serde_json::Value = if let Ok(tool) = std::env::var("FLUX_MOCK_TOOL") {
            let input: serde_json::Value = std::env::var("FLUX_MOCK_TOOL_INPUT")
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            serde_json::json!({
                "body": [{
                    "kind": "call", "op": tool,
                    "args": [{ "kind": "lit", "value": input }]
                }]
            })
        } else if let Ok(cmd) = std::env::var("FLUX_MOCK_BASH") {
            serde_json::json!({
                "body": [{
                    "kind": "call", "op": "bash",
                    "args": [{ "kind": "lit", "value": cmd }]
                }]
            })
        } else {
            serde_json::json!({
                "body": [{
                    "kind": "call", "op": "write",
                    "args": [
                        { "kind": "lit", "value": "flux-mock.txt" },
                        { "kind": "lit", "value": "created by flux mock\n" }
                    ]
                }]
            })
        };

        let chunks = vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: "plan1".into(),
                name: "emit_plan".into(),
                input: serde_json::json!({ "ast": ast }),
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ];
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

/// Interactive approval prompt for tool calls not covered by a rule.
struct StdinApprover;

#[async_trait]
impl Approver for StdinApprover {
    async fn request(
        &self,
        tool: &str,
        subjects: &[String],
        _intents: &IntentSet,
    ) -> ApprovalChoice {
        // Format subjects as a human-readable list (not Debug), with paths trimmed to the last two
        // components so long absolute paths don't swamp the prompt.
        let subjects_fmt = if subjects.is_empty() {
            String::new()
        } else {
            let formatted: Vec<String> = subjects
                .iter()
                .map(|s| {
                    let p = std::path::Path::new(s);
                    let trimmed = p
                        .components()
                        .rev()
                        .take(2)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<std::path::PathBuf>();
                    style::yellow(&trimmed.display().to_string())
                })
                .collect();
            format!(" {}", formatted.join(", "))
        };
        eprint!(
            "\n{} `{}`{}  [y]es / [a]lways / [N]o: ",
            style::yellow("approve"),
            style::bold(tool),
            subjects_fmt
        );
        std::io::stderr().flush().ok();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return ApprovalChoice::Deny;
        }
        match line.trim().to_lowercase().as_str() {
            "y" | "yes" => ApprovalChoice::Allow,
            "a" | "always" => ApprovalChoice::AllowAlways(tool.to_string()),
            _ => ApprovalChoice::Deny,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install a colored error formatter so top-level anyhow errors use the same style as inline
    // `eprintln!("{} {e:#}", style::red("error:"))` calls rather than a bare `Error: …` line.
    // We do this before `style::init` so even parse errors (before color flags are known) get color
    // when stderr is a tty — safe because `style::init` defaults to auto.
    style::init(style::ColorChoice::Auto);
    // `flux auth …` is a distinct mode; everything else is the prompt runner.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(|s| s == "auth").unwrap_or(false) {
        return run_auth(&argv[2..]).await;
    }
    if argv.get(1).map(|s| s == "plugin").unwrap_or(false) {
        return run_plugin(&argv[2..]);
    }
    if argv.get(1).map(|s| s == "sessions").unwrap_or(false) {
        return run_sessions();
    }
    // `flux run <app.flux>` runs a multi-agent program — but only when the next arg actually looks
    // like a program file (a `.flux` path or an existing file), so a normal prompt opening with the
    // word "run" (e.g. `flux run the tests`) still falls through to the agent.
    if argv.get(1).map(|s| s == "run").unwrap_or(false)
        && argv
            .get(2)
            .map(|p| p.ends_with(".flux") || std::path::Path::new(p).is_file())
            .unwrap_or(false)
    {
        return run_app_cmd(&argv[2..]).await;
    }
    if argv.get(1).map(|s| s == "flow").unwrap_or(false) {
        return run_flow(&argv[2..]).await;
    }
    if argv.get(1).map(|s| s == "preset").unwrap_or(false) {
        return preset::run_preset(&argv[2..]).await;
    }
    let cli = Cli::parse();
    style::init(cli.color);
    // `-v`/`--verbose` exports `FLUX_VERBOSE` so the sinks (and any sub-process) show full output.
    if cli.verbose {
        std::env::set_var("FLUX_VERBOSE", "1");
    }
    if let Some(addr) = cli.serve.clone() {
        run_serve(cli, addr).await
    } else if cli.tui {
        run_tui(cli).await
    } else if cli.plan {
        run_plan(cli)
            .await
            .map_err(|e| {
                eprintln!("{} {e:#}", style::red("error:"));
                std::process::exit(1);
            })
            .unwrap_or(());
        Ok(())
    } else if cli.prompt.is_empty() && !cli.print {
        // No prompt and not one-shot → interactive agentic REPL.
        run_repl(cli)
            .await
            .map_err(|e| {
                eprintln!("{} {e:#}", style::red("error:"));
                std::process::exit(1);
            })
            .unwrap_or(());
        Ok(())
    } else {
        run_prompt(cli)
            .await
            .map_err(|e| {
                eprintln!("{} {e:#}", style::red("error:"));
                std::process::exit(1);
            })
            .unwrap_or(());
        Ok(())
    }
}

/// `flux run <app.flux>` — load and run a multi-agent flux **Program** through the `flux-app` host
/// (event bus + triggers + journeys). A bare single-flow file is accepted too. The provider is
/// best-effort: a program built only from pure ops runs without credentials; model-backed ops need a
/// resolvable `provider/model` (defaulting like the prompt path) and degrade with a clear note.
async fn run_app_cmd(args: &[String]) -> Result<()> {
    use std::path::Path;

    let mut path: Option<&str> = None;
    let mut model_spec: Option<String> = None;
    let mut auto_approve = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-m" | "--model" => {
                model_spec = args.get(i + 1).cloned();
                i += 2;
            }
            // By default destructive ops in the program are DENIED (no human at a prompt); `--yes`
            // opts into allow-all for a trusted, pre-authored program.
            "-y" | "--yes" => {
                auto_approve = true;
                i += 1;
            }
            s if !s.starts_with('-') && path.is_none() => {
                path = Some(s);
                i += 1;
            }
            _ => i += 1,
        }
    }
    let path = path
        .ok_or_else(|| anyhow::anyhow!("usage: flux run <app.flux> [-m provider/model] [--yes]"))?;
    let spec = model_spec.unwrap_or_else(|| "anthropic/claude-sonnet-4-6".to_string());

    // Best-effort: a pure-op program runs without a provider. `build_provider` resolves the model
    // alias (`-m opus`/`sonnet`/…) to a real id, so the cognition pack gets a valid model.
    let (provider, model): (Option<std::sync::Arc<dyn Provider>>, String) = match build_provider(
        &spec,
    ) {
        Ok((native, resolved)) => (Some(std::sync::Arc::new(native)), resolved),
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
                .unwrap_or(&spec)
                .to_string();
            (None, m)
        }
    };

    flux_app::run_program_file(Path::new(path), provider, model, auto_approve)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Launch the HTTP API daemon.
async fn run_serve(cli: Cli, addr: String) -> Result<()> {
    if !cli.yes {
        bail!("`--serve` requires `--yes` (HTTP requests have no interactive approver)");
    }
    // The daemon auto-approves every tool call, so an unauthenticated listener is remote code
    // execution. Require a bearer token (`FLUX_SERVER_TOKEN`) for any non-loopback bind; loopback
    // may run tokenless for local use.
    let token = std::env::var("FLUX_SERVER_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    if token.is_none() && !addr_is_loopback(&addr) {
        bail!(
            "refusing to serve on a non-loopback address ({addr}) without authentication — set \
             FLUX_SERVER_TOKEN to require `Authorization: Bearer <token>`, or bind 127.0.0.1"
        );
    }
    let (agent, _session_id, _spawner) = build_agent(&cli).await?;
    flux_server::serve(&addr, agent, token).await
}

/// Whether `addr` (host:port or bare host) binds only the loopback interface.
fn addr_is_loopback(addr: &str) -> bool {
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
async fn run_tui(cli: Cli) -> Result<()> {
    let auto_approve = cli.yes;
    let (agent, session_id, _spawner) = build_agent(&cli).await?;
    flux_tui::run(agent, session_id, auto_approve).await
}

/// `flux plugin add <name> <program> [args…] | ls | pin <name> <version> | rollback <name>`.
fn run_plugin(args: &[String]) -> Result<()> {
    let dir = plugins_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    match args.first().map(String::as_str) {
        Some("ls") | None => {
            let found = flux_plugin::discover(&dir);
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
                println!(
                    "{:<16} {} {}{pin}",
                    p.name,
                    p.descriptor.program,
                    p.descriptor.args.join(" ")
                );
            }
            Ok(())
        }
        Some("add") => {
            let name = args
                .get(1)
                .context("usage: flux plugin add <name> <program> [args…]")?;
            let program = args
                .get(2)
                .context("usage: flux plugin add <name> <program> [args…]")?;
            let rest: Vec<String> = args.get(3..).unwrap_or(&[]).to_vec();
            flux_plugin::add_descriptor(
                &dir,
                name,
                &flux_plugin::PluginDescriptor {
                    program: program.clone(),
                    args: rest,
                    pinned: None,
                },
            )
            .context("write plugin descriptor")?;
            println!("added plugin `{name}` → {program}");
            Ok(())
        }
        Some("pin") => {
            let name = args
                .get(1)
                .context("usage: flux plugin pin <name> <version>")?;
            let version = args
                .get(2)
                .context("usage: flux plugin pin <name> <version>")?;
            flux_plugin::set_pinned(&dir, name, Some(version.clone())).context("pin plugin")?;
            println!("pinned `{name}` to {version}");
            Ok(())
        }
        Some("rollback") => {
            let name = args.get(1).context("usage: flux plugin rollback <name>")?;
            flux_plugin::set_pinned(&dir, name, None).context("rollback plugin")?;
            println!("cleared pin on `{name}`");
            Ok(())
        }
        Some(other) => bail!("unknown `flux plugin` command `{other}` (try ls|add|pin|rollback)"),
    }
}

/// `flux auth status | login <provider>`.
async fn run_auth(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("status") | None => {
            for s in flux_credentials::auth_status() {
                let mark = if s.available { "✓" } else { "·" };
                println!("{mark} {:<11} {}", s.provider, s.source);
            }
            Ok(())
        }
        Some("login") => match args.get(1).map(String::as_str).unwrap_or("") {
            "claude" => login_claude().await,
            "codex" => bail!(
                "codex login: sign in with the Codex CLI — flux imports `~/.codex/auth.json` automatically"
            ),
            other => bail!("`flux auth login` expects `claude` (got `{other}`)"),
        },
        Some(other) => {
            bail!("unknown `flux auth` command `{other}` (try `status` or `login claude`)")
        }
    }
}

/// Interactive Anthropic (Claude subscription) PKCE login.
async fn login_claude() -> Result<()> {
    let pkce = flux_credentials::generate_pkce();
    let state = flux_credentials::generate_state();
    let url = flux_credentials::anthropic_authorize_url(&pkce, &state);
    println!(
        "Open this URL, approve access, then paste the code from the callback page:\n\n{url}\n"
    );
    print!("code: ");
    std::io::stdout().flush().ok();
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    flux_credentials::anthropic_exchange_and_store(code.trim(), &state, &pkce.verifier)
        .await
        .context("exchange authorization code")?;
    println!("\u{2713} stored Claude subscription credentials in ~/.flux/credentials.toml");
    Ok(())
}

/// Run a one-shot prompt turn.
async fn run_prompt(cli: Cli) -> Result<()> {
    let prompt = cli.prompt.join(" ");

    if prompt.trim().is_empty() {
        bail!("provide a prompt, e.g. `flux \"summarize the README\"`");
    }

    // One engine: a prompt always runs the agentic Flux-Lang engine. `-p`/`--print` only means
    // print-and-exit (a chat-only turn just answers in prose; pass `--yes` for non-interactive
    // tool approval). The legacy tool-less raw-completion path is gone — there is one engine.
    run_agentic(&cli, prompt).await
}

#[cfg(test)]
mod tests {
    use super::{tool_preview, truncate};

    #[test]
    fn truncate_caps_with_ellipsis() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 3), "hel…");
    }

    #[test]
    fn tool_preview_single_line_unchanged() {
        assert_eq!(tool_preview("no matches", false), "no matches");
    }

    #[test]
    fn tool_preview_caps_lines_by_default_and_shows_all_when_full() {
        // Default: up to 40 lines shown, the rest counted (with a `-v for full` hint).
        let many: String = (1..=50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let p = tool_preview(&many, false);
        assert!(p.contains("line 40"), "40th line shown: {p}");
        assert!(!p.contains("line 41"), "41st line elided: {p}");
        assert!(
            p.contains("(+10 more lines; -v for full)"),
            "elision note: {p}"
        );
        assert!(p.contains("\n  line 2"), "continuation lines indented: {p}");

        // Full (`-v`): every line shown, no elision note.
        let p = tool_preview(&many, true);
        assert!(p.contains("line 50"), "all lines shown when full: {p}");
        assert!(!p.contains("more lines"), "no elision note when full: {p}");
    }

    #[test]
    fn tool_preview_caps_a_long_single_line_unless_full() {
        let p = tool_preview(&"x".repeat(600), false);
        assert!(p.ends_with('…'));
        assert!(p.chars().count() <= 501);
        // Full: the whole line, untruncated.
        let p = tool_preview(&"x".repeat(600), true);
        assert_eq!(p.chars().count(), 600);
        assert!(!p.ends_with('…'));
    }
}
