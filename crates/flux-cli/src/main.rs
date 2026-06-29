//! The `flux` binary.
//!
//! M0 surface: a one-shot mode that streams a single Anthropic response to stdout. The
//! interactive REPL and TUI land in M2; this establishes the end-to-end path
//! (CLI → provider → stream → render).

mod plugin_skill;
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

use flux_agent::{AgentSpec, DEFAULT_SYSTEM_PROMPT};
use flux_core::{Chunk, ContentBlock, StopReason, Usage};
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_flow::state::FlowStore;
use flux_flow::AgentSink;
use flux_orchestrate::{ProviderFactory, Role, RoleRegistry, SubAgents, TaskTool};
use flux_provider::{ChunkStream, Effort, NativeProvider, Provider, Request};
use flux_providers::anthropic::anthropic_from_env;
use flux_providers::openai::{ollama_api, openai_from_env, openrouter_from_env};
use flux_runtime::context::{EnvContext, GitContext, ProjectFiles, Projector, RepoSignal};
use flux_runtime::{
    AllowApprover, ApprovalChoice, Approver, Executor, PermissionManager, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_spec::IntentSet;
use flux_system::{System, Workspace};
use reedline::{FileBackedHistory, Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};
use std::borrow::Cow;

/// flux — the LLM plans, the runtime runs.
#[derive(Parser, Debug)]
#[command(
    name = "flux",
    version,
    about = "flux — the LLM plans, the runtime runs",
    long_about = "flux — the LLM plans, the runtime runs.\n\n\
        Run the agent with `flux run <prompt>`; with no arguments, `flux` opens the interactive REPL. \
        The other entry points are subcommands too: `flux plan <prompt>` reviews a plan before running, \
        `flux tui` is the chat UI, and `flux serve <addr>` is the HTTP daemon. Run `flux help` for the \
        full list of commands."
)]
struct Cli {
    /// A subcommand (run `flux help` to list them). With none, `flux` opens the interactive REPL.
    #[command(subcommand)]
    command: Option<Commands>,

    /// When to colorize output: auto (a terminal, `NO_COLOR` unset), always, or never.
    #[arg(long, value_enum, default_value_t, global = true)]
    color: style::ColorChoice,
}

/// The flags for running an agent turn — flattened into each agent-path subcommand (`run`, `plan`,
/// `tui`, `serve`), so they live on those commands and stay off every other subcommand's help.
/// (`--color` is `global` on [`Cli`] instead; it applies to every command.)
#[derive(clap::Args, Debug)]
struct AgentFlags {
    /// (Hidden) Non-interactive print mode — a bare prompt is already one-shot, so this is a no-op alias.
    #[arg(short = 'p', long = "print", hide = true)]
    print: bool,

    /// Fully-qualified `provider/model` spec. Provider must be one of:
    ///   `anthropic` (API key), `claude` (OAuth/subscription), `openai`, `codex`, `openrouter`
    ///   (OpenAI Chat wire), `openrouter-anthropic` (OpenRouter's native Messages endpoint —
    ///   leak-proof tool calls), `ollama` (local, OpenAI Chat wire), `ollama-anthropic` (local
    ///   Messages endpoint). Short aliases `sonnet`, `opus`, `haiku` are shorthands for
    ///   `anthropic/<model>`.
    /// Examples: `claude/claude-sonnet-4-6`, `openai/gpt-4o`, `openrouter-anthropic/z-ai/glm-4.6`.
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

    /// Reveal the agent loop: stream the loop-machinery ops (`plan`/`run_plan`/`observe`/…) that are
    /// filtered from the surface by default, so you can watch each turn iterate. Also enabled by
    /// `FLUX_SHOW_LOOP`. See `flux loop show` for the loop itself and `/evidence` for the audit trail.
    #[arg(long)]
    show_loop: bool,

    /// Continue the most recent session instead of starting a new one.
    #[arg(short = 'c', long)]
    continue_: bool,

    /// Resume the most recent session (equivalent to --continue; used by hot-reload).
    #[arg(long)]
    resume: bool,

    /// Dev mode: enables hot-reload (`flux_reload` tool) and other developer tools.
    #[arg(long)]
    dev: bool,
}

/// A standalone parser wrapper used only to materialize a default-populated [`AgentFlags`] from
/// synthesized args (see [`AgentFlags::from_model_yes`]). Going through clap preserves field defaults
/// like `max_tokens` that a hand-built `Default` would zero out.
#[derive(Parser, Debug)]
struct AgentFlagsOnly {
    #[command(flatten)]
    agent: AgentFlags,
}

impl AgentFlags {
    /// Build agent flags from just a model spec + `--yes` — the entry points (`flux flow run`,
    /// `flux preset --run`, and the bare `flux` REPL) that run an agent without the full turn-flag CLI.
    /// Preserves clap's field defaults (e.g. `max_tokens = 16384`). The args are synthesized here, so
    /// the parse never fails.
    fn from_model_yes(model: Option<&str>, yes: bool) -> Self {
        let mut args: Vec<String> = vec!["flux".to_string()];
        if yes {
            args.push("--yes".to_string());
        }
        if let Some(m) = model {
            args.push("-m".to_string());
            args.push(m.to_string());
        }
        AgentFlagsOnly::parse_from(&args).agent
    }
}

/// The flux subcommands. Each renders its own `flux <cmd> --help`. With no subcommand, `flux` opens
/// the interactive REPL; any unrecognized first token is a clap "unrecognized subcommand" error (so a
/// stray word never launches an autonomous turn — use `flux run <prompt>`).
#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Run the agent on a prompt, or a multi-agent program: `flux run <prompt…>` / `flux run <app.flux>`.
    Run {
        #[command(flatten)]
        agent: AgentFlags,
        /// The prompt words, or a path to an `<app.flux>` multi-agent program. Agent flags
        /// (`-m`, `--yes`, …) may appear before or after.
        prompt: Vec<String>,
    },
    /// Plan mode: compile the prompt to a Flux-Lang plan and show it (without running it by default).
    /// On a terminal it then asks `run it? [y/N]`; piped or with `-o json|yaml` it prints the plan and
    /// exits (never runs).
    Plan {
        #[command(flatten)]
        agent: AgentFlags,
        /// Plan output format when not running it: json, yaml, or pretty (default).
        #[arg(short = 'o', long, value_enum)]
        output: Option<OutputFormat>,
        /// The prompt to compile into a plan.
        prompt: Vec<String>,
    },
    /// Launch the ratatui chat TUI (requires a real terminal). Tool calls raise a y/a/N modal; pass
    /// `--yes` to auto-approve all calls without a modal.
    Tui {
        #[command(flatten)]
        agent: AgentFlags,
    },
    /// Bind a long-running HTTP API daemon (REST + SSE). Requires `--yes` (HTTP has no interactive
    /// approver); a non-loopback bind requires `FLUX_SERVER_TOKEN`.
    Serve {
        #[command(flatten)]
        agent: AgentFlags,
        /// Address to bind, e.g. `127.0.0.1:8787`.
        addr: String,
    },
    /// Connect to a remote A2A agent and chat with it like a local agent. With prompt words or
    /// piped stdin it runs a single turn and exits; otherwise it opens an interactive REPL.
    A2a {
        /// Remote agent base URL (e.g. `http://127.0.0.1:8787`) or a full `/a2a` endpoint URL.
        url: String,
        /// Optional one-shot prompt. If empty and stdin is a TTY, the REPL opens instead.
        prompt: Vec<String>,
        /// Bearer token for a gated endpoint (falls back to `FLUX_A2A_TOKEN`).
        #[arg(long)]
        token: Option<String>,
    },
    /// Run a benchmark suite against flux and print a summary.
    #[command(
        after_help = "ADAPTERS:\n  synthetic       real-model coding riddles (fast, no Docker)\n  mock            offline CI fixture (drives -m mock)\n  terminal-bench  the real Docker benchmark\n  multi           several behind one combined score (with --members)\n\nEXAMPLES:\n  flux eval synthetic -m openrouter-anthropic/anthropic/claude-sonnet-4.6 --watch --report r.md\n  flux eval multi --members synthetic,terminal-bench"
    )]
    Eval {
        /// Which suite to run: synthetic | mock | terminal-bench | multi.
        adapter: String,
        /// Model the suite's agent runs (e.g. `-m mock`, `-m openrouter-anthropic/anthropic/claude-sonnet-4.6`).
        #[arg(short = 'm', long)]
        model: Option<String>,
        /// Restrict to these task ids (comma-separated).
        #[arg(long, value_delimiter = ',')]
        tasks: Vec<String>,
        /// For `multi`: the member adapters to combine (comma-separated).
        #[arg(long, value_delimiter = ',')]
        members: Vec<String>,
        /// Cap the number of tasks (0 = all).
        #[arg(long, default_value_t = 0)]
        limit: u64,
        /// Trials per task (>1 averages out single-run model noise).
        #[arg(long, default_value_t = 1)]
        trials: u64,
        /// Write a categorized Markdown report to this path.
        #[arg(long)]
        report: Option<String>,
        /// Stream each task's agent activity to the terminal live.
        #[arg(long)]
        watch: bool,
    },
    /// Run a multi-agent program with its event-trigger channels (cron / webhook / Slack).
    App {
        #[command(subcommand)]
        action: AppAction,
    },
    /// Run a single behavioral loop (a Flux-Lang flow — native text, or a pre-compiled DraftAst JSON file).
    Flow {
        #[command(subcommand)]
        action: FlowAction,
    },
    /// Inspect or customize the agent loop (`assets/agent-loop.flux`).
    Loop {
        #[command(subcommand)]
        action: Option<LoopAction>,
    },
    /// List recent sessions (newest first).
    Sessions {
        /// Delete all zero-message (abandoned) sessions.
        #[arg(long)]
        prune: bool,
    },
    /// Provider authentication (status / login).
    Auth {
        #[command(subcommand)]
        action: Option<AuthAction>,
    },
    /// Manage subprocess plugins (any-language ops).
    Plugin {
        #[command(subcommand)]
        action: Option<PluginAction>,
    },
    /// Print a shell completion script to stdout (defaults to fish).
    Completion {
        /// Shell to generate for: bash | zsh | fish | powershell | elvish.
        shell: Option<String>,
    },
    /// Scaffold or run a parameterized flow recipe.
    Preset {
        /// `list` | `<name> key=value …` (passed through to the preset cookbook).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// `flux app …`
#[derive(clap::Subcommand, Debug)]
enum AppAction {
    /// Run a `.flux` program and serve its declared channels until Ctrl-C. A program with cron/webhook/
    /// Slack channels runs as a background daemon; one with only a `cli` channel (or none) reads stdin.
    Run {
        #[command(flatten)]
        agent: AgentFlags,
        /// Path to the `<program.flux>` multi-agent program.
        program: String,
    },
}

/// `flux flow …`
#[derive(clap::Subcommand, Debug)]
enum FlowAction {
    /// Run a checked-in Flux-Lang program file.
    Run {
        /// Path to the `.flux` loop — native Flux-Lang text, or a checked-in DraftAst JSON.
        file: String,
        /// Model for the program's agent steps.
        #[arg(short = 'm', long)]
        model: Option<String>,
        /// Auto-approve every tool call (programs deny destructive ops without it).
        #[arg(long)]
        yes: bool,
    },
}

/// `flux loop …`
#[derive(clap::Subcommand, Debug)]
enum LoopAction {
    /// Print the active agent loop (the default).
    Show,
    /// Write the built-in loop to `.flux/agent-loop.flux` so it can be edited.
    Eject {
        /// Overwrite an existing override.
        #[arg(short, long)]
        force: bool,
    },
}

/// `flux auth …`
#[derive(clap::Subcommand, Debug)]
enum AuthAction {
    /// Show which providers are configured (the default).
    Status,
    /// Log in to a provider (currently `claude`).
    Login {
        /// Provider to log in to.
        provider: String,
    },
}

/// `flux plugin …`
#[derive(clap::Subcommand, Debug)]
enum PluginAction {
    /// List installed plugins (the default).
    Ls,
    /// Add a plugin: `add <name> <program> [args…]`.
    Add {
        name: String,
        program: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Pin a plugin to a version: `pin <name> <version>`.
    Pin { name: String, version: String },
    /// Clear a plugin's version pin: `rollback <name>`.
    Rollback { name: String },
    /// Invoke one operation of an installed plugin directly: `call <name> <op> [json-input]`.
    Call {
        name: String,
        op: String,
        /// JSON input object for the operation (default `{}`).
        input: Option<String>,
    },
    /// Register every `flux-plugin-*` binary in a directory: `install [dir]`
    /// (default `plugins/target/release`).
    Install { dir: Option<String> },
    /// Generate a `flux-plugins` skill (SKILL.md + references/) from installed plugin manifests —
    /// the flux analogue of fluxplane's `fluxplane-plugin skill`. Prints to stdout by default; rerun
    /// with `--install` to (re)generate the skill tree (i.e. refresh).
    Skill {
        /// Write the SKILL.md + references/ into a skills dir (the project `.flux/skills/flux-plugins`).
        #[arg(long)]
        install: bool,
        /// With `--install`, target the user-global `~/.claude/skills/flux-plugins` instead.
        #[arg(long)]
        global: bool,
        /// Write the SKILL.md to this single file (references go in a sibling `references/`).
        #[arg(long)]
        out: Option<String>,
    },
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

/// Output format for `flux plan -o …`.
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

const KNOWN_PROVIDERS: &[&str] = &[
    "anthropic",
    "claude",
    "openai",
    "codex",
    "openrouter",
    "openrouter-anthropic",
    "ollama",
    "ollama-anthropic",
];

/// Parse a fully-qualified `provider/model` spec and build the matching provider from environment
/// credentials. Provider must be an explicit prefix (`anthropic/`, `claude/`, `openai/`, `codex/`,
/// `openrouter/`, `openrouter-anthropic/`, `ollama/`, `ollama-anthropic/`). Bare short aliases
/// (`sonnet`, `opus`, `haiku`) are implicitly `anthropic/<alias>`.
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
        // OpenRouter over its native Anthropic Messages endpoint — tool calls come back as
        // structured `tool_use` blocks instead of leaking as `<tool_call>` text on the Chat path.
        "openrouter-anthropic" => flux_providers::openrouter::openrouter_anthropic_from_env()
            .context("openrouter-anthropic provider")?,
        "ollama" => ollama_api(),
        // Local ollama over its Anthropic Messages endpoint (latest ollama), for native tool calls.
        "ollama-anthropic" => flux_providers::ollama::ollama_anthropic_api(),
        "claude" => {
            let ts = flux_credentials::claude_token_source().context("claude provider")?;
            flux_providers::anthropic::claude_oauth(ts)
        }
        "codex" => {
            let ts = flux_credentials::codex_token_source().context("codex provider")?;
            flux_providers::openai::codex_oauth(ts)
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

/// Build the knowledge datasource from the workspace's documentation files (markdown/text), indexed as
/// `file.document` records under the `local` source. Deliberately cheap: doc extensions only, capped file
/// count and size — code search is served by `grep`, not this. Errors are swallowed (an empty index just
/// yields "no matches"). Returns the shared backend the retrieval ops dispatch against.
async fn build_doc_index(system: &System) -> Arc<dyn flux_capabilities::DatasourceBackend> {
    const DOC_EXTS: &[&str] = &[".md", ".txt", ".rst", ".adoc", ".mdx"];
    const MAX_DOCS: usize = 200;
    const MAX_BYTES: usize = 100_000;
    // Wrap the keyword backend in the semantic (embeddings) backend *before* ingest — when built with
    // `--features embeddings` and an embeddings key resolves — so records are embedded as they're indexed.
    let backend: Arc<dyn flux_capabilities::DatasourceBackend> =
        datasource_backend(Arc::new(flux_capabilities::MemoryBackend::new()));
    let Ok(files) = system.walk_files(".", 4000).await else {
        return backend;
    };
    let mut docs: Vec<(String, String)> = Vec::new();
    for f in files {
        if docs.len() >= MAX_DOCS {
            break;
        }
        if !DOC_EXTS.iter().any(|e| f.ends_with(e)) {
            continue;
        }
        if let Ok(text) = system.read_file(&f).await {
            if text.len() <= MAX_BYTES {
                docs.push((f, text));
            }
        }
    }
    // Index under the `local` source as `file.document` records via the markdown ingester.
    let _ = flux_capabilities::ingest_markdown(&*backend, "local", &docs);
    backend
}

/// Build the knowledge backend from a program's declared [`datasource`](flux_lang::program::DatasourceDecl)s
/// — the `flux app run` counterpart of [`build_doc_index`]'s implicit workspace index. Each declared
/// source is ingested under its own name by the matching ingester (`markdown` walks a docs directory;
/// `openapi` reads a JSON spec file). An unknown `kind` is a clean error. Returns the shared backend the
/// retrieval ops dispatch against.
async fn build_datasources(
    decls: &[flux_lang::program::DatasourceDecl],
    system: &System,
) -> Result<Arc<dyn flux_capabilities::DatasourceBackend>> {
    const DOC_EXTS: &[&str] = &[".md", ".txt", ".rst", ".adoc", ".mdx"];
    const MAX_DOCS: usize = 1000;
    const MAX_BYTES: usize = 200_000;
    let backend: Arc<dyn flux_capabilities::DatasourceBackend> =
        datasource_backend(Arc::new(flux_capabilities::MemoryBackend::new()));
    for d in decls {
        match d.kind.as_str() {
            "markdown" => {
                let base = d.path.as_deref().unwrap_or(".");
                let files = system.walk_files(base, 4000).await.unwrap_or_default();
                let mut docs: Vec<(String, String)> = Vec::new();
                for f in files {
                    if docs.len() >= MAX_DOCS {
                        break;
                    }
                    if !DOC_EXTS.iter().any(|e| f.ends_with(e)) {
                        continue;
                    }
                    if let Ok(text) = system.read_file(&f).await {
                        if text.len() <= MAX_BYTES {
                            docs.push((f, text));
                        }
                    }
                }
                flux_capabilities::ingest_markdown(&*backend, &d.name, &docs)
                    .map_err(|e| anyhow::anyhow!("datasource `{}` (markdown): {e}", d.name))?;
            }
            "openapi" => {
                let path = d.path.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("datasource `{}` (openapi) needs a `path`", d.name)
                })?;
                let text = system
                    .read_file(path)
                    .await
                    .map_err(|e| anyhow::anyhow!("datasource `{}`: read {path}: {e}", d.name))?;
                let spec: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                    anyhow::anyhow!("datasource `{}`: parse {path} as OpenAPI JSON: {e}", d.name)
                })?;
                flux_capabilities::ingest_openapi(&*backend, &d.name, &spec)
                    .map_err(|e| anyhow::anyhow!("datasource `{}` (openapi): {e}", d.name))?;
            }
            other => {
                return Err(anyhow::anyhow!(
                    "datasource `{}` has unknown kind `{other}` (expected markdown | openapi)",
                    d.name
                ))
            }
        }
    }
    Ok(backend)
}

/// Wrap a keyword backend in the semantic (embeddings) backend when built with `--features embeddings`
/// and an embeddings API key resolves from env; otherwise return it unchanged (the default).
#[cfg(feature = "embeddings")]
fn datasource_backend(
    inner: Arc<dyn flux_capabilities::DatasourceBackend>,
) -> Arc<dyn flux_capabilities::DatasourceBackend> {
    match flux_capabilities::OpenAiEmbedder::from_env() {
        Some(embedder) => Arc::new(flux_capabilities::SemanticIndex::new(
            inner,
            Arc::new(embedder),
        )),
        None => inner,
    }
}

#[cfg(not(feature = "embeddings"))]
fn datasource_backend(
    inner: Arc<dyn flux_capabilities::DatasourceBackend>,
) -> Arc<dyn flux_capabilities::DatasourceBackend> {
    inner
}

/// Session size (serialized chars) past which the agent summarizes old turns. Override with
/// `FLUX_COMPACT_CHARS` (`0` disables compaction).
fn compact_threshold() -> usize {
    std::env::var("FLUX_COMPACT_CHARS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(48_000)
}

/// Discover skills from the project's `.flux/skills` plus the user/global dirs (`~/.flux/skills`,
/// `~/.agents/skills`, `~/.claude/skills`), project winning on a name clash. Activation (triggers or
/// a description fallback) gates which bodies are injected per turn.
fn load_skills(cwd: &std::path::Path) -> Vec<flux_skill::Skill> {
    flux_skill::discover_merged(&flux_skill::default_skill_dirs(cwd))
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
fn run_sessions(prune: bool) -> Result<()> {
    let store = open_event_store()?;
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
        eprintln!("no sessions yet — start one with `flux` or `flux run`");
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

/// `flux loop [show|eject]` — inspect and customize the flux-lang agent loop that drives every turn.
///
/// The loop is real Flux-Lang (`assets/agent-loop.flux`): `plan → match → run_plan → observe`,
/// repeated until the model answers in prose. `show` prints the active loop (a workspace
/// `.flux/agent-loop.flux` override if present, else the built-in); `eject` writes the built-in to
/// `.flux/agent-loop.flux` so it can be edited (the engine honors the override on the next turn).
fn run_loop_cmd(action: Option<LoopAction>) -> Result<()> {
    use flux_flow::engine::{agent_loop_source, builtin_agent_loop, load_agent_loop, LoopSource};

    let cwd = std::env::current_dir().context("current dir")?;
    match action.unwrap_or(LoopAction::Show) {
        LoopAction::Show => {
            let (source, text) = agent_loop_source(&cwd);
            match &source {
                LoopSource::Builtin => {
                    eprintln!("{} built-in (compiled-in default)", style::bold("source:"));
                }
                LoopSource::Override(path) => {
                    eprintln!("{} {}", style::bold("source:"), path.display());
                    // The engine errors on a bad override rather than silently using the built-in, so
                    // surface a parse failure here too instead of pretending the override is live.
                    if let Err(e) = load_agent_loop(&cwd) {
                        eprintln!("{} {e}", style::red("invalid override:"));
                    }
                }
            }
            eprintln!();
            // The loop text goes to stdout so `flux loop show` is pipeable.
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        LoopAction::Eject { force } => {
            let dir = cwd.join(".flux");
            let path = dir.join("agent-loop.flux");
            if path.exists() && !force {
                bail!(
                    "{} already exists — edit it directly, or pass --force to overwrite with the built-in",
                    path.display()
                );
            }
            std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
            std::fs::write(&path, builtin_agent_loop())
                .with_context(|| format!("write {}", path.display()))?;
            eprintln!(
                "{} {} — edit it to customize the loop (the engine uses it on the next turn)",
                style::green("wrote"),
                path.display()
            );
            Ok(())
        }
    }
}

/// Open the unified event store under `~/.flux/events.db` (conversation + run trace + turn telemetry).
fn open_event_store() -> Result<EventStore> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    let dir = home.join(".flux");
    std::fs::create_dir_all(&dir)?;
    EventStore::open(dir.join("events.db")).context("open event store")
}

/// Open flux-flow's own store under `~/.flux/flow.db` (values, symbols, suspensions). Run-trace
/// events are forwarded to the shared `events` log.
fn open_flow_store(events: Arc<EventStore>) -> Result<FlowStore> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    let dir = home.join(".flux");
    std::fs::create_dir_all(&dir)?;
    FlowStore::open(dir.join("flow.db"), events).context("open flow store")
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
async fn build_agent(
    flags: &AgentFlags,
) -> Result<(FlowEngine, String, Arc<dyn flux_runtime::Spawner>)> {
    // Guarded system rooted at the current directory; layered config loaded from it.
    let cwd = std::env::current_dir().context("current dir")?;
    let cfg = flux_config::load(&cwd).context("load .flux/config.toml")?;
    // Opt into the generic `bash` op when config enables it — exported as the env signal the runtime's
    // off-by-default `shell` group surfaces on. A user who set `FLUX_ENABLE_BASH` directly is honored
    // too (we only ever turn it on here, never off).
    if cfg.enable_shell {
        std::env::set_var("FLUX_ENABLE_BASH", "1");
    }
    let model_spec = resolve_model_spec(&flags.model, &cfg);

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
    let roles = load_roles(&cwd);
    let mut child_base = ToolRegistry::new();
    flux_tools::register_builtins(&mut child_base);
    let factory: ProviderFactory = {
        let spec = model_spec.clone();
        Arc::new(move || provider_for(&spec).map_err(|e| flux_core::Error::Other(e.to_string())))
    };
    // One construction path for sub-agents (shared with the SDK's `FlowClient::with_sub_agents`):
    // `SubAgents::into_spawner` builds the spawner; we register `TaskTool` into the top-level registry
    // below. Sub-agents inherit the same authorization floor as the top-level agent.
    let spawner: Arc<dyn flux_runtime::Spawner> =
        SubAgents::new(roles, child_base, factory, model.clone(), flags.max_tokens)
            .with_authorization(policy.clone(), caller.clone(), trust.clone())
            .into_spawner(system.clone());

    // Tools + permissions: from config (deny/allow rules); if no allow rules are configured,
    // reads are pre-allowed by default so the common case needs no config. Mutating tools prompt
    // (unless --yes) and "always-allow" choices are persisted back by the caller.
    let mut registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut registry);
    if flags.dev {
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

    // Reflexive ops (`plan`/`run_plan`): registered so a pre-authored flow (`flux flow run`, and the
    // agent loop in flux-lang) can call them, but tagged to the never-surfaced `reflect` group so they
    // stay OUT of the model-facing catalog in ordinary turns. They are only functional when a `LoopHost`
    // is installed (per reflexive run — see `run_draft_ast`); without it they return a clear error.
    flux_tools::register_reflect(&mut registry);

    // Guarded web access (policy-gated as network egress; private/loopback per config).
    registry.register(Arc::new(
        flux_capabilities::browser::WebFetchTool::default().allow_private(cfg.allow_private_net),
    ));

    // Auto-index workspace docs (markdown/text, capped & cheap) into the knowledge datasource, and
    // register the retrieval ops (`search`/`get`/`list`/`relation`/`batch_get`).
    let backend = build_doc_index(&system).await;
    flux_capabilities::register_datasource_ops(&mut registry, backend);

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
                        .with_manifest(m),
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
    let approver: Arc<dyn Approver> = if flags.yes {
        Arc::new(AllowApprover)
    } else {
        Arc::new(StdinApprover)
    };
    // JS pre-tool hooks (observe/modify/deny) from `.flux/hooks/*.js`.
    let mut hook_dirs = vec![cwd.join(".flux").join("hooks")];
    if let Some(home) = std::env::var_os("HOME") {
        hook_dirs.push(std::path::PathBuf::from(home).join(".flux").join("hooks"));
    }
    let js_hooks = flux_plugin::hooks::JsHookEngine::load(&hook_dirs);
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

    let events = Arc::new(open_event_store()?);
    let session_id = if flags.continue_ || flags.resume {
        events
            .latest_session()
            .context("latest session")?
            .ok_or_else(|| anyhow::anyhow!("no session to resume"))?
    } else {
        events.create_session(&model).context("create session")?
    };

    let flow = open_flow_store(events.clone())?;
    // Assemble the engine: this installs the reflexive loop host on the executor and loads the flux-lang
    // `agent-loop.flux` (the turn loop is flux-lang, not Rust).
    let spec = AgentSpec {
        model,
        system_prompt,
        skills: load_skills(&cwd),
        max_tokens: flags.max_tokens,
        max_iterations: 25,
        groups,
        compact_threshold_chars: compact_threshold(),
        cwd: cwd.clone(),
        // The CLI builds its own richly-configured executor (perms/approver/hooks/policy/identity)
        // above, so `tools`/`permissions` are already applied there — `into_engine` consumes only the
        // engine-identity fields.
        ..AgentSpec::default()
    };
    let agent = spec
        .into_engine(Arc::from(provider), executor, events, flow)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok((agent, session_id, spawner))
}

/// One-shot agentic turn.
async fn run_agentic(flags: &AgentFlags, prompt: String) -> Result<()> {
    let (agent, session_id, _spawner) = build_agent(flags).await?;
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
/// `flux eval <adapter> [--tasks a,b] [--members a,b] [--limit N] [-m model] [--trials N]
/// [--report out.md] [--watch]` — run a benchmark suite ad-hoc through flux-eval and print a summary
/// (same adapters + scoring the `eval_run` op and improve loop use). `--watch` streams each task's
/// agent activity live; `--report` writes the categorized Markdown report.
#[allow(clippy::too_many_arguments)]
async fn run_eval_cmd(
    adapter: String,
    tasks: Vec<String>,
    members: Vec<String>,
    limit: u64,
    trials: u64,
    report_path: Option<String>,
    watch: bool,
    model: Option<String>,
) -> Result<()> {
    let mut params = serde_json::json!({
        "adapter": adapter,
        "tasks": tasks,
        "limit": limit,
        "trials": trials,
        "watch": watch,
    });
    if let Some(m) = &model {
        params["model"] = serde_json::Value::String(m.clone());
    }
    if !members.is_empty() {
        params["members"] = serde_json::Value::Array(
            members
                .iter()
                .map(|m| serde_json::json!({ "adapter": m }))
                .collect(),
        );
    }

    let report = flux_eval::ops::run_eval(params)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    println!("{}", flux_eval::ops::report_view(&report));
    if let Some(cases) = report.get("cases").and_then(|v| v.as_array()) {
        for c in cases {
            let id = c.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
            let pr = c.get("pass_rate").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let mark = if pr >= 1.0 { "ok  " } else { "FAIL" };
            let iters = c
                .get("mean_iterations")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let errs = c
                .get("mean_tool_errors")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            println!("  [{mark}] {id}  ({iters:.0} iters, {errs:.0} tool-errs)");
        }
    }
    if let Some(path) = report_path {
        let md = flux_eval::report::render_markdown(&report);
        std::fs::write(&path, md).with_context(|| format!("write report {path}"))?;
        println!("report written to {path}");
    }
    Ok(())
}

async fn run_flow(file: &str, model: Option<String>, yes: bool) -> Result<()> {
    // Build the agent flags from the command's own model/`--yes` (reuses the shared agent wiring).
    let flags = AgentFlags::from_model_yes(model.as_deref(), yes);

    let src = std::fs::read_to_string(file).with_context(|| format!("read flow {file}"))?;
    // A behavioral loop file is native flux-lang text, or a checked-in JSON `DraftAst` (sniffed by the
    // leading `{`). Both load as the same AST.
    let ast: flux_flow::ast::DraftAst = if src.trim_start().starts_with('{') {
        serde_json::from_str(&src)
            .with_context(|| format!("parse {file} as a Flux-Lang DraftAst (JSON)"))?
    } else {
        flux_lang::parse::parse(&src)
            .map_err(|e| anyhow::anyhow!("parse {file} as Flux-Lang text: {e}"))?
    };

    run_draft_ast(&flags, &ast).await
}

/// Execute a pre-built `DraftAst` through the full envelope — the shared core behind both
/// `flux flow run <file.flux>` and `flux preset <name> --run`. Builds the agent, validates the flow
/// against the live op registry, previews risk + installs the per-op approver, runs it, and prints the
/// outcome. The only inputs are the agent flags (model/`--yes`) and the AST itself.
pub(crate) async fn run_draft_ast(
    flags: &AgentFlags,
    ast: &flux_flow::ast::DraftAst,
) -> Result<()> {
    let (engine, session_id, _spawner) = build_agent(flags).await?;
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

    // Risk preview (informational; every op still gates at dispatch through the engine's approver,
    // which `build_agent` set from `--yes`).
    let risk = flux_flow::runtime::plan_risk(ast, engine.executor.registry());
    eprintln!(
        "\n{}  {}{}",
        style::bold("flow"),
        risk_badge(&risk.summary()),
        style::dim(&format!(" · {} op(s)", risk.ops.len()))
    );

    // Point the engine's installed loop host at this run's session + sink (a flow may call
    // `plan`/`run_plan`, which re-enter the planner/interpreter through this same executor). The sink is
    // shared so the outer flow and any inner `run_plan` stream live onto one surface, sub-steps interleaved.
    let shared: Arc<std::sync::Mutex<dyn AgentSink>> =
        Arc::new(std::sync::Mutex::new(CliSink::new(0)));
    engine.loop_host.set_turn(
        session_id.clone(),
        Some(engine.system_prompt.clone()),
        shared.clone(),
    );

    let mut sink = flux_flow::loop_host::SharedSink::new(shared.clone());
    let outcome = flux_flow::runtime::execute_flow(
        engine.flow.as_ref(),
        engine.executor.as_ref(),
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
    shared.lock().unwrap().turn_end(None);
    Ok(())
}

/// An `AskUser` that prompts on stdin — used by `flux plan` when attached to a terminal.
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

/// `flux plan <prompt>` (plan mode, one-shot): compile the prompt into a Flux-Lang plan and show it. On
/// an interactive terminal it then asks `run it? [y/N]` and executes on yes; piped or with `-o json|yaml`
/// it just prints the plan and exits (never runs). The same engine drives this and a real turn, so the
/// plan you see is the plan that runs.
async fn run_plan(
    flags: AgentFlags,
    output: Option<OutputFormat>,
    prompt_words: Vec<String>,
) -> Result<()> {
    let prompt = prompt_words.join(" ");
    if prompt.trim().is_empty() {
        bail!(
            "`flux plan` needs a prompt, e.g. `flux plan \"summarize the README into SUMMARY.txt\"`"
        );
    }
    let (engine, session_id, _spawner) = build_agent(&flags).await?;
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
    if output.is_some() || !std::io::stdout().is_terminal() {
        let rendered = match output.unwrap_or_default() {
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
    if !(flags.yes || confirm_plan(risk.ops.len())) {
        eprintln!("{}", style::dim("not run"));
        return Ok(());
    }

    // Approved → run it through the same envelope (PlanApprover: approved ops pass without a re-prompt;
    // destructive ops still escalate to the fallback — per-op confirm, or auto under --yes).
    let fallback: Arc<dyn Approver> = if flags.yes {
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

// ── `flux a2a` — remote A2A agent client ───────────────────────────────────────

/// `~/.flux/a2a-history.txt` — separate from the main REPL history.
fn a2a_history_path() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("HOME")?).join(".flux");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("a2a-history.txt"))
}

/// The `a2a › ` prompt for the remote-agent REPL.
struct A2aPrompt;
impl Prompt for A2aPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("a2a › ")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("… ")
    }
    fn render_prompt_history_search_indicator(&self, _s: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Borrowed("(reverse-search) ")
    }
}

/// A thin markdown renderer for the remote agent's reply, mirroring `CliSink`'s live rendering.
///
/// It tracks everything rendered so far so it can absorb either streaming convention transparently:
/// agents that send incremental **deltas** and agents that send cumulative **snapshots** (each event
/// is the full text so far). [`A2aRender::push_message`] pushes only the new suffix in the snapshot
/// case, so neither double-renders.
struct A2aRender {
    live: flux_markdown::render::LiveRenderer,
    rendered: String,
}

impl A2aRender {
    fn new() -> Self {
        let stdout_tty = std::io::stdout().is_terminal();
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|c| c.parse::<usize>().ok())
            .filter(|&w| w >= 20)
            .unwrap_or(80);
        A2aRender {
            live: flux_markdown::render::LiveRenderer::new(
                flux_markdown::render::Theme::auto(),
                width,
                stdout_tty,
            ),
            rendered: String::new(),
        }
    }
    /// Append `t` to the live render and to the running record.
    fn push(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        let mut out = std::io::stdout().lock();
        let _ = self.live.push(t, &mut out);
        drop(out);
        self.rendered.push_str(t);
    }
    /// Render an agent message whose text may be a **delta** or a cumulative **snapshot**. If it
    /// extends what we've already shown, push only the new tail; otherwise push it as a fresh delta.
    fn push_message(&mut self, t: &str) {
        let suffix = new_render_suffix(&self.rendered, t);
        self.push(suffix);
    }
    /// True if anything has been rendered this turn.
    fn has_output(&self) -> bool {
        !self.rendered.is_empty()
    }
    fn finish(&mut self) {
        if self.live.is_active() {
            let mut out = std::io::stdout().lock();
            let _ = self.live.finish(&mut out);
        }
    }
}

/// What to actually render for an incoming agent message, given what's already on screen: the new
/// tail if `incoming` is a cumulative snapshot that extends `rendered`, else the whole `incoming`
/// (a delta). One code path then absorbs both streaming conventions without double-rendering.
fn new_render_suffix<'a>(rendered: &str, incoming: &'a str) -> &'a str {
    incoming.strip_prefix(rendered).unwrap_or(incoming)
}

/// Render one streaming event. Status-update / message text is fed through [`A2aRender::push_message`]
/// so delta- and snapshot-style agents both render correctly. Returns `true` once the stream's
/// final/terminal event arrives.
fn handle_a2a_event(ev: flux_a2a::StreamEvent, render: &mut A2aRender) -> bool {
    use flux_a2a::StreamEvent;
    match ev {
        StreamEvent::StatusUpdate(u) => {
            if let Some(m) = &u.status.message {
                render.push_message(&m.text());
            }
            u.is_final
        }
        StreamEvent::Message(m) => {
            render.push_message(&m.text());
            false
        }
        StreamEvent::Task(t) => {
            // A terminal Task on the stream: if nothing streamed, render its text once.
            if !render.has_output() {
                render.push_message(&t.final_text());
            }
            t.status.state.is_terminal()
        }
        StreamEvent::ArtifactUpdate(a) => {
            for p in &a.artifact.parts {
                if let Some(s) = p.as_text() {
                    render.push(s);
                }
            }
            false
        }
    }
}

/// Run one A2A turn: send `text` as a single task and render the remote agent's reply.
async fn a2a_turn(
    client: &flux_a2a::A2aClient,
    context_id: &str,
    text: &str,
    streaming: bool,
    cancel: &tokio_util::sync::CancellationToken,
) {
    let msg = flux_a2a::Message::user_text(text, Some(context_id.to_string()));
    if streaming {
        let mut stream = match client.stream(msg).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{} {e}", style::red("error:"));
                return;
            }
        };
        let mut render = A2aRender::new();
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    render.finish();
                    eprintln!("{}", style::dim("(cancelled)"));
                    return;
                }
                next = stream.next() => {
                    match next {
                        Some(Ok(ev)) => {
                            if handle_a2a_event(ev, &mut render) {
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            render.finish();
                            eprintln!("{} {e}", style::red("error:"));
                            return;
                        }
                        None => break,
                    }
                }
            }
        }
        render.finish();
        if !render.has_output() {
            eprintln!("{}", style::dim("(no output)"));
        }
    } else {
        // Non-streaming: blocking send, polling `tasks/get` if a general agent answers with a
        // still-running task.
        let outcome = match client.send(msg, true).await {
            Ok(o) => o,
            Err(e) => {
                eprintln!("{} {e}", style::red("error:"));
                return;
            }
        };
        let reply = match outcome.as_task() {
            Some(t) if !t.status.state.is_terminal() => {
                match client
                    .await_task(&t.id, std::time::Duration::from_millis(700), 120)
                    .await
                {
                    Ok(done) => done.final_text(),
                    Err(e) => {
                        eprintln!("{} {e}", style::red("error:"));
                        return;
                    }
                }
            }
            _ => outcome.final_text(),
        };
        if reply.trim().is_empty() {
            eprintln!("{}", style::dim("(no output)"));
            return;
        }
        let mut render = A2aRender::new();
        render.push(&reply);
        render.finish();
    }
}

/// `flux a2a <URL>` — connect to a remote A2A agent and drive it from the CLI like a local agent.
async fn run_a2a(url: String, prompt_words: Vec<String>, token: Option<String>) -> Result<()> {
    let token = token.or_else(|| std::env::var("FLUX_A2A_TOKEN").ok());
    let mut client = flux_a2a::A2aClient::new(&url)
        .map_err(|e| anyhow::anyhow!("invalid a2a url `{url}`: {e}"))?
        .with_token(token);

    // Discover the remote agent (best-effort): its name + whether it streams. If the card can't be
    // fetched we fall back to non-streaming `message/send` — the lowest-common-denominator that
    // returns a clear result/error, rather than risking a silent non-SSE response.
    let mut streaming = false;
    match client.fetch_agent_card().await {
        Ok(card) => {
            streaming = card.capabilities.streaming;
            let name = if card.name.is_empty() {
                "a2a agent"
            } else {
                card.name.as_str()
            };
            let ver = if card.version.is_empty() {
                String::new()
            } else {
                format!(" v{}", card.version)
            };
            eprintln!(
                "{}",
                style::dim(&format!("connected → {name}{ver} · {}", client.rpc_url()))
            );
            let desc = card.description.lines().next().unwrap_or("").trim();
            if !desc.is_empty() {
                eprintln!("{}", style::dim(desc));
            }
        }
        Err(e) => eprintln!(
            "{}",
            style::dim(&format!(
                "(no agent card: {e}; using non-streaming message/send) → {}",
                client.rpc_url()
            ))
        ),
    }

    // One stable conversation context for this session (forward-compatible with stateful remotes).
    let context_id = flux_a2a::new_id();

    // One-shot when given prompt words, or when stdin is piped (not a TTY).
    let piped = !std::io::stdin().is_terminal();
    if !prompt_words.is_empty() || piped {
        let prompt = if !prompt_words.is_empty() {
            prompt_words.join(" ")
        } else {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
            buf.trim().to_string()
        };
        if prompt.is_empty() {
            return Ok(());
        }
        let client_ref = &client;
        let ctx_ref = context_id.as_str();
        let prompt_ref = prompt.as_str();
        run_interruptible(move |c| async move {
            a2a_turn(client_ref, ctx_ref, prompt_ref, streaming, &c).await;
        })
        .await;
        return Ok(());
    }

    // Interactive REPL.
    eprintln!(
        "{}",
        style::dim("a2a chat — /help, Ctrl-C interrupts a turn, Ctrl-D exits")
    );
    let history: Box<dyn reedline::History> = match a2a_history_path() {
        Some(p) => Box::new(
            FileBackedHistory::with_file(1000, p)
                .unwrap_or_else(|_| FileBackedHistory::new(1000).expect("in-memory history")),
        ),
        None => Box::new(FileBackedHistory::new(1000).expect("in-memory history")),
    };
    let mut editor = Reedline::create().with_history(history);
    loop {
        let line = match editor.read_line(&A2aPrompt) {
            Ok(Signal::Success(buf)) => buf,
            Ok(Signal::CtrlC) => continue,
            Ok(Signal::CtrlD) => break,
            Ok(_) => continue,
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
                    eprintln!("a2a REPL commands:");
                    eprintln!("  /card   show the remote agent card");
                    eprintln!("  /exit   quit");
                    eprintln!("  Ctrl-C  interrupt a running turn   Ctrl-D  exit");
                }
                "card" => match client.fetch_agent_card().await {
                    Ok(card) => {
                        if let Ok(s) = serde_json::to_string_pretty(&card) {
                            println!("{s}");
                        }
                    }
                    Err(e) => eprintln!("{} {e}", style::red("error:")),
                },
                other => eprintln!(
                    "{}",
                    style::dim(&format!("(unknown command /{other} — try /help)"))
                ),
            }
            continue;
        }
        let client_ref = &client;
        let ctx_ref = context_id.as_str();
        run_interruptible(move |c| async move {
            a2a_turn(client_ref, ctx_ref, input, streaming, &c).await;
        })
        .await;
    }
    Ok(())
}

/// Interactive agentic REPL (tools enabled), with slash commands.
async fn run_repl(flags: AgentFlags) -> Result<()> {
    let (mut agent, mut session_id, spawner) = build_agent(&flags).await?;
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
                        ("/shell", "toggle the generic bash op (off by default)"),
                        ("/tools", "list available tools"),
                        (
                            "/evidence",
                            "show the audit trail this session has recorded",
                        ),
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
                "shell" => {
                    // Toggle the generic `bash` op for the session by flipping the env signal the
                    // runtime's `shell` group surfaces on; it takes effect from the next turn (the
                    // advertised catalog is recomputed per turn from `detect_signals`).
                    let currently_on = std::env::var("FLUX_ENABLE_BASH")
                        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
                    if currently_on {
                        std::env::remove_var("FLUX_ENABLE_BASH");
                    } else {
                        std::env::set_var("FLUX_ENABLE_BASH", "1");
                    }
                    eprintln!(
                        "{}",
                        style::dim(&format!(
                            "shell (bash) {} — the generic `bash` op is {} the catalog from the next turn",
                            if currently_on { "off" } else { "on" },
                            if currently_on { "hidden from" } else { "in" }
                        ))
                    );
                }
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
                                let provider: Arc<dyn Provider> = Arc::new(native);
                                agent.provider = provider.clone();
                                agent.model = model.clone();
                                // The loop host holds its own planner handle — swap it too.
                                agent.loop_host.set_model(provider, model);
                                let _ = agent.events.set_model(&session_id, &agent.model);
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
                "evidence" => {
                    // The audit trail the loop and the dispatcher have recorded this session: tool
                    // calls/errors, per-iteration markers, and any flow-emitted observations. This is
                    // the same shared log the `observe`/`evidence`/grading ops read.
                    eprintln!("{}", format_evidence(&agent.executor.evidence()));
                }
                "session" => eprintln!("session {session_id} · model {}", agent.model),
                "sessions" => match agent.events.list(30) {
                    Ok(list) if !list.is_empty() => {
                        for s in &list {
                            let here = if s.id == session_id { "*" } else { " " };
                            // Try to load the first user message as a human-readable preview.
                            let preview = agent
                                .events
                                .conversation(&s.id)
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
                        match agent.events.info(id) {
                            Ok(info) => {
                                let n = agent
                                    .events
                                    .conversation(&info.id)
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
                        .events
                        .create_session(&agent.model)
                        .context("new session")?;
                    eprintln!("started new session {session_id}");
                }
                other => eprintln!("unknown command /{other} (try /help)"),
            }
            continue;
        }
        // Plan mode: compile + show a plan, store it for `/run`, but DON'T execute. Refine by chatting.
        // Interruptible: the first Ctrl-C drops the in-flight compose and returns to the prompt.
        if plan_mode {
            let agent_ref = &agent;
            let sid_ref = session_id.as_str();
            let mut new_plan: Option<flux_flow::ast::DraftAst> = None;
            let plan_slot = &mut new_plan;
            run_interruptible(move |c| async move {
                let mut sink = CliSink::new(0);
                match agent_ref.plan_turn(sid_ref, input, &mut sink, &c).await {
                    Ok(Some(ast)) => {
                        *plan_slot = Some(ast);
                        eprintln!(
                            "{}",
                            style::dim(
                                "(plan ready — `/run` to execute, or send a message to refine)"
                            )
                        );
                    }
                    Ok(None) => {} // prose answer, or the compose was cancelled — nothing to run
                    Err(e) => eprintln!("{} {e:#}", style::red("error:")),
                }
            })
            .await;
            // Only replace a prior pending plan when a fresh one was produced (prose/cancel keep it).
            if let Some(ast) = new_plan {
                pending_plan = Some(ast);
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

/// REPL `/run`: execute a reviewed plan. Typing `/run` after reviewing the plan in `/plan` mode IS the
/// approval, so the plan runs as a pre-approved unit — its ops don't prompt individually (deny rules
/// still apply). The scope guard closes when this returns.
async fn run_pending_plan(
    agent: &FlowEngine,
    session_id: &str,
    ast: &flux_flow::ast::DraftAst,
    cancel: &tokio_util::sync::CancellationToken,
) {
    let _scope = agent.executor.enter_approved_scope();
    let mut sink = CliSink::new(0);
    // Race execution against `cancel`: `execute_flow` has no cancellation of its own, so Ctrl-C is
    // honored by dropping the in-flight flow future (which aborts the current op's IO). The future
    // borrows `sink`, so scope it in a block and read its result out as owned data; `None` => cancelled.
    let result: Option<Result<String, String>> = {
        let fut = flux_flow::runtime::execute_flow(
            &agent.flow,
            &agent.executor,
            session_id,
            ast,
            &mut sink,
        );
        tokio::pin!(fut);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            res = &mut fut => Some(res.map(|o| o.result).map_err(|e| format!("{e:#}"))),
        }
    };
    match result {
        Some(Ok(out)) => {
            if !out.trim().is_empty() {
                println!("{out}");
            }
            sink.turn_end(None);
        }
        Some(Err(e)) => eprintln!("{} {e}", style::red("error:")),
        None => {
            // Cancelled: stop the in-flight op's spinner and return to the prompt.
            sink.turn_end(None);
            eprintln!("{}", style::dim("(cancelled)"));
        }
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
/// Render the session's evidence log for `/evidence`: a one-line summary plus one line per
/// observation (phase, kind, compact data), flagging `tool_error` rows. Returns the empty-state
/// message when nothing has been recorded yet. Reads the same shared log the `observe`/`evidence`/
/// grading ops write.
fn format_evidence(log: &flux_evidence::EvidenceLog) -> String {
    let obs = log.all();
    if obs.is_empty() {
        return "no evidence recorded yet — run a turn first".to_string();
    }
    let errors = obs.iter().filter(|o| o.kind == "tool_error").count();
    let iters = obs.iter().filter(|o| o.kind == "turn.iteration").count();
    let mut out = format!(
        "evidence: {} observation{}, {iters} iteration{}, {errors} error{}",
        obs.len(),
        if obs.len() == 1 { "" } else { "s" },
        if iters == 1 { "" } else { "s" },
        if errors == 1 { "" } else { "s" },
    );
    for o in obs {
        // Pad before coloring — `{:<N}` counts ANSI bytes, so styling a padded column would break
        // alignment.
        let phase = format!("{:<9}", format!("{:?}", o.phase).to_lowercase());
        let mark = if o.kind == "tool_error" {
            style::red("!")
        } else {
            " ".to_string()
        };
        let data = if o.data.is_null() {
            String::new()
        } else {
            truncate(&o.data.to_string(), 100)
        };
        out.push_str(&format!(
            "\n  {mark} {} {:<16} {}",
            style::dim(&phase),
            o.kind,
            style::dim(&data)
        ));
    }
    out
}

/// A compact, readable label for a loop-machinery op (`plan`/`run_plan`/`observe`/…) shown when
/// `--show-loop` reveals the loop. Returns `None` for ordinary ops (which fall through to the normal
/// label path). These ops carry large inputs, so the label deliberately omits the payload.
fn loop_machinery_label(name: &str, input: &Value) -> Option<String> {
    let (verb, note) = match name {
        "plan" => ("plan", "ask the model"),
        "run_plan" => ("run plan", "execute the emitted graph"),
        "observe" => {
            let kind = input.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            return Some(format!("{}  {}", style::cyan("observe"), style::dim(kind)));
        }
        "evidence" => ("evidence", "read the audit trail"),
        "metrics" => ("metrics", "calls / errors / iterations"),
        "grade" => ("grade", "check a criterion"),
        _ => return None,
    };
    Some(format!("{}  {}", style::cyan(verb), style::dim(note)))
}

fn render_call_label(name: &str, input: &Value, verbose: bool) -> String {
    // Column width: wide enough for the longest built-in op name (`web_fetch` = 9).
    const GUTTER: usize = 10;
    const ARG_CAP: usize = 120;
    // The loop machinery (revealed by `--show-loop`) carries large inputs — a plan AST, a transcript.
    // Give those a compact, readable label so the stream reads as loop iterations, not a payload dump.
    if let Some(label) = loop_machinery_label(name, input) {
        return label;
    }
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
    live: flux_markdown::render::LiveRenderer,
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
            live: flux_markdown::render::LiveRenderer::new(
                flux_markdown::render::Theme::auto(),
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
        // The right-hand token annotation: context-window occupancy, generated tokens, cache + hit-rate.
        let token_inline = usage.as_ref().map(usage_annotation).unwrap_or_default();
        // Always print a rule so the turn boundary is visible even for prose-only replies.
        let summary = if self.steps > 0 {
            let plural = if self.steps == 1 { "" } else { "s" };
            format!("{} step{plural} · {elapsed}{token_inline}", self.steps)
        } else {
            // Prose-only turn: a minimal rule with elapsed + token stats.
            format!("· {elapsed}{token_inline}")
        };
        let rule_len = self.width.saturating_sub(summary.chars().count() + 2);
        eprintln!("{} {}", style::rule(rule_len), style::dim(&summary));
    }
}

/// The compact token annotation appended to a turn-end rule (and the prose `/goal` footer): the
/// context-window occupancy (the final prompt size), the tokens generated, and — when prompt caching
/// is in play — the cached tokens with the hit-rate (cached ÷ context). All four figures the user
/// asked to see; empty when nothing was billed (e.g. an offline `-m mock` turn).
fn usage_annotation(u: &Usage) -> String {
    let context = u.context_tokens();
    if context == 0 && u.output_tokens == 0 {
        return String::new();
    }
    let mut s = format!(
        " · ctx {} · out {}",
        style::fmt_tokens(context),
        style::fmt_tokens(u.output_tokens)
    );
    if u.cache_read_input_tokens > 0 && context > 0 {
        let pct = (u.cache_read_input_tokens as f64 / context as f64 * 100.0).round() as u64;
        s.push_str(&format!(
            " · cache {} ({pct}% hit)",
            style::fmt_tokens(u.cache_read_input_tokens)
        ));
    }
    s
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
            // Same figures as the main rule, without the leading separator.
            let stats = usage_annotation(&u);
            let stats = stats.trim_start_matches(" · ");
            if !stats.is_empty() {
                eprintln!("{}", style::dim(stats));
            }
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
        // were fed back, so the engine loops here — answer in prose, which ends the turn. The usage
        // chunk mimics a cached re-send (most of the prompt read from cache) so the offline path
        // exercises the turn-end token annotation (context / output / cache + hit-rate).
        if n > 0 {
            let chunks = vec![
                Chunk::Block(ContentBlock::Text {
                    text: "Finished.".into(),
                }),
                Chunk::Usage(Usage {
                    input_tokens: 180,
                    output_tokens: 12,
                    cache_read_input_tokens: 1_240,
                    ..Default::default()
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
            Chunk::Usage(Usage {
                input_tokens: 1_240,
                output_tokens: 48,
                ..Default::default()
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
        let prompt = format!(
            "\n{} `{}`{}  [y]es / [a]lways / [N]o: ",
            style::yellow("approve"),
            style::bold(tool),
            subjects_fmt
        );
        read_choice(prompt, ApprovalChoice::AllowAlways(tool.to_string())).await
    }

    /// The whole-plan confirm. The plan tree + risk were already rendered (the `flow.plan` observation),
    /// so this is one line. `always` here trusts every plan for the rest of the session.
    async fn request_plan(&self, summary: &str, ops: usize) -> ApprovalChoice {
        let prompt = format!(
            "\n{} this plan? ({} op(s) · {})  [y]es / [a]lways / [N]o: ",
            style::yellow("run"),
            ops,
            summary,
        );
        read_choice(prompt, ApprovalChoice::AllowAlways("*plans*".to_string())).await
    }
}

/// Print `prompt`, then read a y/a/N answer **off the async runtime** so the turn's future YIELDS while
/// waiting — a blocking read inside the poll would freeze the task and make Ctrl-C inert. On a terminal
/// we read a single keypress via crossterm in raw mode: the keystroke is consumed cleanly (no leaked
/// line-reader that would fight reedline for stdin on the next prompt), and Ctrl-C / Ctrl-D / `n` / Esc
/// all decline. Off a terminal (pipes, eval) we read a line — EOF ends it and there's no prompt to
/// corrupt. `always` is returned for `a`/`always`.
async fn read_choice(prompt: String, always: ApprovalChoice) -> ApprovalChoice {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    if !std::io::stdin().is_terminal() {
        return match read_stdin_line().await {
            Some(line) => parse_choice(&line, always),
            None => ApprovalChoice::Deny,
        };
    }
    let choice = tokio::task::spawn_blocking(move || read_key_choice(always))
        .await
        .unwrap_or(ApprovalChoice::Deny);
    eprintln!(); // raw mode echoes nothing — close the prompt line
    choice
}

/// Restores cooked mode on drop, so a panic or early return inside the key-read never leaves the
/// terminal in raw mode.
struct RawModeGuard;
impl RawModeGuard {
    fn enable() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Read one approval keypress in raw mode (blocking — call inside `spawn_blocking`). The key is consumed
/// and the function returns, so nothing outlives the call to fight the next reedline read. Ctrl-C/Ctrl-D
/// decline (in raw mode they arrive as key events, not SIGINT).
fn read_key_choice(always: ApprovalChoice) -> ApprovalChoice {
    use crossterm::event::{read, Event, KeyCode, KeyEventKind, KeyModifiers};
    let _raw = match RawModeGuard::enable() {
        Ok(g) => g,
        Err(_) => return ApprovalChoice::Deny,
    };
    loop {
        match read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                return match k.code {
                    KeyCode::Char('c') | KeyCode::Char('d') if ctrl => ApprovalChoice::Deny,
                    KeyCode::Char('y') | KeyCode::Char('Y') => ApprovalChoice::Allow,
                    KeyCode::Char('a') | KeyCode::Char('A') => always,
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Enter | KeyCode::Esc => {
                        ApprovalChoice::Deny
                    }
                    _ => continue, // ignore other keys, keep waiting
                };
            }
            Ok(_) => continue,
            Err(_) => return ApprovalChoice::Deny,
        }
    }
}

/// Read one line from stdin off the async runtime (`spawn_blocking`). Used only on the non-terminal
/// path (pipes / eval), where EOF ends the read and there's no interactive prompt to corrupt.
async fn read_stdin_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok().map(|_| line)
    })
    .await
    .ok()
    .flatten()
}

/// Map a typed y/a/N line to a choice (the non-terminal fallback). `always` is returned for `a`/`always`.
fn parse_choice(line: &str, always: ApprovalChoice) -> ApprovalChoice {
    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => ApprovalChoice::Allow,
        "a" | "always" => always,
        _ => ApprovalChoice::Deny,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
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
    // subcommands (`run`/`plan`/`tui`/`serve`). With no subcommand, `flux` opens the REPL.
    let cli = Cli::parse();
    style::init(cli.color);

    let run = async {
        match cli.command {
            // The agent-path subcommands. Each exports its own verbose/show-loop env first.
            Some(Commands::Run { agent, prompt }) => {
                apply_agent_env(&agent);
                // `flux run <app.flux>` runs a multi-agent program; `flux run <prompt…>` runs a turn.
                if prompt
                    .first()
                    .map(|p| p.ends_with(".flux") || std::path::Path::new(p).is_file())
                    .unwrap_or(false)
                {
                    return run_app_cmd(prompt, agent.model.clone(), agent.yes).await;
                }
                // `flux run` with no prompt drops into the REPL (with the given agent flags).
                if prompt.is_empty() {
                    return run_repl(agent).await;
                }
                run_prompt(agent, prompt).await
            }
            Some(Commands::Plan {
                agent,
                output,
                prompt,
            }) => {
                apply_agent_env(&agent);
                run_plan(agent, output, prompt).await
            }
            Some(Commands::Tui { agent }) => {
                apply_agent_env(&agent);
                run_tui(agent).await
            }
            Some(Commands::Serve { agent, addr }) => {
                apply_agent_env(&agent);
                run_serve(agent, addr).await
            }
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
                action: AppAction::Run { agent, program },
            }) => {
                apply_agent_env(&agent);
                run_app(&program, agent.model.clone(), agent.yes).await
            }
            Some(Commands::Flow {
                action: FlowAction::Run { file, model, yes },
            }) => run_flow(&file, model, yes).await,
            Some(Commands::Loop { action }) => run_loop_cmd(action),
            Some(Commands::Sessions { prune }) => run_sessions(prune),
            Some(Commands::Auth { action }) => run_auth(action).await,
            Some(Commands::Plugin { action }) => run_plugin(action).await,
            Some(Commands::Completion { shell }) => run_completion(shell.as_deref()),
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

/// Export the per-turn env signals (`FLUX_VERBOSE`, `FLUX_SHOW_LOOP`) the agent-path subcommands honor.
fn apply_agent_env(flags: &AgentFlags) {
    if flags.verbose {
        std::env::set_var("FLUX_VERBOSE", "1");
    }
    if flags.show_loop {
        std::env::set_var("FLUX_SHOW_LOOP", "1");
    }
}

/// `flux completion <shell>` — print a shell completion script to stdout and exit. Pure output, no
/// side effects: a shell sources this as you type, so it must never touch the network or start a
/// turn. Supports bash/zsh/fish/powershell/elvish; defaults to fish.
fn run_completion(shell: Option<&str>) -> Result<()> {
    use clap::CommandFactory;
    use clap_complete::Shell;
    let shell = match shell {
        Some("bash") => Shell::Bash,
        Some("zsh") => Shell::Zsh,
        Some("powershell" | "pwsh") => Shell::PowerShell,
        Some("elvish") => Shell::Elvish,
        Some("fish") | None => Shell::Fish,
        Some(other) => {
            eprintln!(
                "flux completion: unsupported shell {other:?} (bash|zsh|fish|powershell|elvish)"
            );
            return Ok(());
        }
    };
    clap_complete::generate(shell, &mut Cli::command(), "flux", &mut std::io::stdout());
    Ok(())
}

/// `flux run <app.flux>` — load and run a multi-agent flux **Program** through the `flux-app` host
/// (event bus + triggers + journeys). A bare single-flow file is accepted too. The provider is
/// best-effort: a program built only from pure ops runs without credentials; model-backed ops need a
/// resolvable `provider/model` (defaulting like the prompt path) and degrade with a clear note.
async fn run_app_cmd(
    prompt: Vec<String>,
    model_spec: Option<String>,
    auto_approve: bool,
) -> Result<()> {
    // The `.flux` path is the first token; `-m`/`--yes` were parsed as global flags.
    let path = prompt
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("usage: flux run <app.flux> [-m provider/model] [--yes]"))?;
    run_app(path, model_spec, auto_approve).await
}

/// Build and run a multi-agent program together with its declared **channels**, the shared body behind
/// both `flux run <app.flux>` (auto-detect) and `flux app run <program.flux>`. Cron/webhook/Slack
/// channels start as background tasks that deliver events into the program's bus (→ triggers → journeys)
/// until Ctrl-C; a program with a `cli` channel — or none at all — keeps the interactive stdin loop. By
/// default destructive ops are DENIED (no human at a prompt); `--yes` opts into allow-all. The provider
/// is best-effort: a pure-op program runs without credentials.
async fn run_app(path: &str, model_spec: Option<String>, auto_approve: bool) -> Result<()> {
    use flux_lang::program::{Module, Program};

    let spec = model_spec.unwrap_or_else(|| "anthropic/claude-sonnet-4-6".to_string());
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

    let src =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read program `{path}`: {e}"))?;
    let mut program = match Module::parse_str(&src).map_err(|e| anyhow::anyhow!("{e}"))? {
        Module::Program(p) => p,
        Module::Flow(flow) => Program {
            flows: vec![flow],
            ..Default::default()
        },
    };
    // Resolve `secret "ENV_NAME"` references in declaration settings from the environment (plaintext is
    // never inline) before any of those settings reach a channel/datasource/agent.
    flux_app::resolve_secrets(&mut program).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Assemble the knowledge + integration tools the program's agent target (`trigger.agent`) and its
    // journeys can drive — the D-09 registry wiring. A guarded `System` rooted at the cwd backs both.
    let cwd = std::env::current_dir()?;
    let system = Arc::new(System::new(
        Workspace::new(&cwd).map_err(|e| anyhow::anyhow!("{e}"))?,
    ));
    // SSRF egress opt-in, off by default. `flux app run` honors the same `allow_private_net` setting as
    // the interactive path (`~/.flux/config.toml`): with it on, plugin HTTP ops may reach
    // private/loopback hosts — needed for a bot deployed next to private DevOps endpoints (in-cluster
    // GitLab / Loki / Prometheus / Kubernetes). A missing or unreadable config keeps the safe default.
    let allow_private = flux_config::load(&cwd)
        .map(|c| c.allow_private_net)
        .unwrap_or(false);
    // The knowledge datasource: build the program's declared datasources, and SHARE the backend so
    // integration plugins' contributed records (via the DatasourceHostCaps bridge) land in the same
    // index the `search`/`get`/`list`/`relation`/`batch_get` ops read.
    let backend = build_datasources(&program.datasources, &system).await?;
    let mut extra_tools: Vec<Arc<dyn flux_runtime::Tool>> =
        flux_capabilities::datasource_tools(backend.clone());
    // Discover subprocess plugins (~/.flux/plugins/*.toml) and project their ops as tools; their host
    // capabilities are the datasource bridge over the guarded System (same boundary as built-in tools).
    if let Some(dir) = plugins_dir() {
        for p in flux_plugin::discover(&dir) {
            let system = system.clone();
            let backend = backend.clone();
            let make_caps = move |m: &flux_plugin::PluginManifest| {
                Arc::new(flux_capabilities::DatasourceHostCaps::new(
                    flux_plugin::SystemHostCaps::new(system)
                        .allow_private_net(allow_private)
                        .with_manifest(m),
                    backend,
                )) as Arc<dyn flux_plugin::HostCapabilities>
            };
            match flux_plugin::load_plugin_tools(
                &p.descriptor.program,
                &p.descriptor.args,
                make_caps,
            )
            .await
            {
                Ok((tools, _host)) => extra_tools.extend(tools),
                Err(e) => eprintln!(
                    "{}",
                    style::dim(&format!("(plugin `{}` failed to load: {e})", p.name))
                ),
            }
        }
    }

    let channel_decls = program.channels.clone();
    let app = std::sync::Arc::new(flux_app::App::with_tools(
        program,
        provider,
        model,
        auto_approve,
        extra_tools,
    ));
    let channels = flux_channels::build_channels(&channel_decls)?;
    // Serve stdin when an interactive `cli` channel is declared, or when the program declares no
    // channels at all (preserving the plain read-eval-print behavior).
    let run_stdin = channel_decls.is_empty() || channel_decls.iter().any(|c| c.kind == "cli");
    let cancel = tokio_util::sync::CancellationToken::new();
    flux_channels::serve(app, channels, run_stdin, cancel).await
}

/// Launch the HTTP API daemon.
async fn run_serve(flags: AgentFlags, addr: String) -> Result<()> {
    if !flags.yes {
        bail!("`flux serve` requires `--yes` (HTTP requests have no interactive approver)");
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
    let (agent, _session_id, _spawner) = build_agent(&flags).await?;
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
async fn run_tui(flags: AgentFlags) -> Result<()> {
    let auto_approve = flags.yes;
    let (agent, session_id, _spawner) = build_agent(&flags).await?;
    flux_tui::run(agent, session_id, auto_approve).await
}

/// `flux plugin add <name> <program> [args…] | ls | pin <name> <version> | rollback <name>`.
async fn run_plugin(action: Option<PluginAction>) -> Result<()> {
    let dir = plugins_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    match action.unwrap_or(PluginAction::Ls) {
        PluginAction::Ls => {
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
        PluginAction::Add {
            name,
            program,
            args,
        } => {
            flux_plugin::add_descriptor(
                &dir,
                &name,
                &flux_plugin::PluginDescriptor {
                    program: program.clone(),
                    args,
                    pinned: None,
                },
            )
            .context("write plugin descriptor")?;
            println!("added plugin `{name}` → {program}");
            Ok(())
        }
        PluginAction::Pin { name, version } => {
            flux_plugin::set_pinned(&dir, &name, Some(version.clone())).context("pin plugin")?;
            println!("pinned `{name}` to {version}");
            Ok(())
        }
        PluginAction::Rollback { name } => {
            flux_plugin::set_pinned(&dir, &name, None).context("rollback plugin")?;
            println!("cleared pin on `{name}`");
            Ok(())
        }
        PluginAction::Call { name, op, input } => {
            let desc = flux_plugin::load_descriptor(&dir, &name)
                .context("load plugin descriptor")?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no such plugin `{name}` — add it with `flux plugin add`/`install` first"
                    )
                })?;
            let input: serde_json::Value = match input {
                Some(s) => serde_json::from_str(&s).context("parse <json-input>")?,
                None => serde_json::json!({}),
            };
            // The same guarded boundary + datasource bridge the agent path uses, over a scratch index.
            let system = Arc::new(System::new(
                Workspace::new(&std::env::current_dir()?).map_err(|e| anyhow::anyhow!("{e}"))?,
            ));
            let backend: Arc<dyn flux_capabilities::DatasourceBackend> =
                Arc::new(flux_capabilities::MemoryBackend::new());
            let mut host = flux_plugin::PluginHost::spawn(&desc.program, &desc.args)
                .await
                .with_context(|| format!("spawn plugin `{name}` ({})", desc.program))?;
            let manifest = host.manifest().await.context("fetch plugin manifest")?;
            let caps = flux_capabilities::DatasourceHostCaps::new(
                flux_plugin::SystemHostCaps::new(system).with_manifest(&manifest),
                backend.clone(),
            );
            let result = host.call_with_host(&op, input, &caps).await;
            let _ = host.shutdown().await;
            let value = result.map_err(|e| anyhow::anyhow!("plugin `{name}` op `{op}`: {e}"))?;
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
        PluginAction::Install { dir: bin_dir } => {
            let bin_dir = std::path::PathBuf::from(
                bin_dir.unwrap_or_else(|| "plugins/target/release".to_string()),
            );
            let mut installed = 0usize;
            for (name, program) in plugin_binaries_in(&bin_dir)
                .with_context(|| format!("scan {}", bin_dir.display()))?
            {
                flux_plugin::add_descriptor(
                    &dir,
                    &name,
                    &flux_plugin::PluginDescriptor {
                        program: program.clone(),
                        args: Vec::new(),
                        pinned: None,
                    },
                )
                .with_context(|| format!("register plugin `{name}`"))?;
                println!("installed `{name}` → {program}");
                installed += 1;
            }
            if installed == 0 {
                eprintln!(
                    "no `flux-plugin-*` binaries in {} (build them first: \
                     `cd plugins && cargo build --release`)",
                    bin_dir.display()
                );
            }
            Ok(())
        }
        PluginAction::Skill {
            install,
            global,
            out,
        } => run_plugin_skill(&dir, install, global, out).await,
    }
}

/// Render the generated `flux-plugins` skill from the installed plugins' manifests (story D-13). Spawns
/// each plugin only to fetch its manifest (no op call); a plugin that fails to spawn/manifest is skipped
/// with a note rather than aborting the whole catalog.
async fn run_plugin_skill(
    dir: &std::path::Path,
    install: bool,
    global: bool,
    out: Option<String>,
) -> Result<()> {
    let mut plugins: Vec<(String, flux_plugin::PluginManifest)> = Vec::new();
    for p in flux_plugin::discover(dir) {
        match flux_plugin::PluginHost::spawn(&p.descriptor.program, &p.descriptor.args).await {
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
    let rendered = plugin_skill::render_plugin_skill(&plugins);

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
        let base = skill_install_dir(global)?;
        std::fs::create_dir_all(&base).with_context(|| format!("create {}", base.display()))?;
        let skill_file = base.join("SKILL.md");
        std::fs::write(&skill_file, &rendered.skill_md)
            .with_context(|| format!("write {}", skill_file.display()))?;
        write_skill_references(&base.join("references"), &rendered.references)?;
        println!(
            "installed flux-plugins skill → {} ({} plugin(s), {} reference(s))",
            base.display(),
            plugins.len(),
            rendered.references.len()
        );
        return Ok(());
    }

    print!("{}", rendered.skill_md);
    Ok(())
}

/// Where `flux plugin skill --install` writes: the project `.flux/skills/flux-plugins` (highest skill
/// precedence) or, with `--global`, the user-global `~/.claude/skills/flux-plugins`.
fn skill_install_dir(global: bool) -> Result<std::path::PathBuf> {
    if global {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        Ok(home.join(".claude").join("skills").join("flux-plugins"))
    } else {
        Ok(std::env::current_dir()?
            .join(".flux")
            .join("skills")
            .join("flux-plugins"))
    }
}

/// Write each generated `references/<plugin>.md` into `dir` (created on demand).
fn write_skill_references(dir: &std::path::Path, refs: &[(String, String)]) -> Result<()> {
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

/// Find every `flux-plugin-<name>` executable in `dir`, returning `(name, absolute-program-path)`
/// pairs sorted by name. Skips sidecar files (e.g. `*.d`). Missing dir is an error (the caller reports).
fn plugin_binaries_in(dir: &std::path::Path) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Only `flux-plugin-<name>` with no extension (skip `flux-plugin-x.d`, etc.).
        let Some(name) = file.strip_prefix("flux-plugin-") else {
            continue;
        };
        if name.is_empty() || name.contains('.') {
            continue;
        }
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

/// `flux auth status | login <provider>`.
async fn run_auth(action: Option<AuthAction>) -> Result<()> {
    match action.unwrap_or(AuthAction::Status) {
        AuthAction::Status => {
            for s in flux_credentials::auth_status() {
                let mark = if s.available { "✓" } else { "·" };
                println!("{mark} {:<11} {}", s.provider, s.source);
            }
            Ok(())
        }
        AuthAction::Login { provider } => match provider.as_str() {
            "claude" => login_claude().await,
            "codex" => bail!(
                "codex login: sign in with the Codex CLI — flux imports `~/.codex/auth.json` automatically"
            ),
            other => bail!("`flux auth login` expects `claude` (got `{other}`)"),
        },
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
async fn run_prompt(flags: AgentFlags, prompt_words: Vec<String>) -> Result<()> {
    let prompt = prompt_words.join(" ");

    if prompt.trim().is_empty() {
        bail!("provide a prompt, e.g. `flux run \"summarize the README\"`");
    }

    // One engine: a prompt always runs the agentic Flux-Lang engine. `-p`/`--print` only means
    // print-and-exit (a chat-only turn just answers in prose; pass `--yes` for non-interactive
    // tool approval). The legacy tool-less raw-completion path is gone — there is one engine.
    run_agentic(&flags, prompt).await
}

#[cfg(test)]
mod tests {
    use super::{
        build_datasources, format_evidence, loop_machinery_label, new_render_suffix,
        plugin_binaries_in, tool_preview, truncate, usage_annotation,
    };
    use serde_json::json;

    /// `build_datasources` walks a `markdown` datasource's directory and ingests its docs into a shared
    /// backend; an unknown `kind` is a clean error.
    #[tokio::test]
    async fn build_datasources_ingests_markdown_and_rejects_unknown_kinds() {
        use flux_lang::program::DatasourceDecl;
        use flux_system::{System, Workspace};

        let dir = std::env::temp_dir().join(format!("flux-ds-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("note.md"), "# Title\nhello from a markdown note").unwrap();
        let system = System::new(Workspace::new(&dir).unwrap());

        let ok = vec![DatasourceDecl {
            name: "docs".into(),
            kind: "markdown".into(),
            path: Some(".".into()),
            settings: serde_json::Value::Null,
        }];
        let backend = build_datasources(&ok, &system).await.unwrap();
        assert!(!backend.is_empty(), "the markdown note was ingested");

        let bad = vec![DatasourceDecl {
            name: "x".into(),
            kind: "nope".into(),
            path: None,
            settings: serde_json::Value::Null,
        }];
        assert!(
            build_datasources(&bad, &system).await.is_err(),
            "an unknown datasource kind is a clean error"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `flux plugin install` scans a directory for `flux-plugin-<name>` executables: it picks those up
    /// (sorted, by stripped name) and skips sidecars (`*.d`), non-prefixed files, and an empty name.
    #[test]
    fn plugin_binaries_in_picks_flux_plugin_executables() {
        let dir = std::env::temp_dir().join(format!("flux-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "flux-plugin-gitlab",
            "flux-plugin-slack",
            "flux-plugin-slack.d", // a cargo sidecar — must be skipped
            "flux-plugin-",        // empty name — skipped
            "not-a-plugin",        // wrong prefix — skipped
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let found = plugin_binaries_in(&dir).unwrap();
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["gitlab", "slack"]);
        // programs are absolute (canonicalized) paths to the binaries
        assert!(found.iter().all(|(_, p)| p.contains("flux-plugin-")));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The turn-end token annotation reports all four figures the user asked for: context-window
    /// occupancy (fresh input + both cache tiers), generated output, the cached tokens, and the
    /// hit-rate (cached ÷ context). It is empty when nothing was billed (offline `-m mock`).
    #[test]
    fn usage_annotation_shows_context_output_and_cache_hit_rate() {
        use flux_core::Usage;
        // 1000 fresh + 9000 cache-read = 10k context; 9000/10000 = 90% hit.
        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_input_tokens: 9_000,
            cache_creation_input_tokens: 0,
            reasoning_tokens: 0,
        };
        let s = usage_annotation(&u);
        assert_eq!(s, " · ctx 10.0k · out 500 · cache 9.0k (90% hit)");

        // No cache → no cache segment, but context + output still show.
        let u = Usage {
            input_tokens: 320,
            output_tokens: 80,
            ..Default::default()
        };
        assert_eq!(usage_annotation(&u), " · ctx 320 · out 80");

        // Nothing billed → empty (so `-m mock` turns render a clean rule).
        assert_eq!(usage_annotation(&Usage::default()), "");
    }

    /// clap validates the whole command tree (catches duplicate arg ids, the global-args + subcommand
    /// wiring, conflicts) at test time rather than only when `flux --help` is first run.
    #[test]
    fn cli_command_tree_is_valid() {
        use clap::CommandFactory;
        super::Cli::command().debug_assert();
    }

    /// Every subcommand is registered so `flux --help` / `flux <cmd> --help` are complete.
    #[test]
    fn help_lists_every_subcommand() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let names: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
        for want in [
            "run",
            "plan",
            "tui",
            "serve",
            "eval",
            "flow",
            "loop",
            "sessions",
            "auth",
            "plugin",
            "completion",
            "preset",
        ] {
            assert!(
                names.contains(&want),
                "missing subcommand `{want}` in {names:?}"
            );
        }
    }

    /// The top level is clean: its only declared flag is the global `--color`. No agent/turn flags or
    /// the promoted mode flags (`serve`/`tui`/`plan`) leak onto it — they live on the subcommands now.
    /// Inspecting the declared arguments (not the rendered text) avoids false hits on flag names that
    /// appear inside a subcommand's *description*.
    #[test]
    fn top_level_has_only_the_color_flag() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let longs: Vec<String> = cmd
            .get_arguments()
            .filter_map(|a| a.get_long().map(String::from))
            .collect();
        for leaked in [
            "max-tokens",
            "model",
            "yes",
            "serve",
            "tui",
            "plan",
            "continue",
            "verbose",
        ] {
            assert!(
                !longs.iter().any(|l| l == leaked),
                "top-level leaks --{leaked}: {longs:?}"
            );
        }
        assert!(
            longs.iter().any(|l| l == "color"),
            "top-level missing --color: {longs:?}"
        );
    }

    /// `flux eval --help` carries its own typed flags + the adapter list (the original ask).
    #[test]
    fn eval_help_documents_its_flags() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let eval = cmd.find_subcommand("eval").expect("eval subcommand");
        let help = eval.clone().render_long_help().to_string();
        for want in ["--watch", "--report", "--tasks", "--members", "synthetic"] {
            assert!(help.contains(want), "`flux eval --help` missing {want:?}");
        }
    }

    /// The turn flags are scoped to the agent path (`run` + top-level), not leaked onto other
    /// subcommands' help — and `eval` carries only its own `-m`, not the full turn-flag set.
    #[test]
    fn agent_flags_are_scoped_off_other_subcommands() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let help_of = |name: &str| {
            cmd.find_subcommand(name)
                .unwrap_or_else(|| panic!("subcommand {name}"))
                .clone()
                .render_long_help()
                .to_string()
        };
        for sub in ["sessions", "loop", "completion", "auth", "plugin"] {
            let h = help_of(sub);
            assert!(
                !h.contains("--max-tokens"),
                "`{sub} --help` leaks --max-tokens"
            );
            assert!(!h.contains("--continue"), "`{sub} --help` leaks --continue");
        }
        // The agent-path subcommands (`run`/`plan`/`tui`/`serve`) carry the turn flags; `eval` has its
        // own `-m` but not `--max-tokens`.
        for agent_cmd in ["run", "plan", "tui", "serve"] {
            assert!(
                help_of(agent_cmd).contains("--max-tokens"),
                "`{agent_cmd} --help` should carry the turn flags"
            );
        }
        let eval = help_of("eval");
        assert!(eval.contains("--model"), "eval should keep its own --model");
        assert!(
            !eval.contains("--max-tokens"),
            "eval should not carry the turn flags"
        );
    }

    #[test]
    fn truncate_caps_with_ellipsis() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 3), "hel…");
    }

    #[test]
    fn format_evidence_empty_is_a_hint() {
        let log = flux_evidence::EvidenceLog::new();
        assert!(format_evidence(&log).contains("no evidence recorded yet"));
    }

    #[test]
    fn format_evidence_summarizes_and_lists_observations() {
        use flux_evidence::{EvidenceLog, Observation, Phase};
        let mut log = EvidenceLog::new();
        log.record(Observation::new(
            "tool_call",
            Phase::Turn,
            json!({"tool": "read"}),
        ));
        log.record(Observation::new(
            "tool_error",
            Phase::Turn,
            json!({"tool": "cargo_test"}),
        ));
        log.record(Observation::new(
            "turn.iteration",
            Phase::Turn,
            json!({"steps": 3}),
        ));

        let out = format_evidence(&log);
        // Summary line counts observations, iterations, and errors (correctly pluralized).
        assert!(out.contains("3 observations"), "{out}");
        assert!(out.contains("1 iteration,"), "singular iteration: {out}");
        assert!(out.contains("1 error"), "{out}");
        // Each observation kind is listed verbatim (the kind column is not colored).
        assert!(out.contains("tool_call"), "{out}");
        assert!(out.contains("tool_error"), "{out}");
        assert!(out.contains("turn.iteration"), "{out}");
    }

    #[test]
    fn loop_machinery_label_only_relabels_machinery_ops() {
        assert!(loop_machinery_label("plan", &json!({}))
            .unwrap()
            .contains("ask the model"));
        assert!(loop_machinery_label("run_plan", &json!({}))
            .unwrap()
            .contains("run plan"));
        // `observe` surfaces its kind; ordinary ops fall through (None) to the normal label path.
        assert!(
            loop_machinery_label("observe", &json!({"kind": "turn.iteration"}))
                .unwrap()
                .contains("turn.iteration")
        );
        assert!(loop_machinery_label("read", &json!({"file": "x"})).is_none());
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

    #[test]
    fn a2a_render_suffix_handles_delta_and_snapshot() {
        // Delta stream: each chunk is new; nothing is the prior prefix → render the whole chunk.
        assert_eq!(new_render_suffix("Hello wor", "ld"), "ld");
        assert_eq!(new_render_suffix("", "Hello"), "Hello");
        // Snapshot stream: each event repeats the whole text so far → render only the new tail.
        assert_eq!(new_render_suffix("Hello", "Hello world"), " world");
        assert_eq!(new_render_suffix("Hello world", "Hello world"), "");
        // A delta that coincidentally doesn't extend the prefix is rendered verbatim.
        assert_eq!(new_render_suffix("abc", "xyz"), "xyz");
    }
}
