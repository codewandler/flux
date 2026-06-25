//! The `flux` binary.
//!
//! M0 surface: a one-shot mode that streams a single Anthropic response to stdout. The
//! interactive REPL and TUI land in M2; this establishes the end-to-end path
//! (CLI → provider → stream → render).

use std::io::Write;

use anyhow::{bail, Context, Result};
use clap::Parser;
use futures::StreamExt;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_agent::{Agent, AgentSink, DEFAULT_SYSTEM_PROMPT};
use flux_anthropic::anthropic_from_env;
use flux_context::{EnvContext, GitContext, ProjectFiles, Projector, RepoSignal};
use flux_core::{Chunk, ContentBlock, StopReason, Usage};
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

/// flux — a Rust agent harness.
#[derive(Parser, Debug)]
#[command(name = "flux", version, about = "flux — a Rust agent harness")]
struct Cli {
    /// The prompt (joined with spaces if given as multiple words).
    prompt: Vec<String>,

    /// One-shot, non-interactive mode (print the response and exit).
    #[arg(short = 'p', long = "print")]
    print: bool,

    /// `provider/model` (provider ∈ anthropic|claude|openai|codex|openrouter; default anthropic).
    /// Bare aliases `sonnet|opus|haiku` resolve against Anthropic. E.g. `openrouter/anthropic/claude-sonnet-4.5`.
    /// Overrides `model` in `.flux/config.toml`; falls back to `sonnet`.
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// Enable adaptive thinking.
    #[arg(long)]
    think: bool,

    /// Reasoning effort (model-dependent; some models reject it).
    #[arg(long, value_enum)]
    effort: Option<EffortArg>,

    /// Maximum tokens to generate.
    #[arg(long, default_value_t = 4096)]
    max_tokens: u32,

    /// Print token usage to stderr when the turn completes.
    #[arg(long)]
    usage: bool,

    /// Agentic mode: enable tools (read/write/edit/bash) under the safety envelope, persist
    /// the session, and loop until the model stops calling tools.
    #[arg(long)]
    agent: bool,

    /// Auto-approve every tool call (headless). Without it, unmatched calls prompt for approval.
    #[arg(long)]
    yes: bool,

    /// Launch the ratatui chat TUI (requires a real terminal; currently also needs `--yes`).
    #[arg(long)]
    tui: bool,

    /// Continue the most recent session instead of starting a new one.
    #[arg(short = 'c', long)]
    continue_: bool,

    /// Bind a long-running HTTP API daemon at this address (e.g. 127.0.0.1:8787).
    #[arg(long)]
    serve: Option<String>,
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
            Ok(()) => eprintln!("\x1b[2m(saved updated permissions to .flux/config.toml)\x1b[0m"),
            Err(e) => eprintln!("\x1b[2m(could not save permissions: {e})\x1b[0m"),
        }
    }
}

const KNOWN_PROVIDERS: &[&str] = &["anthropic", "claude", "openai", "codex", "openrouter"];

/// Parse a `provider/model` spec (default provider `anthropic`) and build the matching provider
/// from environment credentials. Returns the live provider plus the resolved concrete model id.
fn build_provider(spec: &str) -> Result<(NativeProvider, String)> {
    let (provider, model) = match spec.split_once('/') {
        Some((p, m)) if KNOWN_PROVIDERS.contains(&p) => (p.to_string(), m.to_string()),
        _ => ("anthropic".to_string(), spec.to_string()),
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
fn run_sessions() -> Result<()> {
    let store = open_session_store()?;
    let sessions = store.list(30)?;
    if sessions.is_empty() {
        eprintln!("no sessions yet — start one with `flux` or `flux --agent`");
        return Ok(());
    }
    for s in sessions {
        println!(
            "{}  {:>3} msg  {:<22} {}",
            s.id,
            s.messages,
            s.model,
            fmt_age(s.created_at_ms)
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

/// Build a fresh boxed provider for a model spec (used by the sub-agent factory).
fn provider_for(spec: &str) -> Result<Box<dyn Provider>> {
    if spec == "mock" || spec.starts_with("mock/") {
        Ok(Box::<MockCliProvider>::default())
    } else {
        let (native, _model) = build_provider(spec)?;
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
async fn build_agent(cli: &Cli) -> Result<(Agent, String, Arc<dyn flux_runtime::Spawner>)> {
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
    registry.register(Arc::new(TaskTool));

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
                Err(e) => eprintln!("\x1b[2m(plugin `{}` failed to load: {e})\x1b[0m", p.name),
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

    let store = Arc::new(open_session_store()?);
    let session_id = if cli.continue_ {
        store
            .latest_session_id()
            .context("latest session")?
            .ok_or_else(|| anyhow::anyhow!("no session to continue"))?
    } else {
        store.create_session(&model).context("create session")?
    };

    let agent = Agent {
        provider,
        executor,
        store,
        model,
        system_prompt,
        max_tokens: cli.max_tokens,
        max_iterations: 25,
        skills: load_skills(&cwd),
        compact_threshold_chars: compact_threshold(),
    };
    Ok((agent, session_id, spawner))
}

/// One-shot agentic turn.
async fn run_agentic(cli: &Cli, prompt: String) -> Result<()> {
    let (agent, session_id, _spawner) = build_agent(cli).await?;
    eprintln!("\x1b[2m[session {session_id} · {}]\x1b[0m", agent.model);
    let initial_rules = agent.executor.allow_rules();
    let mut sink = CliSink;
    agent
        .run_turn(&session_id, &prompt, &mut sink)
        .await
        .context("agent turn")?;
    persist_new_rules(&initial_rules, &agent.executor.allow_rules());
    Ok(())
}

/// A minimal `reedline` prompt: a single `› ` indicator (no left/right segments).
struct FluxPrompt;

impl Prompt for FluxPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("› ")
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
        "\x1b[2mflux · {} · session {session_id} — /help, Ctrl-C interrupts a turn, Ctrl-D exits\x1b[0m",
        agent.model
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
    let prompt = FluxPrompt;

    loop {
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
                "help" => eprintln!(
                    "commands: /help  /tools  /model <spec>  /session  /sessions  /resume <id>  \
                     /clear  /pd <goal>  /goal <cond>  /loop <n> <task>  /exit"
                ),
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
                        eprintln!("\x1b[2mplan-and-dispatch (dependency waves)…\x1b[0m");
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
                                Err(e) => eprintln!("\x1b[31merror:\x1b[0m {e}"),
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
                        for s in list {
                            let here = if s.id == session_id { "*" } else { " " };
                            eprintln!(
                                "{here} {}  {:>3} msg  {:<20} {}",
                                s.id,
                                s.messages,
                                s.model,
                                fmt_age(s.created_at_ms)
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
        // Run the turn interruptibly: the first Ctrl-C cancels it (without killing the REPL); the
        // turn unwinds cleanly and we return to the prompt. (Ctrl-D exits.)
        let agent_ref = &agent;
        let sid_ref = session_id.as_str();
        run_interruptible(move |c| async move {
            let mut sink = CliSink;
            if let Err(e) = agent_ref
                .run_turn_cancellable(sid_ref, input, &mut sink, &c)
                .await
            {
                eprintln!("\x1b[31merror:\x1b[0m {e}");
            }
        })
        .await;
    }
    persist_new_rules(&initial_rules, &agent.executor.allow_rules());
    Ok(())
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
                    eprintln!("\n\x1b[2m(interrupting…)\x1b[0m");
                }
            }
        }
    }
}

/// `/goal <cond>`: drive turns toward a goal, asking a cheap `evaluator` sub-agent after each turn
/// whether the goal is satisfied; stop on SATISFIED, max-iterations, or cancellation.
async fn run_goal(
    agent: &Agent,
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
        eprintln!("\x1b[2m[goal {}/{}]\x1b[0m", i + 1, MAX);
        let mut sink = GoalSink::default();
        if let Err(e) = agent
            .run_turn_cancellable(session_id, &next_input, &mut sink, &cancel)
            .await
        {
            eprintln!("\x1b[31merror:\x1b[0m {e}");
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
                eprintln!("\x1b[2m(evaluator error: {e})\x1b[0m");
                return;
            }
        };
        // Match only a leading verdict so "not satisfied"/"unsatisfied" don't false-positive.
        if verdict.trim().to_uppercase().starts_with("SATISFIED") {
            eprintln!("\x1b[2m[goal satisfied]\x1b[0m");
            return;
        }
        next_input = verdict
            .split_once(':')
            .map(|(_, r)| r.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| goal.to_string());
    }
    eprintln!("\x1b[2m[goal loop ended]\x1b[0m");
}

/// `/loop <count> <task>`: run `task` up to `count` times (stops early on cancellation).
async fn run_loop(
    agent: &Agent,
    session_id: &str,
    count: usize,
    task: &str,
    cancel: tokio_util::sync::CancellationToken,
) {
    for i in 0..count {
        if cancel.is_cancelled() {
            break;
        }
        eprintln!("\x1b[2m[loop {}/{}]\x1b[0m", i + 1, count);
        let mut sink = CliSink;
        if let Err(e) = agent
            .run_turn_cancellable(session_id, task, &mut sink, &cancel)
            .await
        {
            eprintln!("\x1b[31merror:\x1b[0m {e}");
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

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let head: String = s.chars().take(n).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

/// Renders streaming text to stdout and tool activity to stderr.
#[derive(Default)]
struct CliSink;

impl AgentSink for CliSink {
    fn text_delta(&mut self, t: &str) {
        print!("{t}");
        std::io::stdout().flush().ok();
    }
    fn thinking_delta(&mut self, t: &str) {
        // Stream extended-thinking tokens dimmed on stderr so reasoning is observable in the REPL
        // (was a silent no-op); kept off stdout so it doesn't pollute piped output.
        eprint!("\x1b[2m{t}\x1b[0m");
        std::io::stderr().flush().ok();
    }
    fn tool_call(&mut self, name: &str, input: &Value) {
        eprintln!(
            "\n\x1b[2m→ {name} {}\x1b[0m",
            truncate(&input.to_string(), 120)
        );
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        let tag = if result.is_error { "✗" } else { "✓" };
        eprintln!(
            "\x1b[2m{tag} {name}: {}\x1b[0m",
            truncate(result.content.trim(), 200)
        );
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        if o.kind == flux_evidence::KIND_DESTRUCTIVE {
            eprintln!("\x1b[33m⚠ destructive operation flagged — approval required\x1b[0m");
        } else if o.kind == "skill.activated" {
            if let Some(name) = o.data.get("skill").and_then(|v| v.as_str()) {
                eprintln!("\x1b[2m✦ skill activated: {name}\x1b[0m");
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
            eprintln!("\x1b[2m⊙ context compacted ({from} → {to} messages)\x1b[0m");
        } else if o.kind == "turn.cancelled" {
            eprintln!("\x1b[2m⊘ turn cancelled\x1b[0m");
        }
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        println!();
        if let Some(u) = usage {
            let cache = if u.cache_creation_input_tokens > 0 || u.cache_read_input_tokens > 0 {
                format!(
                    " cache_w={} cache_r={}",
                    u.cache_creation_input_tokens, u.cache_read_input_tokens
                )
            } else {
                String::new()
            };
            eprintln!(
                "\x1b[2m[usage in={} out={}{cache}]\x1b[0m",
                u.input_tokens, u.output_tokens
            );
        }
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
            "\n\x1b[2m→ {name} {}\x1b[0m",
            truncate(&input.to_string(), 120)
        );
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        let tag = if result.is_error { "✗" } else { "✓" };
        eprintln!(
            "\x1b[2m{tag} {name}: {}\x1b[0m",
            truncate(result.content.trim(), 200)
        );
    }
    fn turn_end(&mut self, _usage: Option<Usage>) {
        println!();
    }
}

/// A built-in offline provider (`-m mock`): call 1 writes `flux-mock.txt` via the `write` tool,
/// call 2 returns a summary and stops. Lets the agentic loop be exercised end-to-end with no
/// network — useful for `flux --agent --yes -m mock` smoke tests.
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

        // Test hook: `FLUX_MOCK_TOOL=<name>` (+ optional `FLUX_MOCK_TOOL_INPUT=<json>`) makes call 1
        // emit a tool call for any registered tool — used to exercise tools end-to-end via the CLI.
        if let Ok(tool) = std::env::var("FLUX_MOCK_TOOL") {
            let input: serde_json::Value = std::env::var("FLUX_MOCK_TOOL_INPUT")
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let chunks = if n == 0 {
                vec![
                    Chunk::TextDelta(format!("Calling `{tool}`.")),
                    Chunk::Block(ContentBlock::Text {
                        text: format!("Calling `{tool}`."),
                    }),
                    Chunk::Block(ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: tool,
                        input,
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::ToolUse),
                    },
                ]
            } else {
                vec![
                    Chunk::TextDelta("Finished.".into()),
                    Chunk::Block(ContentBlock::Text {
                        text: "Finished.".into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }

        // Test hook: `FLUX_MOCK_BASH=<cmd>` makes call 1 emit a `bash` tool call with that command
        // (used to exercise the destructive-command approval gate end-to-end via the CLI).
        if let Ok(cmd) = std::env::var("FLUX_MOCK_BASH") {
            let chunks = if n == 0 {
                vec![
                    Chunk::TextDelta(format!("Running `{cmd}`.")),
                    Chunk::Block(ContentBlock::Text {
                        text: format!("Running `{cmd}`."),
                    }),
                    Chunk::Block(ContentBlock::ToolUse {
                        id: "b1".into(),
                        name: "bash".into(),
                        input: serde_json::json!({ "command": cmd }),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::ToolUse),
                    },
                ]
            } else {
                vec![
                    Chunk::TextDelta("Finished.".into()),
                    Chunk::Block(ContentBlock::Text {
                        text: "Finished.".into(),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::EndTurn),
                    },
                ]
            };
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }

        let chunks = if n == 0 {
            vec![
                Chunk::TextDelta("I'll create the file.".into()),
                Chunk::Block(ContentBlock::Text {
                    text: "I'll create the file.".into(),
                }),
                Chunk::Block(ContentBlock::ToolUse {
                    id: "m1".into(),
                    name: "write".into(),
                    input: serde_json::json!({
                        "path": "flux-mock.txt",
                        "content": "created by flux mock\n"
                    }),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ]
        } else {
            vec![
                Chunk::TextDelta("Done — wrote flux-mock.txt.".into()),
                Chunk::Block(ContentBlock::Text {
                    text: "Done — wrote flux-mock.txt.".into(),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ]
        };
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
        eprint!("\n\x1b[33mapprove\x1b[0m `{tool}` {subjects:?}  [y]es / [a]lways / [N]o: ");
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
    let cli = Cli::parse();
    if let Some(addr) = cli.serve.clone() {
        run_serve(cli, addr).await
    } else if cli.tui {
        run_tui(cli).await
    } else if cli.prompt.is_empty() && !cli.print {
        // No prompt and not one-shot → interactive agentic REPL.
        run_repl(cli).await
    } else {
        run_prompt(cli).await
    }
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

/// Launch the ratatui chat TUI. The TUI installs its own modal approver, so `--yes` is not required
/// (tool calls raise an in-TUI y/a/N prompt).
async fn run_tui(cli: Cli) -> Result<()> {
    let (agent, session_id, _spawner) = build_agent(&cli).await?;
    flux_tui::run(agent, session_id).await
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
        // The interactive REPL arrives in M2; for now a prompt is required.
        bail!("provide a prompt, e.g. `flux -p \"hello\"` (interactive mode lands in M2)");
    }

    if cli.agent {
        return run_agentic(&cli, prompt).await;
    }

    let cfg = std::env::current_dir()
        .ok()
        .map(|cwd| flux_config::load(&cwd))
        .transpose()
        .context("load .flux/config.toml")?
        .unwrap_or_default();
    let model_spec = resolve_model_spec(&cli.model, &cfg);
    let (provider, model) = build_provider(&model_spec)?;

    let mut req = Request::new(model, prompt).with_max_tokens(cli.max_tokens);
    if cli.think {
        req = req.with_thinking(true);
    }
    if let Some(effort) = cli.effort {
        req = req.with_effort(effort.into());
    }

    let mut stream = provider
        .stream(req)
        .await
        .context("failed to start the response stream")?;

    let mut stdout = std::io::stdout();
    let mut in_thinking = false;
    let mut final_usage = None;

    while let Some(chunk) = stream.next().await {
        match chunk.context("stream error")? {
            Chunk::TextDelta(text) => {
                if in_thinking {
                    // Close the dimmed thinking block before visible output.
                    eprintln!("\x1b[0m");
                    in_thinking = false;
                }
                print!("{text}");
                stdout.flush().ok();
            }
            Chunk::ThinkingDelta(text) => {
                if !in_thinking {
                    eprint!("\x1b[2m[thinking] ");
                    in_thinking = true;
                }
                eprint!("{text}");
            }
            Chunk::Usage(u) => final_usage = Some(u),
            Chunk::Done { .. } => {}
            Chunk::MessageStart { .. } | Chunk::Block(_) => {}
        }
    }

    println!();

    if cli.usage {
        if let Some(u) = final_usage {
            eprintln!(
                "[usage] in={} out={} cache_w={} cache_r={} total={}",
                u.input_tokens,
                u.output_tokens,
                u.cache_creation_input_tokens,
                u.cache_read_input_tokens,
                u.total()
            );
        }
    }

    Ok(())
}
