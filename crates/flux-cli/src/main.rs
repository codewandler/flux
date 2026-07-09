//! The `flux` binary.
//!
//! M0 surface: a one-shot mode that streams a single Anthropic response to stdout. The
//! interactive REPL and TUI land in M2; this establishes the end-to-end path
//! (CLI → provider → stream → render).

mod changelog;
mod plugin_skill;
mod preset;
mod skill_cmd;
mod style;
mod usage;

use std::io::{IsTerminal, Write};

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser};
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
        `flux tui` is the chat UI, and `flux app run <program.flux>` runs a multi-agent program \
        (add `--serve <addr>` to expose an agent over HTTP/A2A). Run `flux help` for the full list of \
        commands."
)]
struct Cli {
    /// A subcommand (run `flux help` to list them). With none, `flux` opens the interactive REPL.
    #[command(subcommand)]
    command: Option<Commands>,

    /// When to colorize output: auto (a terminal, `NO_COLOR` unset), always, or never.
    #[arg(long, value_enum, default_value_t, global = true)]
    color: style::ColorChoice,

    /// Grant READ access to an additional directory outside the workspace (repeatable). Reads, `glob`,
    /// and `grep` may reach under it; writes stay confined to the current directory. Layers over
    /// `[workspace] add_dirs` in .flux/config.toml (exported as `FLUX_ADD_DIRS` so `app run` inherits it).
    #[arg(long = "add-dir", value_name = "DIR", global = true)]
    add_dir: Vec<std::path::PathBuf>,

    /// Lift the filesystem sandbox entirely — read AND write anywhere on disk. Dangerous; prints a
    /// warning. Prefer `--add-dir` for read-only access to specific directories.
    #[arg(long = "allow-all-paths", global = true)]
    allow_all_paths: bool,

    /// Temporarily allow egress to private/internal network addresses for THIS invocation only —
    /// the ephemeral, audited equivalent of a `[private_net]` config grant (no config edit, nothing
    /// persisted). Plugins still only reach the private hosts their manifest declares; `web_fetch`
    /// is opened for the run (its guard has no manifest safeguard, so this re-exposes cloud-metadata
    /// and RFC-1918 ranges to any fetched URL). Prefer a scoped `[private_net.plugins]` grant for
    /// anything recurring. Exported as `FLUX_ALLOW_PRIVATE_NET` so `app run`/`plugin call` inherit it.
    #[arg(long = "allow-private-net", global = true)]
    allow_private_net: bool,
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
    ///   `anthropic` (API key), `claude` (OAuth/subscription), `openai`, `codex`, `aws` (Claude
    ///   via AWS Bedrock; credentials from the AWS chain — env, `aws sso login` + `AWS_PROFILE`,
    ///   IRSA, or EKS Pod Identity — no `aws` CLI needed), `openrouter` (OpenAI Chat wire),
    ///   `openrouter-anthropic` (OpenRouter's native Messages endpoint — leak-proof tool calls),
    ///   `ollama` (local, OpenAI Chat wire), `ollama-anthropic` (local Messages endpoint).
    ///   Short aliases `sonnet`, `opus`, `haiku` are shorthands for `anthropic/<model>`; bare
    ///   `codex` is shorthand for `codex/gpt-5.5` (the ChatGPT-subscription main model; the
    ///   legacy `*-codex` ids are rejected by the backend); bare `aws` (or `aws/sonnet`,
    ///   `aws/opus`, `aws/haiku`) resolves to the region's Bedrock inference profile.
    /// Examples: `claude/claude-sonnet-4-6`, `openai/gpt-4o`, `codex/gpt-5.5`,
    ///   `aws/us.anthropic.claude-sonnet-4-6`, `openrouter-anthropic/z-ai/glm-4.6`.
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

    /// Per-turn token budget (all tiers, summed across the turn's model calls): once crossed, the
    /// turn ends honestly with a budget-exceeded answer instead of consulting the model again.
    /// Overrides `FLUX_TURN_TOKEN_BUDGET` and `[limits] turn_token_budget` in .flux/config.toml.
    /// Off by default (no ceiling).
    #[arg(long)]
    turn_budget: Option<u64>,

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

    /// Trace the outer agent loop's structure: one dim line per round (`⟳ round 3/25`) and per
    /// structural node (op calls with bind names, match/when branches taken, return) of the
    /// agent-loop program. Inner plan execution is not traced. Also enabled by `FLUX_TRACE_LOOP`.
    #[arg(long)]
    trace_loop: bool,

    /// Extra skill directory, layered above `[skills] dirs` from .flux/config.toml and the
    /// well-known set (`.flux/skills`, `.claude/skills`, `~/.flux/skills`, …). Repeatable; earlier
    /// dirs win a skill-name clash.
    #[arg(long = "skill-dir", value_name = "DIR")]
    skill_dirs: Vec<std::path::PathBuf>,

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
    /// Fork a recorded session at a decision point (A-46): the prefix replays hermetically from
    /// the cassette (no side effects), then the tail DIVERGES live through the real approval
    /// envelope — inject a different value, run an edited plan, or let the model re-plan.
    Fork {
        /// Session id (`s_42`), or `last` for the most recent session.
        session: String,
        /// Top-level statement index (0-based) of the run's FINAL executed plan to diverge at.
        #[arg(long)]
        at: usize,
        /// Mode A: inject this JSON value as the fork statement's result, then run the rest live.
        #[arg(long, conflicts_with_all = ["edit", "replan"])]
        inject: Option<String>,
        /// Mode C: continue with this edited plan file (.flux text or JSON DraftAst) — unchanged
        /// leading statements fast-forward against the replayed prefix, edits run live.
        #[arg(long, conflicts_with_all = ["inject", "replan"])]
        edit: Option<String>,
        /// Mode B (default): let the model re-plan the tail live from the forked state.
        #[arg(long)]
        replan: bool,
        /// With --replan: the instruction for the re-planned tail (default: continue the
        /// recorded task).
        #[arg(long)]
        prompt: Option<String>,
        #[command(flatten)]
        agent: AgentFlags,
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
    ///
    /// Reusable flows and composite ops live as `.flux` files under `.flux/flows` (project) and
    /// `~/.flux/flows` (global): the agent discovers them with the `flow_list` tool and runs them
    /// with `flow_run`, and composite `op`s placed there auto-load as callable ops. (The legacy
    /// `~/.flux/ops` / `.flux/ops` dirs are still read.)
    Flow {
        #[command(subcommand)]
        action: FlowAction,
    },
    /// Run the strict-review protocol over `--files` and print a `ReviewReport` (flux L-13; design
    /// `docs/designs/strict-review-flows.md`). Self-contained: the reviewer roles and the
    /// `strict_review` flow are embedded in the binary, so this works in any repo — a project's own
    /// `.flux/agents/review-*.md` still overrides the built-in role definitions. Read-only: this never
    /// posts anywhere, it only prints to stdout.
    Review {
        #[command(flatten)]
        agent: AgentFlags,
        /// Files to review (at least one).
        #[arg(long = "files", required = true, num_args = 1..)]
        files: Vec<String>,
        /// Output format: `md` (a readable findings summary, the default) or `json` (the raw
        /// `ReviewReport`).
        #[arg(long, value_enum, default_value_t)]
        format: ReviewFormat,
        /// Exit 1 if any finding's severity is at or above this threshold (`info`|`low`|`medium`|
        /// `high`|`critical`). Omit to always exit 0 regardless of findings.
        #[arg(long, value_enum)]
        fail_on: Option<ReviewSeverity>,
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
    /// Per-model token usage + cost across flux and detected local agent harnesses.
    Usage(usage::UsageArgs),
    /// Hermetically replay a recorded session (A-45): plans re-parse from the durable
    /// `plan_source`, op outputs are served from the C-43 cassette — no model call, no live IO,
    /// side effects never re-fired. Divergence from the recording fails loudly.
    Replay {
        /// Session id (`s_42`), or `last` for the most recent session.
        #[arg(default_value = "last")]
        session: String,
        /// Replay only this turn's plans (1-based). Cross-turn symbol references fail honestly.
        #[arg(long)]
        turn: Option<usize>,
        /// Also replay this session's sub-agent child streams (A-08 correlation), in spawn order.
        #[arg(long)]
        sub_agents: bool,
        /// Emit a machine-readable JSON report instead of the human summary.
        #[arg(long)]
        json: bool,
    },
    /// Diff two recorded runs (C-44): align their executed statements and show exactly where the
    /// PLAN changed (differing statement content) vs where the same plan hit a DIFFERENT WORLD
    /// (differing recorded op output). Exit code 1 when the runs diverge, `diff`-style.
    Diff {
        /// First session id (`s_42`), or `last`.
        a: String,
        /// Second session id (e.g. a fork of the first).
        b: String,
        /// Emit a machine-readable JSON report.
        #[arg(long)]
        json: bool,
    },
    /// Mine `~/.flux/events.db` for flux-native NL→Flux-Lang training data (D-53).
    Corpus {
        #[command(subcommand)]
        action: CorpusAction,
    },
    /// Provider authentication (status / login).
    Auth {
        #[command(subcommand)]
        action: Option<AuthAction>,
    },
    /// The plugin CLI — manage subprocess plugins (any-language ops).
    ///
    /// Lifecycle over the signed plugin pack (`plugins-v*` releases): `install` (verified remote;
    /// `--dir` registers local builds), `pin`/`rollback` (enforced version switches over the
    /// versioned store), plus `ls`/`status`/`call`/`uninstall`/`skill`.
    Plugin {
        #[command(subcommand)]
        action: Option<PluginAction>,
    },
    /// Inspect the persisted endpoint store (`~/.flux/endpoints.toml`). Operator-only, weak refs only —
    /// never prints a secret value.
    Endpoint {
        #[command(subcommand)]
        action: EndpointAction,
    },
    /// Render or install generated Claude-format Flux skills.
    Skill {
        /// Which section to render/install: cli | lang | plugin | ops. Omit for the root skill.
        #[arg(value_enum, value_name = "TYPE")]
        type_: Option<skill_cmd::SkillType>,
        /// Install generated skill directories instead of printing SKILL.md to stdout.
        #[arg(long)]
        install: bool,
        /// With `--install`, target the user-global `~/.claude/skills` instead of project `.flux/skills`.
        #[arg(long)]
        global: bool,
    },
    /// Show what changed in flux, in plain language (the customer changelog).
    Changelog {
        /// Show a specific version's section (e.g. `0.11.6`).
        version: Option<String>,
        /// Show every recorded release.
        #[arg(long)]
        all: bool,
        /// Show the not-yet-released section (development builds).
        #[arg(long)]
        unreleased: bool,
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
    ///
    /// `--serve <addr>` additionally exposes an agent over the HTTP/A2A API (sessions + SSE + A2A +
    /// agent-card) for testing. With no `<program>`, `--serve` serves flux's built-in coding agent —
    /// the former `flux serve`.
    Run {
        #[command(flatten)]
        agent: AgentFlags,
        /// Path to the `<program.flux>` multi-agent program. Optional when `--serve` is given (then the
        /// built-in coding agent is served).
        program: Option<String>,
        /// Expose an agent over the HTTP/A2A API at this address (defaults to `127.0.0.1:8787`). With a
        /// program, serves its agent; with none, serves the built-in coding agent. Requires `--yes`.
        #[arg(
            long,
            value_name = "ADDR",
            num_args = 0..=1,
            default_missing_value = "127.0.0.1:8787"
        )]
        serve: Option<String>,
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
        /// Opt into resumable mode (L-25): a halt (a failed top-level statement, or the L-24
        /// reified `await` pause) prints a structured halt report — a ✓/✗/· marked statement tree,
        /// a machine-readable failure summary, and the session id — and exits non-zero, instead of
        /// erroring the whole run. Implied by `--resume`.
        #[arg(long)]
        resumable: bool,
        /// Resume a previously halted run of THIS file: a literal session id (printed by the halt
        /// report), or `last` — the most recent halted `flow run` session for this flow's declared
        /// name (`flow <name> -> …`; an unnamed flow can't be disambiguated this way and needs the
        /// explicit session id). Re-parses this (possibly corrected) file, folds the halted
        /// session's statement ledger, fast-forwards the matching completed prefix (values
        /// rehydrated), and executes from the first changed statement.
        #[arg(long, value_name = "SESSION|last")]
        resume: Option<String>,
        /// The payload to bind to a resumed top-level `await` (`$reply = await …`). Parsed as JSON, so
        /// a bare word is a JSON string (`--resume-value hi` binds `"hi"`) and `--resume-value 42`
        /// binds the number. Required when `--resume`-ing a session that halted awaiting a value; omit
        /// it for a plain checkpoint/failure resume. Without it, resuming past an unbound await refuses
        /// with a clear error instead of failing later on `unbound symbol`.
        #[arg(long, value_name = "JSON")]
        resume_value: Option<String>,
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

/// `flux corpus …`
#[derive(clap::Subcommand, Debug)]
enum CorpusAction {
    /// Pair every accepted plan's canonical text (`plan_source`, L-38) with the user instruction
    /// that produced it and emit corpus-shaped JSONL (one row per line):
    /// `{id, nl_goal, source, provenance: {session, turn}, flux_rev}`. Reads events.db read-only;
    /// prints a skip-count summary to stderr (precision over recall — an ambiguous or pre-L-38 row
    /// is dropped and counted, never guessed at).
    Export {
        /// Write JSONL to this file instead of stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
}

/// `flux auth …`
#[derive(clap::Subcommand, Debug)]
enum AuthAction {
    /// Show which providers are configured (the default).
    Status,
    /// Log in to a provider (`claude`/`codex`) or an installed OAuth2 plugin (by name).
    Login {
        /// Provider (`claude`/`codex`) or installed plugin name to log in to.
        provider: String,
        /// Use the OAuth2 password grant (prompt for username + password) instead of the browser
        /// PKCE flow — for a plugin whose OAuth2 method supports it.
        #[arg(long)]
        password: bool,
    },
}

/// `flux plugin …`
#[derive(clap::Subcommand, Debug)]
enum PluginAction {
    /// List installed plugins (the default).
    #[command(alias = "list")]
    Ls,
    /// Add a plugin: `add <name> <program> [args…]`.
    Add {
        name: String,
        program: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Pin a plugin to a pack version — a verified version switch (D-48): `pin <name> <version>`.
    /// The version is fetched into the versioned store if absent (the same signed-index +
    /// checksum path as `install`; already-stored versions repoint offline), the descriptor is
    /// repointed with its sha256 recorded (re-checked at every spawn — drift refuses to run),
    /// and the replaced version is remembered for `rollback`.
    Pin { name: String, version: String },
    /// Roll back to the version in place before the last switch: `rollback <name>` — offline and
    /// instant (the side-by-side versioned store keeps it on disk). Current and previous swap,
    /// so a second `rollback` flips forward again. (Pre-D-48 this merely cleared the advisory
    /// pin; it now switches versions.)
    Rollback { name: String },
    /// Invoke one operation of an installed plugin directly: `call <name> <op> [json-input]`
    /// (alias: `run`). Input is built from the optional `<json-input>` object plus any
    /// `--arg key=value` flags (coerced to the op's declared `input_schema` types and merged
    /// over the JSON base). `--dry-run` validates locally against the schema and prints the
    /// coerced input without spawning the plugin; `--no-validate` skips schema coercion/validation.
    #[command(alias = "run")]
    Call {
        name: String,
        op: String,
        /// JSON input object for the operation (default `{}`; `--arg` values merge over this).
        input: Option<String>,
        /// `key=value` arg coerced to the op's declared schema type (string/integer/boolean/
        /// array/object). Repeatable; values merge over `<json-input>`.
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        arg: Vec<String>,
        /// Validate the input against the op's schema locally and print the coerced input +
        /// any problems — never spawn the plugin.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Skip schema coercion/validation of `--arg` values (pass them through as strings).
        #[arg(long = "no-validate")]
        no_validate: bool,
    },
    /// Install plugins from the signed `plugins-v*` pack release (D-47): `install <name>[@<version>] …`
    /// (resolves the newest pack release, or an exact `plugins-v<version>` tag), or `install --all`
    /// for the whole pack. Verifies the index signature and every archive's sha256 before unpacking
    /// into the versioned store `~/.flux/plugins/bin/<name>/<version>/`. Re-installing a version
    /// already present is an idempotent no-op.
    ///
    /// `install --dir [path]` is the other mode — the pre-D-47 local scan, registering every
    /// `flux-plugin-*` binary already built in a directory (default `plugins/target/release`) with
    /// no version/hash recorded. The two modes are exclusive; bare `install` with neither plugin
    /// names/`--all` nor `--dir` is an error naming both.
    Install {
        /// Plugin name(s) to install, each optionally pinned to `@<version>` (remote mode).
        names: Vec<String>,
        /// Install every plugin in the pack (remote mode).
        #[arg(long)]
        all: bool,
        /// Scan a local directory for already-built `flux-plugin-*` binaries instead of the
        /// remote pack channel (local-scan mode; defaults to `plugins/target/release` when given
        /// with no value).
        #[arg(
            long,
            value_name = "PATH",
            num_args = 0..=1,
            default_missing_value = "plugins/target/release"
        )]
        dir: Option<String>,
    },
    /// Remove an installed plugin descriptor: `uninstall <name>`. With `--purge`, also delete
    /// the plugin's versioned-store directory (`~/.flux/plugins/bin/<name>/`) — every downloaded
    /// version, including what `rollback` would flip to.
    Uninstall {
        name: String,
        /// Also remove the plugin's versioned binary store (all downloaded versions).
        #[arg(long)]
        purge: bool,
    },
    /// Log in to this plugin's OAuth2 provider (alias for `flux auth login <name>`): runs the browser
    /// PKCE flow (or `--password`) and stores the tokens so a later `call` needs no env token.
    Login {
        /// Installed plugin name.
        name: String,
        /// Use the OAuth2 password grant instead of the browser PKCE flow.
        #[arg(long)]
        password: bool,
    },
    /// Inspect installed plugins — liveness + declared surface: `status [<name>]`.
    /// With no argument it summarizes every installed plugin; `ls` stays the terse default.
    Status {
        /// One plugin to inspect in full; omit for every installed plugin.
        name: Option<String>,
    },
    /// Generate the plugin section skill from installed plugin manifests.
    /// Alias for `flux skill plugin`; prints to stdout by default, or installs with `--install`.
    Skill {
        /// Write the SKILL.md + references/ into the project `.flux/skills/flux-plugin`.
        #[arg(long)]
        install: bool,
        /// With `--install`, target the user-global `~/.claude/skills/flux-plugin` instead.
        #[arg(long)]
        global: bool,
        /// Write the SKILL.md to this single file (references go in a sibling `references/`).
        #[arg(long)]
        out: Option<String>,
    },
}

/// `flux endpoint …` — the operator mirror of the agent's `endpoint.*` ops over the persisted
/// `~/.flux/endpoints.toml` store. Every subcommand deals in weak references only: it shows the
/// credential *location* (the `credential_ref`), never a value.
#[derive(clap::Subcommand, Debug)]
enum EndpointAction {
    /// List the persisted endpoint records (id, product, bare URL, owner, ttl/health, credential
    /// location) — never a secret value.
    List,
    /// Show one persisted record in full by id (still reference-only).
    Show {
        /// Endpoint id (e.g. `@endpoint/<ns>-<name>`).
        id: String,
    },
    /// Report what a reference WOULD bind to at connect time: source, bare host/url, and the
    /// credential-ref *location* — explicitly NOT the secret value (an operator diagnostic).
    Resolve {
        /// Endpoint id to diagnose.
        id: String,
    },
    /// Persist a record into `~/.flux/endpoints.toml` (weak-ref only; re-resolved live each session).
    /// Reads the record from the persisted store, or accepts `--from-json <EndpointRef>`.
    Import {
        /// Endpoint id to import.
        id: String,
        /// A weak `EndpointRef` (JSON) to import directly when the id is not already in the store.
        #[arg(long, value_name = "JSON")]
        from_json: Option<String>,
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

/// `flux review --format` output mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
enum ReviewFormat {
    /// A readable markdown findings summary (the default).
    #[default]
    Md,
    /// The raw `ReviewReport` JSON.
    Json,
}

/// `flux review --fail-on` severity threshold, ordered low → high so `>=` comparisons are meaningful.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
enum ReviewSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl ReviewSeverity {
    /// Parse a `ReviewFinding.severity` string into the same ordering. Unlike
    /// `flux_tools::cognition`'s ranking (which sorts an unrecognized severity as the *lowest* tier,
    /// safe for stable ordering), an unrecognized value here maps to `Critical` — the *highest* tier
    /// — so a malformed or unexpected severity string can never silently slip under a `--fail-on`
    /// threshold; the gate fails safe (an unparseable severity trips it) rather than failing open.
    fn from_finding_str(s: &str) -> Self {
        match s {
            "info" => Self::Info,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            _ => Self::Critical,
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

// Codex/Anthropic model resolution is backend knowledge owned by each provider crate
// (`flux_providers::codex::resolve_model`, `flux_providers::anthropic::resolve_model`) so every
// surface — CLI, SDK, server, TUI, the L3 sub-agent spawner — shares one owner instead of each
// carrying its own alias table. The CLI owns only the *shorthand policy*: bare `codex` (no model)
// means "use the provider default".

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
    "aws",
    "openrouter",
    "openrouter-anthropic",
    "ollama",
    "ollama-anthropic",
];

/// The provider prefix a `provider/model` spec resolves to — the part before `/`, or a bare short
/// alias mapped to its provider (`sonnet`/`opus`/`haiku`/`mock` → `anthropic`, bare `codex`/`aws` →
/// themselves). `None` for a bare word that is not a known alias. The single source of truth for the
/// bare-alias set, shared by [`build_provider`] and [`auth_row_for_spec`] so the two can never drift.
fn spec_provider_prefix(spec: &str) -> Option<&str> {
    match spec.split_once('/') {
        Some((p, _)) => Some(p),
        None => match spec {
            "sonnet" | "opus" | "haiku" | "mock" => Some("anthropic"),
            "codex" => Some("codex"),
            "aws" => Some("aws"),
            _ => None,
        },
    }
}

/// Parse a fully-qualified `provider/model` spec and build the matching provider from environment
/// credentials. Provider must be an explicit prefix (`anthropic/`, `claude/`, `openai/`, `codex/`,
/// `openrouter/`, `openrouter-anthropic/`, `ollama/`, `ollama-anthropic/`). Bare short aliases
/// (`sonnet`, `opus`, `haiku`) are implicitly `anthropic/<alias>`.
/// Any other bare string (no `/`) is an error — use `anthropic/` or `claude/` to disambiguate.
fn build_provider(spec: &str) -> Result<(NativeProvider, String, String)> {
    // Returns (native, provider, resolved_model) so callers can reconstruct the canonical
    // `provider/model` spec (e.g. for cost/subscription detection, which reads the provider prefix).
    let (provider, model) = match spec.split_once('/') {
        Some((p, m)) if KNOWN_PROVIDERS.contains(&p) => (p.to_string(), m.to_string()),
        Some((p, _)) => bail!(
            "unknown provider `{p}` — use one of: {}",
            KNOWN_PROVIDERS.join(", ")
        ),
        // Bare short aliases only; everything else needs an explicit provider prefix. The alias set
        // lives in `spec_provider_prefix`; the bare model string is the alias itself for the anthropic
        // short-names (`sonnet`/`opus`/`haiku`/`mock`), else the provider's default — bare `codex` →
        // the ChatGPT-subscription main model, bare `aws` → the Bedrock default — resolved below.
        None => match spec_provider_prefix(spec) {
            Some(provider) => {
                let model = if provider == "anthropic" { spec } else { "" };
                (provider.to_string(), model.to_string())
            }
            None => bail!(
                "model spec `{spec}` has no provider prefix — use `provider/model`, e.g. \
                 `anthropic/{spec}` or `claude/{spec}` (providers: {})",
                KNOWN_PROVIDERS.join(", ")
            ),
        },
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
            flux_providers::codex::oauth(ts)
        }
        // AWS Bedrock (Anthropic over SigV4), streaming via invoke-with-response-stream. The full
        // credential chain (env → SSO → IRSA → EKS Pod Identity) is materialized into `AWS_*` env
        // HERE, in the one factory — so every subcommand that builds a provider (`flux review`,
        // `flow run`, `preset --run`, the REPL `/model` swap, the sub-agent factory) gets the
        // chain, not just `build_agent` (C-11). Bedrock bakes the model id into the credential
        // (it's in the invoke URL), so resolve after the chain sets the region.
        "aws" => {
            ensure_aws_chain()?;
            let m = flux_providers::bedrock::resolve_model(&model);
            flux_providers::bedrock::bedrock_with_env(m).context("aws provider")?
        }
        other => bail!(
            "unknown provider `{other}` (known: {})",
            KNOWN_PROVIDERS.join(", ")
        ),
    };

    let model = match provider.as_str() {
        "anthropic" | "claude" => flux_providers::anthropic::resolve_model(&model),
        "codex" => flux_providers::codex::resolve_model(&model),
        "aws" => flux_providers::bedrock::resolve_model(&model),
        _ => model,
    };
    Ok((native, provider, model))
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

/// Discover skills from the project's `.flux/skills` and `.claude/skills` plus the user/global dirs
/// (`~/.flux/skills`, `~/.agents/skills`, `~/.claude/skills`), with custom dirs layered above the
/// well-known set: `--skill-dir` flags first, then `[skills] dirs` from the layered config (project
/// before user) — earlier dirs win a name clash (L-02). Activation (triggers or a description
/// fallback) gates which bodies are injected per turn; discovery reads metadata only.
fn load_skills(
    cwd: &std::path::Path,
    cfg: &flux_config::Config,
    cli_dirs: &[std::path::PathBuf],
) -> Vec<flux_skill::Skill> {
    let mut extra: Vec<std::path::PathBuf> = cli_dirs.to_vec();
    extra.extend(cfg.skill_dir_paths());
    flux_skill::discover_merged(&flux_skill::skill_dirs(cwd, &extra))
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

/// `flux usage` — per-model tokens + cost for the current/last session, and an all-sessions total.
/// Reads the unified event store's `cost_summary` projection (C-06); pricing is the builtin table
/// overlaid by `~/.flux/pricing.toml` (same loader the live turn-end annotation uses).
fn run_usage(args: usage::UsageArgs) -> Result<()> {
    let pricing = flux_credentials::load_pricing_table();
    usage::run_usage(args, &pricing)
}

/// The store-parameterized body of [`run_usage`] (tests pass an in-memory store so they don't touch
/// `HOME`'s real `~/.flux/events.db`).
#[cfg(test)]
fn run_usage_with(store: &EventStore, pricing: &flux_core::PricingTable) -> Result<()> {
    usage::run_usage_with(store, pricing)
}

/// A-45: `flux replay <SESSION|last>` — hermetic offline re-execution of a recorded session.
/// Plans re-parse from the durable `plan_source`, op outputs are served from the C-43 cassette;
/// the lazy provider is never constructed (no model op is ever reached), and no live IO or side
/// effect can fire (a served dispatch never touches the executor). Non-zero exit on divergence,
/// so a recording can be pinned in CI.
async fn run_replay(
    session_arg: &str,
    turn: Option<usize>,
    sub_agents: bool,
    json: bool,
) -> Result<()> {
    let events = Arc::new(open_event_store()?);
    let sid = if session_arg == "last" {
        events
            .latest_session()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .context("no recorded sessions in ~/.flux/events.db")?
    } else {
        events
            .info(session_arg)
            .with_context(|| format!("unknown session `{session_arg}`"))?;
        session_arg.to_string()
    };
    drop(events);

    // Reuse the target session id so no fresh session record is minted; the driver writes only to
    // its own scratch store — replay is a pure read of the recording. `--yes` is safe by
    // construction here: a served op never executes, and the Replay scope auto-allows `confirm`.
    let flags = AgentFlags::from_model_yes(None, true);
    let (engine, _session, _spec, _spawner) = build_agent_lazy(&flags, Some(sid.clone())).await?;
    eprintln!(
        "{}",
        style::dim(&format!(
            "replay · session {sid} · offline (no model call, no live IO)"
        ))
    );

    let mut sink = CliSink::new(0);
    let report =
        flux_flow::replay::replay_session(&engine.events, &engine.executor, &sid, turn, &mut sink)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

    // A-08 tree: child streams replay after the parent, in spawn order (their `task` cells on the
    // parent tape carried only the child's summarized result — each child has its own tape).
    let mut child_reports = Vec::new();
    if sub_agents {
        for child in engine.events.children_of(&sid)? {
            eprintln!("{}", style::dim(&format!("replay · sub-agent {child}")));
            match flux_flow::replay::replay_session(
                &engine.events,
                &engine.executor,
                &child,
                None,
                &mut sink,
            )
            .await
            {
                Ok(r) => child_reports.push(r),
                // A child recorded before C-43 (or with the cassette off) must not sink the
                // parent's result — report it honestly and continue.
                Err(e) => eprintln!("{}", style::dim(&format!("  {child}: {e}"))),
            }
        }
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "session": report.session,
                "plans": report
                    .plans
                    .iter()
                    .map(|p| serde_json::json!({ "flow_key": p.flow_key, "halted": p.halted }))
                    .collect::<Vec<_>>(),
                "cells_total": report.cells_total,
                "cells_consumed": report.cells_consumed,
                "missing_sources": report.missing_sources,
                "diverged": report.diverged,
                "sub_agents": child_reports.iter().map(|r| serde_json::json!({
                    "session": r.session,
                    "cells_total": r.cells_total,
                    "cells_consumed": r.cells_consumed,
                    "diverged": r.diverged,
                })).collect::<Vec<_>>(),
            })
        );
    } else {
        println!(
            "replayed {} plan(s) · {}/{} recorded cell(s) served",
            report.plans.len(),
            report.cells_consumed,
            report.cells_total
        );
        for p in &report.plans {
            match &p.halted {
                Some(h) => println!("  ✗ {} — halted (reproduced): {h}", p.flow_key),
                None => println!("  ✓ {}", p.flow_key),
            }
        }
        if report.missing_sources > 0 {
            eprintln!(
                "{}",
                style::dim(&format!(
                    "note: {} recorded execution(s) have no stored plan_source (pre-L-38 or \
                     oversized) and were skipped",
                    report.missing_sources
                ))
            );
        }
    }
    if let Some(d) = report.diverged {
        bail!("replay diverged from the recording: {d}");
    }
    for r in &child_reports {
        if let Some(d) = &r.diverged {
            bail!(
                "sub-agent {} replay diverged from the recording: {d}",
                r.session
            );
        }
    }
    Ok(())
}

/// A-46: `flux fork <SESSION> --at <N>` — branch a recorded run at a decision point. The prefix
/// replays hermetically from the cassette into a NEW session (correlated to the source; no side
/// effects), then the tail diverges LIVE through the real approval envelope: `--inject` a value,
/// `--edit` a corrected plan, or (default) `--replan` via the model. The forked session records
/// its own cassette, so the fork is itself replayable and diffable against its parent.
async fn run_fork(
    session_arg: &str,
    at: usize,
    inject: Option<String>,
    edit: Option<String>,
    replan: bool,
    prompt: Option<String>,
    flags: &AgentFlags,
) -> Result<()> {
    let _ = replan; // mode B is the default; the flag exists for explicitness.
    let events = Arc::new(open_event_store()?);
    let sid = if session_arg == "last" {
        events
            .latest_session()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .context("no recorded sessions in ~/.flux/events.db")?
    } else {
        events
            .info(session_arg)
            .with_context(|| format!("unknown session `{session_arg}`"))?;
        session_arg.to_string()
    };
    let src_info = events.info(&sid).map_err(|e| anyhow::anyhow!("{e}"))?;
    let last_input = events
        .turns(&sid)
        .ok()
        .and_then(|ts| ts.last().map(|t| t.user_input.clone()));

    // Mint the fork session, correlated to its source (the A-08 linkage `flux replay
    // --sub-agents` and cost rollups already understand), and seed its conversation with the
    // parent's messages so a re-planned tail has the recorded context.
    let fork_sid = events
        .create_session_with_context(
            &src_info.model,
            &flux_events::EventContext {
                correlation_id: Some(sid.clone()),
                agent_id: Some(format!("fork:{sid}@{at}")),
                ..Default::default()
            },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    for m in events
        .conversation(&sid)
        .map_err(|e| anyhow::anyhow!("{e}"))?
    {
        events
            .record_message(&fork_sid, &m)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    drop(events);

    let (engine, _session, model_spec, _spawner) =
        build_agent_lazy(flags, Some(fork_sid.clone())).await?;
    eprintln!(
        "{}",
        style::dim(&format!(
            "fork · {sid} @ statement {at} → {fork_sid} · prefix from tape, tail live"
        ))
    );
    let mut sink = CliSink::new(0).with_cost(model_spec, flux_credentials::load_pricing_table());

    let prefix = flux_flow::fork::replay_prefix(
        &engine.events,
        &engine.flow,
        &engine.executor,
        &sid,
        &fork_sid,
        at,
        &mut sink,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let outcome = if let Some(raw) = inject {
        let value: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("--inject is not valid JSON: {raw}"))?;
        Some(
            flux_flow::fork::diverge_inject(
                &engine.flow,
                &engine.executor,
                &fork_sid,
                &prefix,
                &value,
                &mut sink,
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        )
    } else if let Some(file) = edit {
        let src = std::fs::read_to_string(&file).with_context(|| format!("read {file}"))?;
        let ast: flux_flow::ast::DraftAst = if src.trim_start().starts_with('{') {
            serde_json::from_str(&src)
                .with_context(|| format!("parse {file} as a Flux-Lang DraftAst (JSON)"))?
        } else {
            match flux_lang::program::Module::parse_str(&src)
                .map_err(|e| anyhow::anyhow!("parse {file} as Flux-Lang text: {e}"))?
            {
                flux_lang::program::Module::Flow(ast) => ast,
                flux_lang::program::Module::Program(_) => {
                    bail!("--edit needs a bare flow, not a multi-agent program")
                }
            }
        };
        Some(
            flux_flow::fork::diverge_edit(
                &engine.flow,
                &engine.executor,
                &fork_sid,
                &prefix,
                &ast,
                &mut sink,
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        )
    } else {
        // Mode B: a live turn on the forked session — the planner sees the copied conversation
        // plus the replayed prefix's symbols, and plans a fresh tail through the full envelope.
        let instruction = prompt.unwrap_or_else(|| match &last_input {
            Some(input) => {
                format!("Continue from the current forked state. The original task was: {input}")
            }
            None => "Continue from the current forked state.".to_string(),
        });
        engine
            .run_turn(&fork_sid, &instruction, &mut sink)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        None
    };

    if let Some(out) = outcome {
        if let Some(halt) = out.failure {
            eprintln!("{}", style::dim(&format!("forked session: {fork_sid}")));
            bail!("fork tail halted: {}", halt.message);
        }
        if !out.result.is_empty() {
            println!("{}", out.result);
        }
    }
    println!(
        "forked session: {fork_sid}  (replay it with `flux replay {fork_sid}`; compare with \
         `flux diff {sid} {fork_sid}`)"
    );
    Ok(())
}

/// C-44: `flux diff <A> <B>` — align two recorded runs and pinpoint the divergence: the PLAN
/// changed (statement content differs) vs the same plan hit a DIFFERENT WORLD (recorded op
/// output differs). Pure read over the two run traces; statement hashes are re-humanized through
/// each session's stored `plan_source`. Exit 1 when the runs diverge, `diff`-style.
fn run_diff_cmd(a_arg: &str, b_arg: &str, json: bool) -> Result<()> {
    let events = Arc::new(open_event_store()?);
    let resolve = |arg: &str| -> Result<String> {
        if arg == "last" {
            events
                .latest_session()
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .context("no recorded sessions")
        } else {
            events
                .info(arg)
                .with_context(|| format!("unknown session `{arg}`"))?;
            Ok(arg.to_string())
        }
    };
    let (a, b) = (resolve(a_arg)?, resolve(b_arg)?);

    // Humanize statement hashes: every stored plan_source's top-level statements, formatted one
    // at a time, keyed by the SAME stmt_hash16 the trace rows carry.
    let mut texts: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for sid in [&a, &b] {
        for turn in events.turns(sid).map_err(|e| anyhow::anyhow!("{e}"))? {
            for att in turn.plan_attempts {
                let Some(src) = att.plan_source else { continue };
                let Ok(ast) = flux_lang::parse::parse(&src) else {
                    continue;
                };
                for node in &ast.body {
                    let h = flux_lang::runtime::stmt_hash16(node);
                    let one = flux_lang::format::format(&flux_flow::ast::DraftAst {
                        name: None,
                        params: vec![],
                        returns: None,
                        body: vec![node.clone()],
                    });
                    texts.insert(h, one.trim().replace('\n', " ⏎ "));
                }
            }
        }
    }
    let text = |stmt: &Option<String>| -> String {
        match stmt {
            Some(h) => texts.get(h).cloned().unwrap_or_else(|| format!("<{h}>")),
            None => "∅ (no statement at this position)".into(),
        }
    };
    let excerpt = |s: &str| -> String {
        let mut end = 96.min(s.len());
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        if end < s.len() {
            format!("{}…", &s[..end].replace('\n', " "))
        } else {
            s.replace('\n', " ")
        }
    };

    let diff = flux_events::run_diff(
        &events.run_trace(&a).map_err(|e| anyhow::anyhow!("{e}"))?,
        &events.run_trace(&b).map_err(|e| anyhow::anyhow!("{e}"))?,
    );

    if json {
        let rows: Vec<serde_json::Value> = diff
            .rows
            .iter()
            .map(|r| match r {
                flux_events::DiffRow::Same { node, stmt } => serde_json::json!({
                    "kind": "same", "node": node, "stmt": stmt,
                }),
                flux_events::DiffRow::Plan {
                    node,
                    a_stmt,
                    b_stmt,
                } => {
                    serde_json::json!({
                        "kind": "plan", "node": node, "a_stmt": a_stmt, "b_stmt": b_stmt,
                    })
                }
                flux_events::DiffRow::Output {
                    node,
                    stmt,
                    op,
                    a,
                    b,
                } => {
                    serde_json::json!({
                        "kind": "output", "node": node, "stmt": stmt, "op": op, "a": a, "b": b,
                    })
                }
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "a": a, "b": b, "identical": diff.identical, "rows": rows })
        );
    } else {
        println!("diff {a} ↔ {b}");
        for r in &diff.rows {
            match r {
                flux_events::DiffRow::Same { stmt, .. } => {
                    println!(
                        "{}",
                        style::dim(&format!("  = {}", text(&Some(stmt.clone()))))
                    );
                }
                flux_events::DiffRow::Plan { a_stmt, b_stmt, .. } => {
                    println!("  ~ plan diverges:");
                    println!("    - {}", text(a_stmt));
                    println!("    + {}", text(b_stmt));
                }
                flux_events::DiffRow::Output { stmt, op, a, b, .. } => {
                    println!(
                        "  ≠ same statement, different world — {}",
                        text(&Some(stmt.clone()))
                    );
                    println!("    op `{op}`:");
                    println!("    - {}", excerpt(a));
                    println!("    + {}", excerpt(b));
                }
            }
        }
        if diff.identical {
            println!("runs are identical ({} statement(s))", diff.rows.len());
        }
    }
    if !diff.identical {
        std::process::exit(1);
    }
    Ok(())
}

/// `flux corpus …` (D-53).
fn run_corpus(action: CorpusAction) -> Result<()> {
    match action {
        CorpusAction::Export { out } => run_corpus_export(out),
    }
}

/// The provenance anchor stamped on every exported row (D-53): the exporting binary's OWN
/// `CARGO_PKG_VERSION`, baked in at compile time — the same figure `flux --version` reports. This is
/// deliberately NOT a runtime `git describe`: an installed binary has no `.git` directory next to it
/// (and a caller's cwd is arbitrary), so a runtime git shell-out would be silently wrong or fail far
/// from where the plan was actually recorded. The crate version is the honest anchor actually
/// available wherever this binary runs — precise enough to tell flux-model whether a corpus row's
/// `plan_source` was recorded under a flux-lang grammar old enough to need re-lowering.
const FLUX_REV: &str = env!("CARGO_PKG_VERSION");

/// `flux corpus export [--out <file>]` — walk `~/.flux/events.db` and emit corpus-shaped JSONL: one
/// accepted plan's canonical text (`plan_source`, L-38) paired with its originating user instruction,
/// per line. Read-only; writes rows to stdout (or `--out`) and a skip-count summary to stderr, so the
/// data stream stays pipeable (`flux corpus export | wc -l`) while the audit trail is still visible.
fn run_corpus_export(out: Option<std::path::PathBuf>) -> Result<()> {
    let store = open_event_store()?;
    let summary = match &out {
        Some(path) => {
            let file = std::fs::File::create(path)
                .with_context(|| format!("create {}", path.display()))?;
            run_corpus_export_with(&store, FLUX_REV, file)?
        }
        None => run_corpus_export_with(&store, FLUX_REV, std::io::stdout())?,
    };
    eprintln!(
        "{} {} row{} exported · skipped {} (no plan_source {} · ambiguous pairing {} · unparseable at HEAD {})",
        style::bold("corpus export:"),
        summary.exported,
        if summary.exported == 1 { "" } else { "s" },
        summary.no_plan_source + summary.ambiguous_pairing + summary.unparseable_at_head,
        summary.no_plan_source,
        summary.ambiguous_pairing,
        summary.unparseable_at_head,
    );
    Ok(())
}

/// The `flux corpus export` outcome, printed to stderr — every row skipped (not just the exported
/// count) is visible, so "precision over recall" never reads as a silent undercount.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CorpusExportSummary {
    exported: u64,
    no_plan_source: u64,
    ambiguous_pairing: u64,
    /// A `plan_source` that failed to re-parse against the flux-lang parser LINKED INTO THIS BINARY
    /// — a scoped, in-repo stand-in for "lower_ok at current flux HEAD" (the fuller flux-model
    /// corpus ladder additionally lowers against a live op catalog + prior-turn symbol state, which
    /// is that repo's concern, not this exporter's). Expected to be 0 in practice: `plan_source` is
    /// documented as "present means parseable" at write time, so this only ever fires when the
    /// text grammar changed incompatibly since the plan was recorded.
    unparseable_at_head: u64,
}

/// The store-parameterized body of [`run_corpus_export`] (tests pass an in-memory store + an in-memory
/// writer so they touch neither `HOME`'s real `~/.flux/events.db` nor the filesystem).
fn run_corpus_export_with(
    store: &EventStore,
    flux_rev: &str,
    mut out: impl Write,
) -> Result<CorpusExportSummary> {
    let (rows, skips) = store.corpus_rows_all()?;
    let mut summary = CorpusExportSummary {
        no_plan_source: skips.no_plan_source,
        ambiguous_pairing: skips.ambiguous_pairing,
        ..Default::default()
    };
    // C-22 restated at the export boundary: `row.source` (plan_source) is already redacted with the
    // LIVE session redactor at record time (`loop_host.rs`'s `attempt.plan_source = redactor.redact(&src)`)
    // — nothing to redo here. `row.nl_goal` (the raw `TurnStarted.user_input`) is NOT redacted at
    // record time (only the agent's own outputs are), so it gets an equivalent scrub here: a bare
    // `Redactor` has no registered secret VALUES for a long-closed session to replay, but its
    // credential-SHAPED-token pattern match (`sk-…`, `ghp_…`, …) still fires independently of any
    // registry — the same class of scrub `capture.py` applies to raw corpus text.
    let redactor = flux_secret::Redactor::new();
    for row in rows {
        // Re-parse against the CURRENTLY LINKED flux-lang parser (Acceptance's "lower_ok at current
        // flux HEAD", scoped to parse validity — see CorpusExportSummary::unparseable_at_head).
        if flux_lang::parse::parse(&row.source).is_err() {
            summary.unparseable_at_head += 1;
            continue;
        }
        let line = serde_json::json!({
            "id": row.id,
            "nl_goal": redactor.redact(&row.nl_goal),
            "source": row.source,
            "provenance": { "session": row.session, "turn": row.turn },
            "flux_rev": flux_rev,
        });
        writeln!(out, "{}", serde_json::to_string(&line)?)?;
        summary.exported += 1;
    }
    Ok(summary)
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

/// The `flux-events`-backed [`EgressAudit`](flux_plugin::EgressAudit) impl: appends a
/// [`EventKind::PrivateNetAdmit`] to the session's stream whenever the plugin host admits a request
/// to a private/internal address under a scoped grant. This is the L6 binding of the L4 trait seam —
/// flux-plugin stays free of an event-store dependency. An append failure is logged, never fatal
/// (auditing must not break a live tool call).
struct EventStoreEgressAudit {
    store: Arc<EventStore>,
    stream: String,
}

impl flux_plugin::EgressAudit for EventStoreEgressAudit {
    fn record_private_admit(&self, caller: &str, host: &str, grant_source: &str) {
        let ev = flux_events::NewEvent::new(flux_events::EventKind::PrivateNetAdmit {
            caller: caller.to_string(),
            host: host.to_string(),
            grant_source: grant_source.to_string(),
        });
        if let Err(e) = self.store.append(&self.stream, ev) {
            eprintln!(
                "{}",
                style::dim(&format!("(audit: failed to record private-net admit: {e})"))
            );
        }
    }
}

/// L6 binding of the L5 [`flux_web::RecordSink`] seam: contributes the `web.page` records `web_fetch`
/// produces to the workspace datasource backend, so a fetched page is searchable afterwards. Errors
/// are swallowed — contribution is best-effort enrichment, never load-bearing for the fetch.
struct BackendRecordSink {
    backend: Arc<dyn flux_capabilities::DatasourceBackend>,
}

impl flux_web::RecordSink for BackendRecordSink {
    fn contribute(&self, records: &[flux_datasource::Record]) {
        let _ = self.backend.upsert(records);
    }
}

/// Seed `redactor` from the credential-bearing env vars: the provider keys
/// (`flux_credentials::provider_env_keys()` — the single source, covering the API-key providers and
/// the AWS secret material the Bedrock chain materializes into env) plus flux's own `FLUX_SECRET`.
/// Credential-shaped tokens are also caught by the redactor's heuristics; this makes the known ones
/// exact. The redactor shares its value store across clones, so seeding any clone seeds them all.
fn seed_provider_env_secrets(redactor: &flux_secret::Redactor) {
    let secret_refs: Vec<flux_secret::Ref> = flux_credentials::provider_env_keys()
        .iter()
        .chain(["FLUX_SECRET"].iter())
        .map(|k| flux_secret::Ref::env(*k))
        .collect();
    flux_runtime::SecretResolver::new().seed_redactor(&mut redactor.clone(), &secret_refs);
}

/// L6 binding of the L4 [`flux_plugin::SecretSink`] seam: registers a credential the host materialized
/// on the `credential` capability path with the executor's [`Redactor`](flux_secret::Redactor), so it
/// is scrubbed from any model-visible output. The redactor shares its value store across clones, so a
/// secret registered here is redacted by the clone the executor uses.
struct RedactorSecretSink {
    redactor: flux_secret::Redactor,
}

impl flux_plugin::SecretSink for RedactorSecretSink {
    fn register_secret(&self, value: &str) {
        self.redactor.add_secret(value);
    }
}

/// L6 binding of the L5 [`flux_capabilities::CrossPluginAudit`] seam: appends a `CrossPluginResolve`
/// event recording which consumer resolved which provider's credential, by *location* (the
/// `credential_ref` string) — never the value (D-27); and (D-30) an `EndpointDiscovered` event per
/// provider whose discovery returned candidates — count only, no URL, no secret. An append failure is
/// logged, never fatal.
struct EventStoreCrossPluginAudit {
    store: Arc<EventStore>,
    stream: String,
}

impl flux_capabilities::CrossPluginAudit for EventStoreCrossPluginAudit {
    fn record_cross_plugin_resolve(
        &self,
        consumer: &str,
        provider: &str,
        reference_location: &str,
    ) {
        let ev = flux_events::NewEvent::new(flux_events::EventKind::CrossPluginResolve {
            consumer: consumer.to_string(),
            provider: provider.to_string(),
            reference_location: reference_location.to_string(),
        });
        if let Err(e) = self.store.append(&self.stream, ev) {
            eprintln!(
                "{}",
                style::dim(&format!(
                    "(audit: failed to record cross-plugin resolve: {e})"
                ))
            );
        }
    }

    fn record_discovery(&self, product: &str, provider: &str, count: usize) {
        let ev = flux_events::NewEvent::new(flux_events::EventKind::EndpointDiscovered {
            product: product.to_string(),
            provider: provider.to_string(),
            count,
        });
        if let Err(e) = self.store.append(&self.stream, ev) {
            eprintln!(
                "{}",
                style::dim(&format!(
                    "(audit: failed to record endpoint discovery: {e})"
                ))
            );
        }
    }
}

/// Materialize the AWS credential chain into env from a **sync** context (C-11): `build_provider`
/// must stay sync (the sub-agent `Spawner` closure demands it), but the chain resolution (SSO/IRSA
/// HTTP) is async. Inside the CLI's multi-thread tokio runtime this hops through `block_in_place`;
/// with no runtime (plain sync callers, tests) it spins a one-shot current-thread runtime. A no-op
/// when `AWS_ACCESS_KEY_ID` is already set (static env / already materialized).
fn ensure_aws_chain() -> Result<()> {
    if std::env::var("AWS_ACCESS_KEY_ID")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Ok(());
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(flux_providers::bedrock::materialize_chain_into_env())
        })?,
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("aws chain: build runtime")?
            .block_on(flux_providers::bedrock::materialize_chain_into_env())?,
    }
    Ok(())
}

/// Build a fresh boxed provider for a model spec (used by the sub-agent factory).
fn provider_for(spec: &str) -> Result<Box<dyn Provider>> {
    if spec == "mock" || spec.starts_with("mock/") {
        Ok(Box::<MockCliProvider>::default())
    } else {
        let (native, _provider, _model) = build_provider(spec).map_err(|e| {
            anyhow::anyhow!(
            "sub-agent provider: {e} (hint: the parent --model spec is forwarded to sub-agents)"
        )
        })?;
        Ok(Box::new(native))
    }
}

/// A provider constructed on FIRST use (C-11). The deterministic execution paths (`flux flow run`,
/// `flux preset --run`) replay pre-authored plans that often contain no model op at all — demanding
/// a credential up front broke credential-less replay (CI boxes re-running a saved plan). The
/// construction error, when the flow DOES reach a model op, is the same one the eager path raises.
struct LazyProvider {
    spec: String,
    /// The provider prefix of `spec`, for `Provider::name` (a `&str` getter needs owned storage).
    display: String,
    cell: tokio::sync::OnceCell<(Box<dyn Provider>, String)>,
}

impl LazyProvider {
    fn new(spec: String) -> Self {
        let display = spec.split('/').next().unwrap_or("model").to_string();
        Self {
            spec,
            display,
            cell: tokio::sync::OnceCell::new(),
        }
    }
}

#[async_trait::async_trait]
impl Provider for LazyProvider {
    fn name(&self) -> &str {
        &self.display
    }

    async fn stream(
        &self,
        mut req: flux_provider::Request,
    ) -> flux_core::Result<flux_provider::ChunkStream> {
        let (provider, resolved_model) = self
            .cell
            .get_or_try_init(|| async {
                let (native, _provider, model) = build_provider(&self.spec)
                    .map_err(|e| flux_core::Error::Other(e.to_string()))?;
                Ok::<_, flux_core::Error>((Box::new(native) as Box<dyn Provider>, model))
            })
            .await?;
        // The engine carried the UNRESOLVED model spec (resolution normally happens at eager
        // construction) — swap in the resolved id for the wire.
        if req.model != *resolved_model {
            req.model = resolved_model.clone();
        }
        provider.stream(req).await
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
    // The strict-review reviewer roles ship in the binary (L-14) — `flux review` and the
    // `review_code` journey must work in ANY repo, not just one carrying `.flux/agents/review-*.md`
    // (a project's own files, loaded above, still win).
    for role in flux_app::review::builtin_review_roles() {
        if reg.get(&role.name).is_none() {
            reg.insert(role);
        }
    }
    reg
}

/// Read-only ops pre-allowed by default when no `[permissions].allow` is configured, so the common
/// case needs no config. `read`/`glob`/`grep`/`search` are the workspace reads; `now`/`cwd`/`home_dir`/
/// `sys_info` are zero-arg ambient reads (no IO, no permission subjects) that carry no approval-worthy
/// effect — gating them only adds friction (e.g. a `now()` in a stored flow would otherwise prompt, and
/// auto-deny on a non-TTY). A configured allow-list replaces this default entirely.
const DEFAULT_ALLOW: &[&str] = &[
    "read", "glob", "grep", "search", "now", "cwd", "home_dir", "sys_info",
];

/// Agentic mode: run a tool-enabled, policy-gated, session-persisted turn.
/// Build a tool-enabled agent (provider + safety envelope + session) for agentic mode / the REPL.
/// Eager provider construction: an agentic turn always calls the model, so a credential problem
/// should fail fast here. Deterministic execution paths use [`build_agent_lazy`].
async fn build_agent(
    flags: &AgentFlags,
) -> Result<(FlowEngine, String, String, Arc<dyn flux_runtime::Spawner>)> {
    build_agent_with(flags, true, None).await
}

/// [`build_agent`] with a LAZY provider (C-11): `flux flow run` / `flux preset --run` replay
/// pre-authored plans that may contain no model op — they must not demand a credential up front.
/// The provider constructs on the first actual model call (same error, surfaced only if needed).
/// `session_override`, when given (L-25's `flux flow run --resume`), is used as the run's session id
/// verbatim instead of minting a fresh one — so a corrected re-run lands in the SAME session whose
/// halt latch it is folding.
async fn build_agent_lazy(
    flags: &AgentFlags,
    session_override: Option<String>,
) -> Result<(FlowEngine, String, String, Arc<dyn flux_runtime::Spawner>)> {
    build_agent_with(flags, false, session_override).await
}

async fn build_agent_with(
    flags: &AgentFlags,
    eager_provider: bool,
    session_override: Option<String>,
) -> Result<(FlowEngine, String, String, Arc<dyn flux_runtime::Spawner>)> {
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
    let (provider, model, canonical_spec): (Box<dyn Provider>, String, String) =
        if model_spec == "mock" || model_spec.starts_with("mock/") {
            (
                Box::<MockCliProvider>::default(),
                "mock".to_string(),
                "mock".to_string(),
            )
        } else if !eager_provider {
            // C-11 lazy: no credential read, no chain resolution, no model-id resolution — all of
            // it happens inside `LazyProvider` on the first model call. The unresolved model part
            // serves for display; `LazyProvider` swaps the resolved id onto the wire.
            let display_model = model_spec
                .split_once('/')
                .map(|(_, m)| m.to_string())
                .unwrap_or_else(|| model_spec.clone());
            (
                Box::new(LazyProvider::new(model_spec.clone())),
                display_model,
                model_spec.clone(),
            )
        } else {
            // The one provider factory (C-11): `build_provider` owns the whole construction,
            // including the aws credential-chain materialization — no per-caller special cases.
            let (native, provider, m) = build_provider(&model_spec)?;
            // The canonical `provider/model` spec (resolved) — what cost/subscription detection
            // reads. The raw `model_spec` input may be a bare alias (`codex`, `sonnet`) that neither
            // `is_subscription` nor `rates_for` can decode, so surface the resolved form.
            let canonical_spec = format!("{provider}/{m}");
            (Box::new(native), m, canonical_spec)
        };

    let mut workspace = Workspace::from_env(&cwd).context("workspace")?;
    if let Some(home) = std::env::var_os("HOME") {
        let flux_dir = std::path::PathBuf::from(home).join(".flux");
        // Global roots for agent-reusable definitions: `~/.flux/flows` is the home for flows +
        // composite ops (discovered by `flow_list`, run by `flow_run`, ops auto-loaded); `~/.flux/ops`
        // is the legacy location, still read during the ops→flows unification.
        for (name, sub) in [("global_flows", "flows"), ("global_ops", "ops")] {
            let dir = flux_dir.join(sub);
            std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
            workspace
                .add_named_root(name, &dir)
                .with_context(|| format!("register {}", dir.display()))?;
        }
    }
    let system = Arc::new(System::new(workspace));

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
    // ONE shared identity cell backs the top-level executor AND the sub-agent spawner: a
    // per-request surface (server principal mode, D-69) swaps it between turns and children
    // spawned afterwards inherit the request principal, never a stale build-time identity.
    let identity = flux_runtime::IdentityCell::new(caller, trust);

    // The unified event store, opened BEFORE the sub-agent spawner (A-08: child runs audit into
    // this same store by default) and before plugins (the egress-audit hook appends
    // `PrivateNetAdmit` events to this stream).
    let events = Arc::new(open_event_store()?);

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
    // below. Sub-agents inherit the same authorization floor as the top-level agent, and audit into
    // the shared event store by default (A-08) — each child gets its own correlated session stream.
    let spawner: Arc<dyn flux_runtime::Spawner> =
        SubAgents::new(roles, child_base, factory, model.clone(), flags.max_tokens)
            .with_authorization_cell(policy.clone(), identity.clone())
            .with_audit(events.clone())
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

    // Root/reflexive ops: `plan`/`run_plan` are registered so a pre-authored flow (`flux flow run`, and
    // the agent loop in flux-lang) can call them, but are tagged to the never-surfaced `reflect` group so
    // they stay OUT of the model-facing catalog in ordinary turns. `op.register` is model-facing and
    // delegates to the engine-installed composite registrar.
    flux_tools::register_reflect(&mut registry);

    // Flow discovery/run: `flow_list` (enumerate .flux/flows + ~/.flux/flows) and `flow_run`
    // (run a stored flow by name in the current session). Model-facing, so the agent can
    // discover and run authored flows.
    flux_tools::register_flows(&mut registry);

    // Auto-index workspace docs (markdown/text, capped & cheap) into the knowledge datasource, and
    // register the retrieval ops (`search`/`get`/`list`/`relation`/`batch_get`). The backend is also
    // the sink `web_fetch` contributes `web.page` records to (below), so read pages are groundable.
    let backend = build_doc_index(&system).await;
    flux_capabilities::register_datasource_ops(&mut registry, backend.clone());

    // This run's session on the store opened above. `session_override` (L-25's `flow run --resume`)
    // wins outright — it names an already-halted session to continue, distinct from the REPL's own
    // `--continue`/`--resume` (latest session) semantics.
    let session_id = if let Some(id) = session_override {
        id
    } else if flags.continue_ || flags.resume {
        events
            .latest_session()
            .context("latest session")?
            .ok_or_else(|| anyhow::anyhow!("no session to resume"))?
    } else {
        events.create_session(&model).context("create session")?
    };

    // Seed the secret redactor from known credential env vars so their values are scrubbed from
    // tool output and logs. (Credential-shaped tokens are also caught by the redactor's heuristics.)
    // Built BEFORE the plugin block so the `credential`-capability secret sink can register
    // host-materialized credentials with the SAME redactor the executor later redacts with — the
    // redactor shares its value store across clones, so a credential resolved mid-run is scrubbed.
    let redactor = flux_secret::Redactor::new();
    seed_provider_env_secrets(&redactor);

    // Native web capabilities (flux-web): `http.request` (tier 1), `web_fetch` + `html_to_markdown`
    // (tier 2), all under the family-wide `[private_net] web` egress scope. Registered here — after
    // the session is resolved — because the `PrivateNetAdmit` audit sink needs the event store +
    // session id, and `web_fetch` contributes `web.page` records to the datasource backend.
    {
        let web_audit: Arc<dyn flux_plugin::EgressAudit> = Arc::new(EventStoreEgressAudit {
            store: events.clone(),
            stream: session_id.clone(),
        });
        flux_web::register_web(
            &mut registry,
            &flux_web::WebOptions {
                private_net: flux_system::net::PrivateNetAllow::from_hosts(
                    effective_web_private_hosts(&cfg),
                ),
                audit: Some(web_audit),
                grant_source: Some(web_grant_source()),
                records: Some(Arc::new(BackendRecordSink {
                    backend: backend.clone(),
                })),
                browser_bin: cfg.browser_bin.clone(),
            },
        );
    }

    // Discover subprocess plugins (~/.flux/plugins/*.toml) and project their operations as tools.
    // Each plugin's host capabilities are the guarded System (same boundary as built-in tools).
    let mut plugin_groups: Vec<flux_evidence::ToolGroup> = Vec::new();
    if let Some(dir) = plugins_dir() {
        // The cross-plugin endpoint-discovery broker (D-26/D-27): a registry of loaded plugins + the
        // shared endpoint registry, so a consumer plugin's `endpoint.discover` capability fans out to
        // providers, and (D-27) the broker is the host-side `ReferenceResolver` for ref-based IO +
        // gated cross-plugin credential resolution.
        let plugin_registry = Arc::new(flux_capabilities::PluginRegistry::new());
        let endpoint_registry = Arc::new(flux_capabilities::EndpointRegistry::with_path(
            flux_capabilities::EndpointRegistry::default_path().unwrap_or_default(),
        ));
        let _ = endpoint_registry.load();
        let invoker = Arc::new(flux_capabilities::HostProviderInvoker::new(
            plugin_registry.clone(),
        ));
        // The static config resolver (named endpoints + Env credentials) is the first link of the
        // broker's resolver chain. (No host config endpoint bindings are wired yet — an empty map
        // resolves named refs to "not bound"; discovered `@endpoint/*` refs resolve from the registry.)
        let static_resolver = Arc::new(flux_capabilities::StaticResolver::new(
            system.clone(),
            std::collections::HashMap::new(),
        ));
        // Cross-plugin credential audit (D-27): records consumer→provider resolutions by LOCATION.
        let xplugin_audit: Arc<dyn flux_capabilities::CrossPluginAudit> =
            Arc::new(EventStoreCrossPluginAudit {
                store: events.clone(),
                stream: session_id.clone(),
            });
        // NOTE(D-27): no interactive `CrossPluginApprover` (a modal/stdin first-use prompt) is wired
        // here — deliberate, not a gap: the seam exists on the broker, but running headless, the
        // operator config grant alone authorizes. An interactive approver is a filed-separately
        // follow-up if wanted (flux D-65 leaves this posture unchanged on both the `build_agent` and
        // `flux app run` paths).
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
            .with_cross_plugin_audit(xplugin_audit),
        );
        // Agent-facing endpoint ops (D-28): `endpoint.discover` fans out through the broker;
        // `endpoint.list`/`info`/`select` read the shared endpoint registry. Surfaced by the
        // `kubernetes` signal (the `endpoint` group). Registered regardless of which plugins load.
        flux_capabilities::register_endpoint_ops(
            &mut registry,
            broker.clone(),
            endpoint_registry.clone(),
        );
        for p in flux_plugin::discover(&dir) {
            // Build host capabilities from the plugin's own manifest declaration, so each plugin
            // gets only the process/secret/http access it asked for (and nothing by default).
            let system = system.clone();
            let cfg_for_caps = cfg.clone();
            let caps_system = system.clone();
            let audit: Arc<dyn flux_plugin::EgressAudit> = Arc::new(EventStoreEgressAudit {
                store: events.clone(),
                stream: session_id.clone(),
            });
            let broker_for_caps = broker.clone();
            let resolver_for_caps = broker.clone() as Arc<dyn flux_plugin::ReferenceResolver>;
            let secret_sink = Arc::new(RedactorSecretSink {
                redactor: redactor.clone(),
            }) as Arc<dyn flux_plugin::SecretSink>;
            let make_caps = move |m: &flux_plugin::PluginManifest| {
                let plugin_private_hosts = effective_plugin_private_hosts(&cfg_for_caps, &m.name);
                // Inject the broker as the resolver (ref-based IO + the `credential` capability) and the
                // redactor-backed secret sink BEFORE wrapping with the broker host-caps.
                let inner = Arc::new(
                    flux_plugin::SystemHostCaps::new(caps_system)
                        .with_manifest(m)
                        .with_private_net_grants(plugin_private_hosts)
                        .with_grant_source(private_net_grant_source_for(&m.name))
                        .with_egress_audit(audit)
                        .with_resolver(resolver_for_caps)
                        .with_secret_sink(secret_sink),
                ) as Arc<dyn flux_plugin::HostCapabilities>;
                // Wrap with the endpoint broker so this plugin's `endpoint.discover` calls fan out
                // (deny-by-default, gated by the manifest's `discover` capability).
                Arc::new(flux_capabilities::EndpointBrokerHostCaps::new(
                    inner,
                    broker_for_caps,
                    m.name.clone(),
                    m.capabilities.discover,
                )) as Arc<dyn flux_plugin::HostCapabilities>
            };
            match flux_plugin::load_plugin_tools(
                &system,
                &p.descriptor.program,
                &p.descriptor.args,
                make_caps,
            )
            .await
            {
                Ok(lp) => {
                    // Register this plugin as a discovery provider so the broker can fan a query back
                    // to it (matched by its manifest's `discovers` products).
                    plugin_registry.register(
                        lp.manifest.name.clone(),
                        flux_capabilities::ProviderEntry {
                            manifest: Arc::new(lp.manifest.clone()),
                            host: lp.host.clone(),
                            caps: lp.caps.clone(),
                        },
                    );
                    plugin_groups.extend(lp.manifest.groups.clone());
                    // The registered tools hold the host alive for the session.
                    for t in lp.tools {
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
    // mutating tools still gate. See [`DEFAULT_ALLOW`]. A configured allow-list replaces it entirely.
    let mut allow = cfg.permissions.allow.clone();
    if allow.is_empty() {
        allow.extend(DEFAULT_ALLOW.iter().map(|s| s.to_string()));
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

    let ctx = ToolContext::new(system)
        .with_spawner(spawner.clone())
        .with_redactor(redactor);
    let executor = Executor::new(registry, perms, approver, ctx)
        .with_hooks(hook_vec)
        .with_policy(policy)
        .with_identity_cell(identity);
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
    groups.push(flux_web::browser_group());
    groups.extend(plugin_groups);
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

    let flow = open_flow_store(events.clone())?;
    // Assemble the engine: this installs the reflexive loop host on the executor and loads the flux-lang
    // `agent-loop.flux` (the turn loop is flux-lang, not Rust).
    let spec = AgentSpec {
        model,
        system_prompt,
        skills: load_skills(&cwd, &cfg, &flags.skill_dirs),
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
    // Per-turn token ceiling (A-10), default OFF. Precedence: --turn-budget > FLUX_TURN_TOKEN_BUDGET
    // > config [limits] turn_token_budget.
    let turn_budget = flags
        .turn_budget
        .or_else(|| {
            std::env::var("FLUX_TURN_TOKEN_BUDGET")
                .ok()
                .and_then(|v| v.trim().parse().ok())
        })
        .or(cfg.limits.turn_token_budget);
    agent.loop_host.set_token_budget(turn_budget);
    // Read-only-round breadth ladder (A-29): config-only overrides of the built-in defaults —
    // raise (or 0-disable) for legitimately read-heavy workflows.
    agent.loop_host.set_readonly_ladder(
        cfg.limits.readonly_rounds_escalate,
        cfg.limits.readonly_rounds_stop,
    );
    Ok((agent, session_id, canonical_spec, spawner))
}

/// One-shot agentic turn.
async fn run_agentic(flags: &AgentFlags, prompt: String) -> Result<()> {
    let (agent, session_id, model_spec, _spawner) = build_agent(flags).await?;
    eprintln!(
        "{}",
        style::dim(&format!("{} · session {session_id}", agent.model))
    );
    let initial_rules = agent.executor.allow_rules();
    let pricing = flux_credentials::load_pricing_table();
    let mut sink = CliSink::new(agent.max_iterations).with_cost(model_spec, pricing);
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

/// Build the `SubAgents` bundle for the strict-review protocol's reviewer fan-out: the same
/// `load_roles` + `SubAgents::new` construction `build_agent` uses for the top-level agent, shared by
/// both `flux review` ([`run_review`]) and `flux app run strict-review` (the built-in-program branch
/// of [`run_app`]) so the two call sites can't drift.
fn build_review_sub_agents(
    cwd: &std::path::Path,
    model_spec: &str,
    model: impl Into<String>,
    max_tokens: u32,
) -> SubAgents {
    let roles = load_roles(cwd);
    let mut child_base = ToolRegistry::new();
    flux_tools::register_builtins(&mut child_base);
    let factory: ProviderFactory = {
        let spec = model_spec.to_string();
        Arc::new(move || provider_for(&spec).map_err(|e| flux_core::Error::Other(e.to_string())))
    };
    SubAgents::new(roles, child_base, factory, model, max_tokens)
}

/// `flux review --files <path>… [--format md|json] [--fail-on <severity>]` — run the strict-review
/// protocol (flux L-13; `docs/designs/strict-review-flows.md` "Phase 4") over `files` and print the
/// resulting `ReviewReport`. Runs the SAME embedded `strict_review` flow text
/// (`flux_app::review::STRICT_REVIEW_FLOW_SRC` — the checked-in `examples/strict_review.flux`, the
/// identical source the `review_code` app journey wraps as a composite op) through
/// `flux_sdk::FlowClient::run_flow` — the deterministic `parse` → `analyze` → `execute_with` path, no
/// model round-trip for the flow itself (only the reviewer sub-agents call a model). Self-contained:
/// [`load_roles`] already falls back to built-in role definitions when a project's own
/// `.flux/agents/review-*.md` is absent, and the flow text ships in the binary — so this works in any
/// repo. Read-only: `strict_review`'s reviewer roles all declare `tools: []`, and this command never
/// writes anywhere but stdout.
async fn run_review(
    flags: &AgentFlags,
    files: Vec<String>,
    format: ReviewFormat,
    fail_on: Option<ReviewSeverity>,
) -> Result<()> {
    let cwd = std::env::current_dir().context("current dir")?;
    let cfg = flux_config::load(&cwd).context("load .flux/config.toml")?;
    let model_spec = resolve_model_spec(&flags.model, &cfg);

    let (provider, model): (Arc<dyn Provider>, String) =
        if model_spec == "mock" || model_spec.starts_with("mock/") {
            (Arc::new(MockCliProvider::default()), "mock".to_string())
        } else {
            let (native, _provider_name, m) = build_provider(&model_spec)?;
            (Arc::new(native), m)
        };

    // Wire roles + sub-agents exactly like `build_agent`: `strict_review`'s bounded 3-role reviewer
    // fan-out (via `task`) delegates through the identical envelope the top-level agent uses.
    let sub_agents = build_review_sub_agents(&cwd, &model_spec, model.clone(), flags.max_tokens);

    // `strict_review`'s core is read-only by construction (git_status/git_diff/read_many + `task`
    // against `tools: []` reviewer roles — see the design's security considerations); auto-approving
    // this specific, fixed flow's own ops is not the same authority `--yes` grants an arbitrary
    // prompt-compiled plan, so it does not consult `flags.yes`.
    let mut client = flux_sdk::FlowClient::builder()
        .model(model)
        .auto_approve(true)
        .build(provider, cwd)
        .context("build flow client")?;
    client.with_sub_agents(sub_agents);

    let mut inputs = serde_json::Map::new();
    inputs.insert("files".to_string(), serde_json::json!(files));

    let out = client
        .run_flow(flux_app::review::STRICT_REVIEW_FLOW_SRC, inputs)
        .await
        .map_err(|e| anyhow::anyhow!("strict_review: {e}"))?;
    let report: flux_tools::cognition::ReviewReport = serde_json::from_str(&out.result)
        .with_context(|| {
            format!(
                "strict_review did not return a ReviewReport: {}",
                out.result
            )
        })?;

    match format {
        ReviewFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).context("serialize ReviewReport")?
            );
        }
        ReviewFormat::Md => println!("{}", render_review_markdown(&report)),
    }

    if should_fail(&report, fail_on) {
        std::process::exit(1);
    }
    Ok(())
}

/// Render a [`flux_tools::cognition::ReviewReport`] as a readable markdown findings summary — the
/// default `flux review` output mode.
fn render_review_markdown(report: &flux_tools::cognition::ReviewReport) -> String {
    let mut out = String::new();
    out.push_str("# Strict review\n\n");
    out.push_str(&format!("{}\n\n", report.summary));
    out.push_str(&format!(
        "Checked {} file(s) · reviewers: {}\n\n",
        report.checked_files.len(),
        report.reviewers.join(", ")
    ));
    if report.findings.is_empty() {
        out.push_str("No findings.\n");
    } else {
        out.push_str("## Findings\n\n");
        for f in &report.findings {
            out.push_str(&format!(
                "### [{}] {} ({})\n\n",
                f.severity.to_uppercase(),
                f.title,
                f.category
            ));
            if let Some(file) = &f.file {
                match f.line {
                    Some(line) => out.push_str(&format!("- **location:** `{file}:{line}`\n")),
                    None => out.push_str(&format!("- **location:** `{file}`\n")),
                }
            }
            out.push_str(&format!(
                "- **reviewer:** {} (agreement: {})\n",
                f.reviewer, f.agreement
            ));
            out.push_str(&format!("- **confidence:** {:.2}\n", f.confidence));
            if !f.evidence.is_empty() {
                out.push_str(&format!("- **evidence:** {}\n", f.evidence));
            }
            if !f.recommendation.is_empty() {
                out.push_str(&format!("- **recommendation:** {}\n", f.recommendation));
            }
            out.push('\n');
        }
    }
    if !report.gaps.is_empty() {
        out.push_str("## Gaps\n\n");
        for gap in &report.gaps {
            out.push_str(&format!("- {gap}\n"));
        }
    }
    out
}

/// The exit-code decision, factored out as a pure function so it is unit-testable without going
/// through `std::process::exit`: `true` iff `threshold` is set AND at least one finding's severity is
/// at or above it. `None` (no `--fail-on`) never fails, regardless of findings.
fn should_fail(
    report: &flux_tools::cognition::ReviewReport,
    threshold: Option<ReviewSeverity>,
) -> bool {
    let Some(threshold) = threshold else {
        return false;
    };
    report
        .findings
        .iter()
        .any(|f| ReviewSeverity::from_finding_str(&f.severity) >= threshold)
}

async fn run_flow(
    file: &str,
    model: Option<String>,
    yes: bool,
    resumable: bool,
    resume: Option<String>,
    resume_value: Option<String>,
) -> Result<()> {
    // Build the agent flags from the command's own model/`--yes` (reuses the shared agent wiring).
    let flags = AgentFlags::from_model_yes(model.as_deref(), yes);

    let src = std::fs::read_to_string(file).with_context(|| format!("read flow {file}"))?;
    // A behavioral loop file is native flux-lang text, or a checked-in JSON `DraftAst` (sniffed by the
    // leading `{`). Both load as the same AST.
    let (ast, composites): (
        flux_flow::ast::DraftAst,
        Vec<flux_lang::program::CompositeOpDecl>,
    ) = if src.trim_start().starts_with('{') {
        (
            serde_json::from_str(&src)
                .with_context(|| format!("parse {file} as a Flux-Lang DraftAst (JSON)"))?,
            Vec::new(),
        )
    } else {
        match flux_lang::program::Module::parse_str(&src)
            .map_err(|e| anyhow::anyhow!("parse {file} as Flux-Lang text: {e}"))?
        {
            flux_lang::program::Module::Flow(ast) => (ast, Vec::new()),
            flux_lang::program::Module::Program(program) => {
                let ast = match (program.flows.as_slice(), program.journeys.as_slice()) {
                    ([flow], []) => flow.clone(),
                    ([], [journey]) => journey.flow.clone(),
                    _ => bail!(
                        "`flux flow run` needs a bare flow or a module with exactly one flow/journey"
                    ),
                };
                (ast, program.ops)
            }
        }
    };

    run_draft_ast_with_composites_resumable(
        &flags,
        &ast,
        &composites,
        resumable,
        resume,
        resume_value,
    )
    .await
}

/// Execute a pre-built `DraftAst` through the full envelope — the shared core behind both
/// `flux flow run <file.flux>` and `flux preset <name> --run`. Builds the agent, validates the flow
/// against the live op registry, previews risk + installs the per-op approver, runs it, and prints the
/// outcome. The only inputs are the agent flags (model/`--yes`) and the AST itself.
pub(crate) async fn run_draft_ast(
    flags: &AgentFlags,
    ast: &flux_flow::ast::DraftAst,
) -> Result<()> {
    run_draft_ast_with_composites(flags, ast, &[]).await
}

pub(crate) async fn run_draft_ast_with_composites(
    flags: &AgentFlags,
    ast: &flux_flow::ast::DraftAst,
    composites: &[flux_lang::program::CompositeOpDecl],
) -> Result<()> {
    run_draft_ast_with_composites_resumable(flags, ast, composites, false, None, None).await
}

/// [`run_draft_ast_with_composites`] plus L-25's opt-in resumable mode for `flux flow run`.
/// `resumable` alone reifies a halting top-level statement (a failure, or the L-24 `Awaiting`
/// reified pause) as a printed, structured halt report + non-zero exit instead of erroring the
/// whole run (design `multipass-agent-loop.md`'s "L-25: pre-authored resumable mode"); `resume`
/// additionally targets a PRIOR halted session (a literal id, or `last`) and folds its statement
/// ledger before executing, so a corrected re-run fast-forwards the matching completed prefix.
/// `resume` implies resumable execution even when `--resumable` was not also passed. `flux preset
/// --run` and every other caller of [`run_draft_ast_with_composites`] pass `false, None` here and
/// keep today's exact strict (non-resumable) behavior — this is additive, not a mode switch.
pub(crate) async fn run_draft_ast_with_composites_resumable(
    flags: &AgentFlags,
    ast: &flux_flow::ast::DraftAst,
    composites: &[flux_lang::program::CompositeOpDecl],
    resumable: bool,
    resume: Option<String>,
    resume_value: Option<String>,
) -> Result<()> {
    let resumable = resumable || resume.is_some();

    // L-25: `--resume` targets a specific, ALREADY-halted session instead of minting a fresh one.
    // Resolved against throwaway store handles before `build_agent_lazy` opens its own — SQLite/WAL
    // supports the sequential opens, and this avoids wasting a session record or mis-tagging plugin
    // audit streams the way overriding `session_id` after construction would.
    let resume_session = match &resume {
        Some(arg) => {
            let events = Arc::new(open_event_store()?);
            let flow = open_flow_store(events.clone())?;
            Some(resolve_resume_session(&events, &flow, ast, arg)?)
        }
        None => None,
    };

    // Lazy provider (C-11): a pre-authored flow is deterministic unless it actually reaches a
    // model op — replaying one must not demand credentials.
    let (engine, session_id, model_spec, _spawner) =
        build_agent_lazy(flags, resume_session).await?;
    eprintln!(
        "{}",
        style::dim(&format!("flow · {} · session {session_id}", engine.model))
    );
    // C-43: authored flow runs record the cassette too (the engine arms it per agent turn; this
    // path executes directly, so it arms its own) — and persist the executed plan as an accepted
    // `plan_source` attempt (this path has no loop host to record it), so `flux flow run`
    // results are replayable with `flux replay` exactly like agent turns. Off with
    // FLUX_CASSETTE=0.
    if flux_flow::cassette::enabled() {
        engine
            .flow
            .set_cassette(Some(Arc::new(flux_flow::cassette::CassetteScope::Record(
                flux_flow::cassette::RecordScope::new(engine.events.clone(), &session_id),
            ))));
        if let Ok(turn_id) = engine
            .events
            .begin_turn(&session_id, "<flow run>", &engine.model)
        {
            let source = flux_lang::format::format(ast);
            let redactor = &engine.executor.context().redactor;
            let _ = engine.events.record_plan_attempt(
                &session_id,
                turn_id,
                flux_events::PlanAttempt {
                    step: 1,
                    outcome: "accepted".into(),
                    error: None,
                    fingerprint: Some(flux_lang::runtime::sha256_hex(
                        &serde_json::to_string(ast).unwrap_or_default(),
                    )),
                    plan_text: None,
                    phase: None,
                    plan_source: Some(redactor.redact(&source)),
                    delta_source: None,
                },
            );
        }
    }
    engine
        .composites
        .ensure_session_loaded(&engine.flow, &session_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut active_composites = engine.composites.active_for_session(&session_id);
    active_composites.extend(composites.iter().cloned());

    // Validate against the live op registry before running anything.
    if let Err(diags) =
        flux_flow::registry::analyze_composites(&active_composites, engine.executor.registry())
    {
        print_diagnostics(&diags);
        bail!("composite validation failed — see diagnostics above");
    }
    let oreg = flux_flow::registry::OpRegistry::new(engine.executor.registry())
        .with_composites(&active_composites);
    // Typed gate (L-16/F9): full structural analysis + lowering, with the session's already-bound
    // symbols satisfying definedness (a resumed session may legitimately reference prior turns).
    let session_symbols: std::collections::HashSet<String> = engine
        .flow
        .view(&session_id)
        .map(|v| v.symbols.into_iter().map(|s| s.name.0).collect())
        .unwrap_or_default();
    if let Err(diags) = flux_flow::analyze::lower(ast, &oreg, &session_symbols) {
        print_diagnostics(&diags);
        bail!("flow validation failed — see diagnostics above");
    }

    // L-25: fold the session's open halt latch. `None` for a fresh `--resumable`-only run (a
    // brand-new session never halted before); on `--resume`, the ledger to fast-forward against.
    // A resume target MUST have an open halt — silently continuing on a stale/typo'd session id
    // would hide a mistake rather than fail loudly.
    let open_halt = if resumable {
        engine.flow.open_halted_plan(&session_id)?
    } else {
        None
    };
    if resume.is_some() && open_halt.is_none() {
        bail!("session {session_id} has no open halt to resume — nothing to fast-forward");
    }

    // A-58 / F-015: a resume that lands on a value-awaiting `await` (`$reply = await …`) must supply
    // its payload. The resumable driver fast-forwards *past* the await, so bind `--resume-value` into
    // the awaited symbol first — otherwise post-await statements die on `unbound symbol`. When a
    // value-await gets no payload, refuse clearly (naming the symbol) instead of advancing into that
    // failure. A bare `await` binds nothing, so it needs no value.
    if let Some(open) = &open_halt {
        let awaited = flux_lang::runtime::awaited_binding(&ast.body, open.halt.node);
        match (&resume_value, awaited) {
            (Some(raw), _) => {
                // Parse as JSON so `42`/`true`/`"x"`/`{…}` keep their type; a bare word is a string.
                let value = serde_json::from_str::<serde_json::Value>(raw.trim())
                    .unwrap_or_else(|_| serde_json::Value::String(raw.clone()));
                let bound = flux_lang::runtime::bind_resume_value(
                    engine.flow.as_ref(),
                    &session_id,
                    &ast.body,
                    open.halt.node,
                    value,
                )
                .map_err(|e| anyhow::anyhow!("bind resume value: {e}"))?;
                if let Some(sym) = bound {
                    eprintln!("{}", style::dim(&format!("resume: bound ${sym} = {raw}")));
                }
            }
            (None, Some(sym)) => {
                bail!(
                    "session {session_id} halted awaiting a value for `${sym}` — pass \
                     --resume-value <json> (e.g. --resume-value '\"hello\"', --resume-value 42)"
                );
            }
            (None, None) => {}
        }
    }

    // Denial re-emission guard (design Part 2 / A-16): a statement policy or the user already
    // refused must never be silently re-dispatched just because it re-appears unchanged in a
    // corrected re-emission. Checked BEFORE executing anything — this authored path never goes
    // through `run_plan`, so it must enforce the same invariant itself.
    if let Some(open) = &open_halt {
        if flux_flow::runtime::denied_reemission_guard(&ast.body, &open.halt) {
            eprintln!(
                "{}",
                style::red(&flux_flow::runtime::render_halt_report(
                    ast,
                    &open.halt,
                    &session_id
                ))
            );
            eprintln!(
                "{}",
                style::dim(
                    "the statement previously refused is unchanged in this file — it was NOT \
                     re-run. Edit it to a different approach, or have an operator re-approve."
                )
            );
            std::process::exit(1);
        }
    }

    // Risk preview (informational; every op still gates at dispatch through the engine's approver,
    // which `build_agent` set from `--yes`). Scoped to the whole plan even when resuming — dispatch
    // itself never re-runs the skipped prefix, so this stays a harmless over-approval preview.
    let risk = if active_composites.is_empty() {
        flux_flow::runtime::plan_risk(ast, engine.executor.registry())
    } else {
        flux_flow::runtime::plan_risk_with_composites(
            ast,
            engine.executor.registry(),
            &active_composites,
        )
    };
    eprintln!(
        "\n{}  {}{}",
        style::bold("flow"),
        risk_badge(&risk.summary()),
        style::dim(&format!(" · {} op(s)", risk.ops.len()))
    );

    // Point the engine's installed loop host at this run's session + sink (a flow may call
    // `plan`/`run_plan`, which re-enter the planner/interpreter through this same executor). The sink is
    // shared so the outer flow and any inner `run_plan` stream live onto one surface, sub-steps interleaved.
    let shared: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
        CliSink::new(0).with_cost(model_spec, flux_credentials::load_pricing_table()),
    ));
    // `None` advertised set: this is the pre-authored `flow run` path, which is deliberately
    // unrestricted by surfacing (the file names its ops explicitly; only model-emitted plans gate).
    engine.loop_host.set_turn(
        session_id.clone(),
        Some(engine.system_prompt.clone()),
        shared.clone(),
        None,
        None,
    );

    let mut sink = flux_flow::loop_host::SharedSink::new(shared.clone());
    let outcome = if resumable {
        // L-25: the SAME resumable entry point `run_plan` uses (`docs/designs/multipass-agent-loop.md`
        // Part 2) — a failing top-level statement reifies onto `outcome.failure` instead of
        // propagating `Err`; `open_halt`'s ledger (when resuming) fast-forwards the matching prefix.
        flux_flow::runtime::execute_flow_resumable_with_composites(
            engine.flow.as_ref(),
            engine.executor.as_ref(),
            &session_id,
            ast,
            &active_composites,
            open_halt.as_ref().map(|o| &o.ledger),
            None,
            &mut sink,
        )
        .await
    } else {
        // Also the no-composites case (empty slice is equivalent): this entry point self-wires
        // the C-43 cassette scope from the store — plain `execute_flow` deliberately does not
        // (it is shared with the outer agent loop, whose machinery is never cassetted).
        flux_flow::runtime::execute_flow_with_composites(
            engine.flow.as_ref(),
            engine.executor.as_ref(),
            &session_id,
            ast,
            &active_composites,
            &mut sink,
        )
        .await
    }
    .context("execute flow")?;

    // A reified halt (L-25): print the structured report and exit non-zero instead of the normal
    // success printing below — the caller corrects the file and re-runs with `--resume`.
    if let Some(halt) = &outcome.failure {
        eprintln!(
            "{}",
            flux_flow::runtime::render_halt_report(ast, halt, &session_id)
        );
        let u = engine.loop_host.turn_usage();
        shared
            .lock()
            .unwrap()
            .turn_end((u.total() > 0).then_some(u));
        std::process::exit(1);
    }

    if !outcome.result.trim().is_empty() {
        println!("{}", outcome.result);
    } else {
        // Always surface a closing summary so a direct flow turn never ends silently.
        eprintln!(
            "{}",
            style::dim(&format!("done \u{00b7} {} step(s)", outcome.steps))
        );
    }
    // A deterministic flow bills nothing (usage stays zero → `None`, today's output); a flow that
    // reached a model op via `plan`/`run_plan` reports its real spend (C-30).
    let u = engine.loop_host.turn_usage();
    shared
        .lock()
        .unwrap()
        .turn_end((u.total() > 0).then_some(u));
    Ok(())
}

/// Resolve `flux flow run <file> --resume <arg>` to a concrete session id (L-25). A literal id is
/// used as-is (the caller finds out soon enough — via [`FlowStore::open_halted_plan`] returning
/// `None` — if it names a session with no open halt). `last` searches the workspace's session store
/// (newest-first) for the most recent session with an open halt latch whose halted plan's key is
/// prefixed by this flow's declared name (the same `name#`/`h:` prefix
/// [`flow_key`](flux_lang::runtime) derives) — an UNNAMED flow can't be disambiguated this way (a
/// bare `h:<hash>` prefix could match ANY unnamed halted plan, including an ordinary chat turn's
/// inner `run_plan` halt, since they share the same session store and ledger machinery), so `last`
/// is refused for it and the caller is pointed at the explicit session id the halt report printed.
fn resolve_resume_session(
    events: &EventStore,
    flow: &FlowStore,
    ast: &flux_flow::ast::DraftAst,
    arg: &str,
) -> Result<String> {
    if arg != "last" {
        return Ok(arg.to_string());
    }
    let name = ast
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "`--resume last` needs the flow to declare a name (`flow <name> -> …`) to find its \
                 halted session unambiguously — pass the explicit session id the halt report \
                 printed instead"
            )
        })?;
    let prefix = format!("{name}#");
    const SEARCH_LIMIT: usize = 500;
    for s in events.list(SEARCH_LIMIT).context("list sessions")? {
        if let Some(open) = flow
            .open_halted_plan(&s.id)
            .with_context(|| format!("open halted plan for session {}", s.id))?
        {
            if open.halt.plan.starts_with(&prefix) {
                return Ok(s.id);
            }
        }
    }
    bail!("no halted `flow run` session found for flow `{name}` — nothing to resume");
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
    let (engine, session_id, model_spec, _spawner) = build_agent(&flags).await?;
    let cli_ask = CliAsk;
    eprintln!(
        "{}",
        style::dim(&format!("plan · {} · agentic", engine.model))
    );

    // A-42: a live sink so any auto-run gather rounds stream (ops/results, phase-aware spinner)
    // instead of the prior silence — no cost/turn-end rendering needed here, `compile_once` never
    // calls `turn_end` on it (this is a compile, not a turn).
    let mut gather_sink = CliSink::new(0);
    let compiled = match engine
        .compile_once(
            &session_id,
            &prompt,
            &mut gather_sink,
            terminal_ask(&cli_ask),
        )
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
        let refusal = if diagnostics_all_unknown_op(&compiled.diagnostics) {
            "plan references unknown operations — not running"
        } else {
            "plan failed validation — not running"
        };
        eprintln!("{}", style::yellow(refusal));
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
    let mut sink = CliSink::new(0).with_cost(model_spec, flux_credentials::load_pricing_table());
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

/// Whether *every* analyzer diagnostic is an unknown-op error (message shape `unknown operation: …`).
/// Picks an accurate header: a validation failure of another class (bad arg, arity, type/shape,
/// composability, unbound symbol, …) must not be filed under "references unknown operations" (A-62 /
/// F-010) — that header misleads both the reader and the planner, which reads diagnostics back to
/// repair. Empty ⇒ false (no header is printed for an empty set).
fn diagnostics_all_unknown_op(diags: &[flux_flow::analyze::Diagnostic]) -> bool {
    !diags.is_empty()
        && diags
            .iter()
            .all(|d| d.message.starts_with("unknown operation"))
}

/// Print analyzer diagnostics to stderr, if any, under a header matching their actual failure class.
fn print_diagnostics(diags: &[flux_flow::analyze::Diagnostic]) {
    if diags.is_empty() {
        return;
    }
    let header = if diagnostics_all_unknown_op(diags) {
        "diagnostics — the plan references unknown operations"
    } else {
        "diagnostics — the plan failed validation"
    };
    eprintln!("{}", style::yellow(header));
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
/// Per-turn cost wiring for every REPL/CLI sink (C-30): one pricing table loaded per command; the
/// model spec is derived from the LIVE engine at each sink construction — the same derivation
/// `loop_host` uses to key stored usage (C-15) — so what the turn line prices and what
/// `flux usage` attributes can never diverge, and a `/model` switch is picked up with zero extra
/// plumbing (the switch arm updates `agent.provider`/`agent.model`, which is all we read).
struct TurnCost {
    pricing: flux_core::PricingTable,
}

impl TurnCost {
    fn load() -> Self {
        Self {
            pricing: flux_credentials::load_pricing_table(),
        }
    }

    /// The canonical `provider/model` spec of the engine's CURRENT provider + model.
    fn spec(agent: &FlowEngine) -> String {
        flux_core::canonical_model_spec(Some(agent.provider.name()), &agent.model)
    }

    /// A cost-attached [`CliSink`] for one turn on `agent`.
    fn sink(&self, agent: &FlowEngine, max_iter: usize) -> CliSink {
        CliSink::new(max_iter).with_cost(Self::spec(agent), self.pricing.clone())
    }
}

async fn run_repl(flags: AgentFlags) -> Result<()> {
    let (mut agent, mut session_id, _spec, spawner) = build_agent(&flags).await?;
    let cost = TurnCost::load();
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
                        let cost_ref = &cost;
                        let sid_ref = session_id.as_str();
                        run_interruptible(move |c| async move {
                            run_pending_plan(agent_ref, cost_ref, sid_ref, &ast, &c).await;
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
                            Ok((native, _provider, model)) => {
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
                            run_goal(&agent, &cost, &session_id, spawner.as_ref(), &cond, c)
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
                        run_interruptible(|c| run_loop(&agent, &cost, &session_id, n, &task, c))
                            .await;
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
                    let mut sink = cost.sink(&agent, 0);
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
            let cost_ref = &cost;
            let sid_ref = session_id.as_str();
            let mut new_plan: Option<flux_flow::ast::DraftAst> = None;
            let plan_slot = &mut new_plan;
            run_interruptible(move |c| async move {
                let mut sink = cost_ref.sink(agent_ref, 0);
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
        let cost_ref = &cost;
        let sid_ref = session_id.as_str();
        run_interruptible(move |c| async move {
            let mut sink = cost_ref.sink(agent_ref, agent_ref.max_iterations);
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
    cost: &TurnCost,
    session_id: &str,
    ast: &flux_flow::ast::DraftAst,
    cancel: &tokio_util::sync::CancellationToken,
) {
    // The human reviewed the rendered plan (tree + risk badge) in `/plan` mode, so the disclosure
    // follows what that preview showed: a destructive op the user saw doesn't re-prompt per-op,
    // while a destructive command assembled at runtime (invisible to the preview) still does.
    let composites = agent.composites.active_for_session(session_id);
    let risk =
        flux_flow::runtime::plan_risk_with_composites(ast, agent.executor.registry(), &composites);
    let _scope = agent.executor.enter_approved_scope(risk.destructive);
    // Scope the loop host to THIS run (C-30): a plan may call `plan`/`run_plan`, which re-enter
    // through the same executor — without `set_turn` they'd stream onto the STALE prior turn's
    // ctx — and scoping is also what lets `turn_usage()` report this run's real model spend
    // (billed only when the plan reaches a model op) instead of `turn_end(None)` forever.
    let shared: Arc<std::sync::Mutex<dyn AgentSink>> =
        Arc::new(std::sync::Mutex::new(cost.sink(agent, 0)));
    agent.loop_host.set_turn(
        session_id.to_string(),
        Some(agent.system_prompt.clone()),
        shared.clone(),
        None,
        None,
    );
    let mut sink = flux_flow::loop_host::SharedSink::new(shared.clone());
    // Race execution against `cancel`: `execute_flow` has no cancellation of its own, so Ctrl-C is
    // honored by dropping the in-flight flow future (which aborts the current op's IO). The future
    // borrows `sink`, so scope it in a block and read its result out as owned data; `None` => cancelled.
    let result: Option<Result<String>> = {
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
            res = &mut fut => Some(res.map(|o| o.result).map_err(|e| anyhow::anyhow!("{e:#}"))),
        }
    };
    let end_with_usage = || {
        let u = agent.loop_host.turn_usage();
        shared
            .lock()
            .unwrap()
            .turn_end((u.total() > 0).then_some(u));
    };
    match result {
        Some(Ok(out)) => {
            if !out.trim().is_empty() {
                println!("{out}");
            }
            end_with_usage();
        }
        Some(Err(e)) => eprintln!("{} {e}", style::red("error:")),
        None => {
            // Cancelled: stop the in-flight op's spinner and return to the prompt.
            end_with_usage();
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
    cost: &TurnCost,
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
        let mut sink = GoalSink {
            cost: Some((TurnCost::spec(agent), cost.pricing.clone())),
            ..Default::default()
        };
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
                flux_runtime::SpawnRequest::new(
                    "evaluator",
                    format!(
                        "Goal: {goal}\n\nLatest result:\n{}\n\nReply `SATISFIED` or `CONTINUE: <next>`.",
                        sink.text
                    ),
                ),
                &cancel,
            )
            .await
        {
            Ok(v) => v.text,
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
    cost: &TurnCost,
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
        let mut sink = cost.sink(agent, 0);
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
    /// The resolved model spec (e.g. `codex/gpt-5.5`) + pricing table for the per-turn cost
    /// annotation. `None` when the sink wasn't given a spec (sub-paths that don't show cost).
    model_spec: Option<String>,
    pricing: Option<flux_core::PricingTable>,
    /// The phase of the most recent `loop.phase` observation this turn (design Part 1 / A-15):
    /// `orient`/`gather`/`execute`, or `None` for a phase-less caller (the `/plan` REPL path,
    /// which doesn't emit `loop.phase` — A-18 brings gather there later). Drives the spinner label
    /// via `phase_spinner_label`.
    phase: Option<String>,
    /// How many `execute`-phase `loop.phase` observations have landed this turn — the first is the
    /// turn's actual execution planning, every one after it means the prior round didn't finish
    /// (a revision), so the spinner reads "revising…" once this exceeds 1. A plain counter over
    /// observations already reaching the sink; no new flux-flow signal needed.
    execute_rounds: usize,
    /// Whether the NEXT `flow.plan` observation is a bounded, read-only gather round rather than
    /// the full execution plan — set on a `gather`-phase `loop.phase` or a `flow.brief` (a brief
    /// only ever accompanies a `gather: true` plan), cleared on `orient`/`execute`. `flow.plan`
    /// itself carries no `gather` flag (that lives on `Compiled`/the host, not the observation), so
    /// this is the cheapest surface-side derivation available without new flux-flow plumbing.
    gather_mode: bool,
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
            model_spec: None,
            pricing: None,
            phase: None,
            execute_rounds: 0,
            gather_mode: false,
        }
    }

    /// Attach a model spec + pricing table so the per-turn annotation appends a dollar cost. The
    /// spec is the full `provider/model` (e.g. `codex/gpt-5.5`) so subscription spend is detected
    /// from the provider prefix; the table is the loaded overlay-on-builtin (`load_pricing_table`).
    fn with_cost(mut self, model_spec: String, pricing: flux_core::PricingTable) -> Self {
        self.model_spec = Some(model_spec);
        self.pricing = Some(pricing);
        self
    }

    /// The per-turn dollar-cost suffix for the annotation, when a model spec + pricing table are
    /// attached and the turn reported usage — see [`cost_suffix`] for the full rendering rules
    /// (incl. the C-30 `$? (unpriced)` marker for un-tabled metered cloud models).
    fn cost_inline(&self, usage: Option<&Usage>) -> String {
        cost_suffix(self.model_spec.as_deref(), self.pricing.as_ref(), usage)
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
        // Fill the otherwise-silent compile wait with a spinner; the compiled plan tree (or the
        // compact gather one-liner) replaces it (via the `flow.plan` observation) once the planner
        // is done. The label is phase-aware (A-15): "orienting…"/"gathering…"/"planning…"/
        // "revising…" — see `phase_spinner_label`.
        if active {
            self.commit();
            if self.use_spinner() {
                self.start_spinner(style::dim(&phase_spinner_label(
                    self.phase.as_deref(),
                    self.execute_rounds,
                )));
            }
        } else {
            self.stop_spinner();
        }
    }
    /// L-23: a plan-skeleton headline for one top-level statement, the instant its `emit_plan`
    /// JSON arguments finish streaming — while composing a large plan takes a while, the running
    /// spinner already started by `planning(true)` shows the tree taking shape node by node
    /// instead of sitting on a bare "planning…" until the whole call completes. The eventual
    /// `flow.plan` observation (`render_plan`) replaces this with the full, authoritative tree.
    fn plan_delta(&mut self, headline: &str) {
        let label = style::dim(&format!(
            "{} · {headline}",
            phase_spinner_label(self.phase.as_deref(), self.execute_rounds)
        ));
        if let Some((state, _)) = &self.spinner {
            state.lock().unwrap().label = label;
        } else if self.stderr_tty {
            // No animated spinner (styling disabled / non-interactive stderr) — still show
            // progress as plain dim lines rather than going silent.
            eprintln!("{}", style::dim(&format!("· {headline}")));
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
        } else if o.kind == "context.shrunk" {
            // A-63 / F-011: a context pack dropped members to fit its budget — surface it once so a
            // plain run shows the eviction (the model-facing transcript line alone never did).
            let dropped = o.data.get("dropped").and_then(|v| v.as_u64()).unwrap_or(0);
            let total = o.data.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
            eprintln!(
                "{}",
                style::dim(&format!("⊙ context: dropped {dropped} of {total} members"))
            );
        } else if o.kind == "turn.cancelled" {
            eprintln!("{}", style::dim("⊘ turn cancelled"));
        } else if o.kind == "loop.phase" {
            self.record_phase(o);
        } else if o.kind == "loop.round" {
            // A-39 (`--trace-loop`/`FLUX_TRACE_LOOP`): one dim line per outer-loop round.
            let round = o.data.get("round").and_then(|v| v.as_u64()).unwrap_or(0);
            let max = o.data.get("max").and_then(|v| v.as_u64()).unwrap_or(0);
            eprintln!("{}", style::dim(&format!("⟳ round {round}/{max}")));
        } else if o.kind == "loop.node" {
            // A-39: one dim line per structural AST node the outer loop executes.
            eprintln!("{}", style::dim(&trace_node_line(&o.data)));
        } else if o.kind == "flow.brief" {
            // A brief only ever accompanies a `gather: true` plan (`compile.rs`'s `parse_brief`
            // call site) — its arrival marks gather mode even when the phase alone (`orient`) is
            // ambiguous between a gather round and a full plan emitted directly.
            self.gather_mode = true;
            self.render_brief(o);
        } else if o.kind == "flow.plan" {
            // A-17 (closes the A-15 residual): `flow.plan` now carries its own `gather` flag,
            // computed host-side from the plan's own `settled` signal — prefer it directly over the
            // surface's `loop.phase`/`flow.brief`-order inference, which couldn't tell an
            // orient-phase gather plan apart from orient emitting the full plan directly when the
            // model's `brief` was unusable. Falls back to the tracked state for a phase-less caller
            // that predates the field (e.g. a stale override still on the pre-A-17 wire shape).
            let gather = o
                .data
                .get("gather")
                .and_then(|v| v.as_bool())
                .unwrap_or(self.gather_mode);
            if gather {
                self.render_gather_compact(o);
            } else {
                self.render_plan(o);
            }
        } else if o.kind == "flow.halt" {
            self.render_halt(o);
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
        // The dollar cost of this turn's tokens, when a model spec + pricing table were attached.
        let cost_inline = self.cost_inline(usage.as_ref());
        // Always print a rule so the turn boundary is visible even for prose-only replies.
        let summary = if self.steps > 0 {
            let plural = if self.steps == 1 { "" } else { "s" };
            format!(
                "{} step{plural} · {elapsed}{token_inline}{cost_inline}",
                self.steps
            )
        } else {
            // Prose-only turn: a minimal rule with elapsed + token stats.
            format!("· {elapsed}{token_inline}{cost_inline}")
        };
        let rule_len = self.width.saturating_sub(summary.chars().count() + 2);
        eprintln!("{} {}", style::rule(rule_len), style::dim(&summary));
    }
}

/// The compact token annotation appended to a turn-end rule (and the prose `/goal` footer): the
/// context-window occupancy (the final prompt size), the tokens generated, cache tiers (read AND
/// write — C-06 added the write side, which used to be silently dropped), and reasoning tokens when
/// the provider reported any. Cost itself is a separate suffix ([`cost_annotation`], appended by the
/// caller via `CliSink::cost_inline`) — this function is only the token breakdown. Empty when nothing
/// was billed (e.g. an offline `-m mock` turn).
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
    if u.cache_creation_input_tokens > 0 {
        s.push_str(&format!(
            " · cache write {}",
            style::fmt_tokens(u.cache_creation_input_tokens)
        ));
    }
    if u.reasoning_tokens > 0 {
        s.push_str(&format!(
            " · reasoning {}",
            style::fmt_tokens(u.reasoning_tokens)
        ));
    }
    s
}

/// The dollar-cost suffix for the turn-end annotation. Subscription spend (claude/codex) is shown
/// as an *equivalent metered cost* prefixed with `~` and tagged `(sub)` — it bills against a flat
/// subscription, not the API, so the figure is illustrative, not a charge. Metered spend shows the
/// raw dollar amount. Returns an empty string for a zero-cost turn (e.g. a cached/no-op call).
fn cost_annotation(money: &flux_core::Money) -> String {
    if money.usd <= 0.0 {
        return String::new();
    }
    let usd = format!("${:.4}", money.usd);
    if money.subscription {
        format!(" · ~{usd} (sub)")
    } else {
        format!(" · {usd}")
    }
}

/// The complete turn-line cost suffix (shared by every sink): the dollar amount when the table
/// prices the spec; the C-30 ` · $? (unpriced)` marker when a **metered cloud** model has no
/// pricing row (real dollars are being spent invisibly — the marker says so and the once-per-run
/// note points at the `~/.flux/pricing.toml` override); empty when usage/spec/table are absent,
/// when the priced cost is zero, or for local/unknown specs (`ollama*`, `mock`, ad-hoc providers —
/// nothing is billed, and hermetic e2e output must stay byte-identical).
fn cost_suffix(
    spec: Option<&str>,
    table: Option<&flux_core::PricingTable>,
    usage: Option<&Usage>,
) -> String {
    let (Some(u), Some(spec), Some(table)) = (usage, spec, table) else {
        return String::new();
    };
    match table.cost(u, spec) {
        Some(money) => cost_annotation(&money),
        None if unpriced_marker_applies(spec) => {
            note_unpriced_once(spec);
            " · $? (unpriced)".to_string()
        }
        None => String::new(),
    }
}

/// The `$?` marker fires only for known metered **cloud** providers — a table miss there hides
/// real spend. Local `ollama*` and unknown/mock providers stay silent. Thin delegate onto
/// `flux_core::is_metered_cloud_spec` (C-33) — the TUI's header uses the same predicate, so the
/// rule has one definition.
fn unpriced_marker_applies(spec: &str) -> bool {
    flux_core::is_metered_cloud_spec(spec)
}

/// One-time (per process) plain-stderr hint explaining the `$?` marker and how to price the model.
fn note_unpriced_once(spec: &str) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "note: no pricing entry for `{spec}` — add one to ~/.flux/pricing.toml to see $ costs"
        );
    });
}

impl CliSink {
    /// Render a `flow.plan` observation: the syntax-highlighted plan tree + a risk badge header. A
    /// resumed/halted plan (`resumed: true`, A-17) carries per-statement ✓/✗/· status markers in its
    /// `plan` text instead of full syntax highlighting — patch-and-continue's granularity is
    /// top-level statements only — so that text is rendered (marker-colored) directly rather than
    /// reconstructing a fresh, unmarked tree from `plan_ast`.
    fn render_plan(&self, o: &flux_evidence::Observation) {
        let resumed = o
            .data
            .get("resumed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let rendered = if resumed {
            o.data
                .get("plan")
                .and_then(|v| v.as_str())
                .map(style_marked_plan)
        } else {
            o.data
                .get("plan_ast")
                .and_then(|v| serde_json::from_value::<flux_flow::ast::DraftAst>(v.clone()).ok())
                .map(|ast| flux_flow::render::render_styled(&ast, &style::plan_palette()))
                .or_else(|| {
                    o.data
                        .get("plan")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
        };
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

    /// Render a `flow.halt` observation (A-17): a red one-liner marking exactly where a plan halted,
    /// printed the moment the halt happens — before the next `plan()` round's spinner (which then
    /// reads "revising…", see `phase_spinner_label`) — so the failure is legible in real time, not
    /// only inside the fed-back transcript text.
    fn render_halt(&self, o: &flux_evidence::Observation) {
        eprintln!("{}", style::red(&halt_line(&o.data)));
    }

    /// Track a `loop.phase` observation (design Part 1 / A-15, emitted at every `plan()` entry):
    /// updates the spinner label state and whether the round's `flow.plan` (rendered a moment
    /// later, from a separate `run_plan` reflexive call) is a compact gather round or the full
    /// execution plan. `gather`/`execute` are unambiguous; `orient` resets to "not gathering yet" —
    /// a `flow.brief` right after (only ever paired with a `gather: true` plan) flips it back on
    /// when orient itself emitted the first gather round.
    fn record_phase(&mut self, o: &flux_evidence::Observation) {
        let phase = o
            .data
            .get("phase")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        match phase.as_str() {
            "execute" => {
                self.execute_rounds += 1;
                self.gather_mode = false;
            }
            "gather" => self.gather_mode = true,
            "orient" => self.gather_mode = false,
            _ => {}
        }
        self.phase = Some(phase);
    }

    /// Render a `flow.brief` observation the moment the grounding artifact is accepted (design
    /// Part 1's "feedback within seconds"): `◆ goal: …` plus a dim `needs: …` line when present.
    fn render_brief(&self, o: &flux_evidence::Observation) {
        let mut lines = brief_lines(&o.data).into_iter();
        if let Some(goal_line) = lines.next() {
            eprintln!("{}", style::cyan(&goal_line));
        }
        for line in lines {
            eprintln!("{}", style::dim(&line));
        }
    }

    /// Render a gather-plan `flow.plan` observation as a compact one-liner (op names, not the full
    /// tree + risk badge a full execution plan gets — those are for the small, read-only,
    /// approval-free collect rounds design Part 1 bounds to ~12 call nodes).
    fn render_gather_compact(&self, o: &flux_evidence::Observation) {
        eprintln!("{}", style::dim(&gather_compact_line(&o.data)));
    }
}

/// The planning spinner's label (A-15): phase-derived so it reads "orienting…"/"gathering…" for
/// the collect passes and "planning…" for the execute pass's first round. "revising…" only once
/// the execute phase has already produced a round THIS turn — a plain counter over the
/// `loop.phase` observations already reaching the sink, not a new flux-flow signal. The halt-aware
/// "✗ step N/M — revising…" line is a separate, real-time render (`render_halt`/`halt_line`, A-17)
/// fired the moment a plan halts, distinct from this spinner label. A phase-less caller (the
/// `/plan` REPL path, which doesn't emit `loop.phase`) falls back to today's "composing plan…".
fn phase_spinner_label(phase: Option<&str>, execute_rounds: usize) -> String {
    match phase {
        Some("orient") => "orienting…".to_string(),
        Some("gather") => "gathering…".to_string(),
        Some("execute") => {
            if execute_rounds > 1 {
                "revising…".to_string()
            } else {
                "planning…".to_string()
            }
        }
        _ => "composing plan…".to_string(),
    }
}

/// Format a `flow.brief` observation's `data` as plain lines (no color, so it's directly testable):
/// `◆ goal: …` then, when present, a `needs: …` list line.
fn brief_lines(data: &Value) -> Vec<String> {
    let goal = data.get("goal").and_then(|v| v.as_str()).unwrap_or("");
    let mut lines = vec![format!("◆ goal: {goal}")];
    if let Some(needs) = data.get("needs").and_then(|v| v.as_array()) {
        let items: Vec<&str> = needs.iter().filter_map(|v| v.as_str()).collect();
        if !items.is_empty() {
            lines.push(format!("  needs: {}", items.join(", ")));
        }
    }
    lines
}

/// Format a `flow.halt` observation's `data` (A-17) as a plain line: `✗ step N/M <op> failed —
/// revising…` — or, when the op isn't directly derivable from the failing statement (a composite/
/// control-flow node), `✗ step N/M failed — revising…`. Emitted once per mid-plan halt, right where
/// the rest of the feedback contract is built (`EngineLoopHost::run_plan`'s halt arm) — a real-time
/// cue distinct from the per-tool ✓/✗ markers the dispatcher already prints as ops run.
fn halt_line(data: &Value) -> String {
    let step = data.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
    let of = data.get("of").and_then(|v| v.as_u64()).unwrap_or(0);
    match data.get("op").and_then(|v| v.as_str()) {
        Some(op) => format!("✗ step {step}/{of} {op} failed — revising…"),
        None => format!("✗ step {step}/{of} failed — revising…"),
    }
}

/// Format a `loop.node` observation's `data` (A-39, `--trace-loop`/`FLUX_TRACE_LOOP`) as a plain,
/// colorless line — one per structural AST node the outer agent loop executes. Falls back to the
/// raw JSON for any `node` kind this hasn't been taught (defensive: the interpreter's trace helper
/// is meant to grow new emission sites without this formatter going stale/panicking).
fn trace_node_line(data: &Value) -> String {
    let label = |key: &str| data.get(key).and_then(|v| v.as_str());
    match data.get("node").and_then(|v| v.as_str()) {
        Some("call") => {
            let op = label("op").unwrap_or("?");
            match label("bind") {
                Some(bind) => format!("· {op} → ${bind}"),
                None => format!("· {op}"),
            }
        }
        Some("when") => {
            let branch = label("branch").unwrap_or("?");
            match label("cond") {
                Some(cond) => format!("· when {cond} → {branch}"),
                None => format!("· when → {branch}"),
            }
        }
        Some("unless") => {
            let entered = data
                .get("entered")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let word = if entered { "enter" } else { "skip" };
            match label("cond") {
                Some(cond) => format!("· unless {cond} → {word}"),
                None => format!("· unless → {word}"),
            }
        }
        Some("match") => {
            let value = label("value").unwrap_or("");
            let arm = label("arm").unwrap_or("?");
            match label("subject") {
                Some(subject) => format!("· match {subject} = {value} → {arm}"),
                None => format!("· match {value} → {arm}"),
            }
        }
        Some("return") => match label("value") {
            Some(v) => format!("· return {v}"),
            None => "· return".to_string(),
        },
        Some("repeat") => {
            let rounds = data.get("rounds").and_then(|v| v.as_u64()).unwrap_or(0);
            let max = data.get("max").and_then(|v| v.as_u64()).unwrap_or(0);
            format!("· until hit — exit after {rounds}/{max}")
        }
        Some("parallel.branch") => {
            let name = label("name").unwrap_or("?");
            format!("· parallel branch ${name}")
        }
        _ => format!("· {data}"),
    }
}

/// Color each line of a marker-prefixed plan render (A-17): `✓` done lines green, `✗` the failed
/// statement red, `·` not-yet-run lines dim — the per-statement status text a resumed/halted plan's
/// `flow.plan` observation carries (`render_marked_plan` in `flux-flow`) instead of a fresh full
/// tree. Any line that doesn't start with one of those three markers passes through unstyled.
fn style_marked_plan(text: &str) -> String {
    text.lines()
        .map(|line| match line.chars().next() {
            Some('✓') => style::green(line),
            Some('✗') => style::red(line),
            Some('·') => style::dim(line),
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format a gather-plan `flow.plan` observation's `data` as a compact one-liner: `gathering ·
/// <op> <arg> · <op> <arg> …`, pulling call nodes off `plan_ast` and reusing the same
/// `format_call` the tool-call stream uses (so `read Cargo.toml`/`grep "needle"` etc. read
/// identically to a real op line). Falls back to a bare op count when the AST can't be walked.
fn gather_compact_line(data: &Value) -> String {
    const ARG_CAP: usize = 60;
    let calls = data
        .get("plan_ast")
        .and_then(|v| serde_json::from_value::<flux_flow::ast::DraftAst>(v.clone()).ok())
        .map(|ast| {
            let mut out = Vec::new();
            for n in &ast.body {
                collect_plan_calls(n, &mut out);
            }
            out
        })
        .unwrap_or_default();
    let summary = if calls.is_empty() {
        let ops = data.get("ops").and_then(|v| v.as_u64()).unwrap_or(0);
        let plural = if ops == 1 { "" } else { "s" };
        format!("{ops} op{plural}")
    } else {
        calls
            .iter()
            .map(|(op, input)| {
                let call = flux_tui::toolview::format_call(op, input);
                if call.arg.is_empty() {
                    call.verb
                } else {
                    format!("{} {}", call.verb, truncate(&call.arg, ARG_CAP))
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    };
    format!("gathering · {summary}")
}

/// Walk a gather plan's top-level shape (a `Call`, a `$x = Call(...)` bind, or a `seq` of either)
/// collecting each call's op name + its input (the single literal-object argument a tool call
/// carries, when the plan author wrote one plainly — a computed/templated argument falls back to
/// an empty input, which `format_call` renders as just the bare verb).
fn collect_plan_calls(node: &flux_flow::ast::Node, out: &mut Vec<(String, Value)>) {
    use flux_flow::ast::Node;
    match node {
        Node::Call { op, args } => {
            let input = args
                .first()
                .and_then(|a| match a {
                    Node::Lit { value } => Some(value.clone()),
                    _ => None,
                })
                .unwrap_or(Value::Null);
            out.push((op.clone(), input));
        }
        Node::Bind { value, .. } => collect_plan_calls(value, out),
        Node::Seq { body, .. } => body.iter().for_each(|n| collect_plan_calls(n, out)),
        _ => {}
    }
}

/// Like [`CliSink`] but also accumulates the assistant text (so `/goal`'s evaluator can read it).
#[derive(Default)]
struct GoalSink {
    text: String,
    /// `(model spec, pricing table)` for the per-turn cost suffix (C-30); `None` in tests.
    cost: Option<(String, flux_core::PricingTable)>,
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
            // Same figures as the main rule (tokens + C-30 cost suffix), without the leading separator.
            let (spec, table) = match &self.cost {
                Some((s, t)) => (Some(s.as_str()), Some(t)),
                None => (None, None),
            };
            let stats = format!(
                "{}{}",
                usage_annotation(&u),
                cost_suffix(spec, table, Some(&u))
            );
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
            // `write` takes its parameters as a single named object (positional args are rejected
            // by plan validation for multi-param ops) — pass one `lit` object, not two positionals.
            serde_json::json!({
                "body": [{
                    "kind": "call", "op": "write",
                    "args": [
                        { "kind": "lit", "value": {
                            "path": "flux-mock.txt",
                            "content": "created by flux mock\n"
                        } }
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
    async fn request_plan(&self, plan: &flux_runtime::PlanApprovalRequest) -> ApprovalChoice {
        let prompt = format!(
            "\n{} this plan? ({})  [y]es / [a]lways / [N]o: ",
            style::yellow("run"),
            plan.subject(),
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

/// Export the C-21 filesystem-access policy to `FLUX_ADD_DIRS` / `FLUX_ALLOW_ALL` from the CLI flags +
/// `[workspace]` config, so `Workspace::from_env` (used at every production construction site) picks it
/// up. Sources are **additive**: `--add-dir` flags, `[workspace] add_dirs`, and any pre-set `FLUX_ADD_DIRS`
/// all contribute; `--allow-all-paths`, `[workspace] allow_all`, or `FLUX_ALLOW_ALL` each enable the hatch.
fn apply_workspace_access_env(cli: &Cli) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = flux_config::load(&cwd).unwrap_or_default();
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

    let allow_all = cli.allow_all_paths
        || cfg.workspace_allow_all()
        || std::env::var("FLUX_ALLOW_ALL")
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
    if allow_all {
        std::env::set_var("FLUX_ALLOW_ALL", "1");
        eprintln!(
            "{} filesystem sandbox disabled (--allow-all-paths): the agent can read AND write anywhere \
             on disk",
            style::red("warning:")
        );
    }

    // Ephemeral private-network egress grant for this invocation (D-96). Exported so surfaces that do
    // not receive the `Cli` (e.g. `flux plugin call`, `app run`) observe the same override.
    if cli.allow_private_net {
        std::env::set_var("FLUX_ALLOW_PRIVATE_NET", "1");
        eprintln!(
            "{} private-network egress allowed for this run (--allow-private-net): plugins may reach \
             the private hosts their manifest declares, and web_fetch may reach any private/loopback \
             address (incl. cloud metadata). Prefer a scoped [private_net.plugins] grant for recurring use.",
            style::red("warning:")
        );
    }
}

/// Whether `--allow-private-net` is in effect for this process. It is propagated as
/// `FLUX_ALLOW_PRIVATE_NET` by [`apply_workspace_access_env`], so surfaces that never receive the
/// [`Cli`] (notably `flux plugin call`) observe it too.
fn private_net_cli_override() -> bool {
    std::env::var_os("FLUX_ALLOW_PRIVATE_NET").is_some()
}

/// The per-plugin private-net host grant, widened to `*` when `--allow-private-net` is active. This
/// only widens the *operator grant* side; `SystemHostCaps::private_net_allow` still intersects it with
/// the plugin's manifest-declared `private_hosts`, so a plugin declaring none stays refused — the
/// deny-by-default envelope (D-20) is preserved, this is just an ephemeral grant equivalent to config.
fn effective_plugin_private_hosts(cfg: &flux_config::Config, name: &str) -> Vec<String> {
    if private_net_cli_override() {
        vec!["*".to_string()]
    } else {
        cfg.plugin_private_hosts(name)
    }
}

/// The family-wide `web`-scope private-net host grant (native `flux-web` ops: `http.request`,
/// `web_fetch`, `browser.*`), widened to `*` when `--allow-private-net` is active.
fn effective_web_private_hosts(cfg: &flux_config::Config) -> Vec<String> {
    if private_net_cli_override() {
        vec!["*".to_string()]
    } else {
        cfg.web_private_hosts()
    }
}

/// The `grant_source` recorded in a native-web `PrivateNetAdmit` audit: the CLI-flag label when
/// `--allow-private-net` is active, else the `web`-scope config source.
fn web_grant_source() -> String {
    if private_net_cli_override() {
        "cli:--allow-private-net".to_string()
    } else {
        "config:web".to_string()
    }
}

/// The `grant_source` recorded in the `PrivateNetAdmit` audit for a plugin caller: the CLI-flag label
/// when `--allow-private-net` is active, else the normal per-plugin config source (`config:plugin/<name>`,
/// matching [`SystemHostCaps::with_manifest`]'s default).
fn private_net_grant_source_for(name: &str) -> String {
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
fn reset_sigpipe() {
    // SAFETY: setting a signal disposition to SIG_DFL is a process-global libc call with no data race,
    // and SIG_DFL installs no handler, so there is no async-signal-safety concern.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
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
    // subcommands (`run`/`plan`/`tui`/`serve`). With no subcommand, `flux` opens the REPL.
    let cli = Cli::parse();
    style::init(cli.color);
    // C-21: export the filesystem-access policy (extra read-only roots + the unconfined hatch) to the
    // environment so every workspace — including `app run` and subprocess paths — inherits it via
    // `Workspace::from_env`.
    apply_workspace_access_env(&cli);

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
                    return run_app_cmd(prompt, &agent).await;
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
            Some(Commands::Fork {
                session,
                at,
                inject,
                edit,
                replan,
                prompt,
                agent,
            }) => {
                apply_agent_env(&agent);
                run_fork(&session, at, inject, edit, replan, prompt, &agent).await
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
                action:
                    AppAction::Run {
                        agent,
                        program,
                        serve,
                    },
            }) => {
                apply_agent_env(&agent);
                run_app(program.as_deref(), &agent, serve).await
            }
            Some(Commands::Flow {
                action:
                    FlowAction::Run {
                        file,
                        model,
                        yes,
                        resumable,
                        resume,
                        resume_value,
                    },
            }) => run_flow(&file, model, yes, resumable, resume, resume_value).await,
            Some(Commands::Review {
                agent,
                files,
                format,
                fail_on,
            }) => {
                apply_agent_env(&agent);
                run_review(&agent, files, format, fail_on).await
            }
            Some(Commands::Loop { action }) => run_loop_cmd(action),
            Some(Commands::Sessions { prune }) => run_sessions(prune),
            Some(Commands::Usage(args)) => run_usage(args),
            Some(Commands::Replay {
                session,
                turn,
                sub_agents,
                json,
            }) => run_replay(&session, turn, sub_agents, json).await,
            Some(Commands::Diff { a, b, json }) => run_diff_cmd(&a, &b, json),
            Some(Commands::Corpus { action }) => run_corpus(action),
            Some(Commands::Auth { action }) => run_auth(action).await,
            Some(Commands::Plugin { action }) => run_plugin(action).await,
            Some(Commands::Endpoint { action }) => run_endpoint(action),
            Some(Commands::Skill {
                type_,
                install,
                global,
            }) => run_skill(type_, install, global).await,
            Some(Commands::Completion { shell }) => run_completion(shell.as_deref()),
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
fn apply_agent_env(flags: &AgentFlags) {
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
async fn run_app_cmd(prompt: Vec<String>, flags: &AgentFlags) -> Result<()> {
    // The `.flux` path is the first token; `-m`/`--yes` were parsed as global flags.
    let path = prompt
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("usage: flux run <app.flux> [-m provider/model] [--yes]"))?;
    run_app(Some(path), flags, None).await
}

/// Build one plugin's [`HostCapabilities`](flux_plugin::HostCapabilities) for the `flux app run`
/// path: the guarded `System` + datasource bridge + endpoint-broker fan-out, with the SAME
/// egress-audit, cross-plugin resolver, and redactor-backed secret sink hooks the `build_agent`
/// path installs on its plugins (flux D-65 parity) — a resolved cross-plugin credential is
/// registered with `secret_sink` so it never appears raw in model-visible tool output, and a
/// private-net admission is recorded through `audit`. A standalone function (not an inline closure)
/// so `run_app`'s wiring is directly unit-testable.
#[allow(clippy::too_many_arguments)]
fn app_plugin_caps(
    system: Arc<System>,
    backend: Arc<dyn flux_capabilities::DatasourceBackend>,
    manifest: &flux_plugin::PluginManifest,
    private_hosts: Vec<String>,
    resolver: Arc<dyn flux_plugin::ReferenceResolver>,
    audit: Arc<dyn flux_plugin::EgressAudit>,
    secret_sink: Arc<dyn flux_plugin::SecretSink>,
    broker: Arc<flux_capabilities::EndpointBroker>,
) -> Arc<dyn flux_plugin::HostCapabilities> {
    // Inject the broker as the resolver (ref-based IO + the `credential` capability) and the
    // redactor-backed secret sink BEFORE wrapping with the datasource + endpoint-broker host-caps.
    let inner = Arc::new(flux_capabilities::DatasourceHostCaps::new(
        flux_plugin::SystemHostCaps::new(system)
            .with_manifest(manifest)
            .with_private_net_grants(private_hosts)
            .with_grant_source(private_net_grant_source_for(&manifest.name))
            .with_egress_audit(audit)
            .with_resolver(resolver)
            .with_secret_sink(secret_sink),
        backend,
    )) as Arc<dyn flux_plugin::HostCapabilities>;
    // Compose the endpoint broker OVER the datasource caps so this plugin's `endpoint.discover`
    // calls fan out (deny-by-default, gated by `discover`).
    Arc::new(flux_capabilities::EndpointBrokerHostCaps::new(
        inner,
        broker,
        manifest.name.clone(),
        manifest.capabilities.discover,
    )) as Arc<dyn flux_plugin::HostCapabilities>
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
fn app_provider_for(spec: &str) -> (Option<std::sync::Arc<dyn Provider>>, String) {
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

async fn run_app(path: Option<&str>, flags: &AgentFlags, serve: Option<String>) -> Result<()> {
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
        let (agent, _session_id, _spec, _spawner) = build_agent(flags).await?;
        return flux_server::serve(&addr, agent, auth).await;
    };

    let auto_approve = flags.yes;
    let spec = flags
        .model
        .clone()
        .unwrap_or_else(|| "anthropic/claude-sonnet-4-6".to_string());
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
        let src = std::fs::read_to_string(path)
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
    let system = Arc::new(System::new(
        Workspace::from_env(&cwd).map_err(|e| anyhow::anyhow!("{e}"))?,
    ));
    // Scoped SSRF egress opt-in, off by default. Program-serving plugin hosts use per-plugin grants;
    // a missing or unreadable config keeps the safe default.
    let cfg = flux_config::load(&cwd).unwrap_or_default();
    // The knowledge datasource: build the program's declared datasources, and SHARE the backend so
    // integration plugins' contributed records (via the DatasourceHostCaps bridge) land in the same
    // index the `search`/`get`/`list`/`relation`/`batch_get` ops read.
    let backend = build_datasources(&program.datasources, &system).await?;
    let mut extra_tools: Vec<Arc<dyn flux_runtime::Tool>> =
        flux_capabilities::datasource_tools(backend.clone());
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
    // Discover subprocess plugins (~/.flux/plugins/*.toml) and project their ops as tools; their host
    // capabilities are the datasource bridge over the guarded System (same boundary as built-in tools).
    if let Some(dir) = plugins_dir() {
        // The cross-plugin endpoint-discovery broker (D-26/D-27), analogous to the `build_agent` path:
        // a registry of loaded plugins + the shared endpoint registry, so a consumer plugin's
        // `endpoint.discover` capability fans out to providers, and (D-27) the broker is the host-side
        // `ReferenceResolver` for ref-based IO + gated cross-plugin credential resolution.
        let plugin_registry = Arc::new(flux_capabilities::PluginRegistry::new());
        let endpoint_registry = Arc::new(flux_capabilities::EndpointRegistry::with_path(
            flux_capabilities::EndpointRegistry::default_path().unwrap_or_default(),
        ));
        let _ = endpoint_registry.load();
        let invoker = Arc::new(flux_capabilities::HostProviderInvoker::new(
            plugin_registry.clone(),
        ));
        let static_resolver = Arc::new(flux_capabilities::StaticResolver::new(
            system.clone(),
            std::collections::HashMap::new(),
        ));
        // Cross-plugin credential audit (D-27) + endpoint discovery audit (D-30): records
        // consumer->provider resolutions and per-provider discovery counts onto this run's stream —
        // parity with the `build_agent` path's `xplugin_audit`. NOTE(D-27): an interactive
        // `CrossPluginApprover` (a modal/stdin first-use prompt) is not wired here either, same as the
        // `build_agent` path — deliberate: the seam exists on the broker, but running headless, the
        // operator config grant alone authorizes; the interactive approver is a filed-separately
        // follow-up if wanted.
        let xplugin_audit: Arc<dyn flux_capabilities::CrossPluginAudit> =
            Arc::new(EventStoreCrossPluginAudit {
                store: app_events.clone(),
                stream: app_run_stream.clone(),
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
            .with_cross_plugin_audit(xplugin_audit),
        );
        // Agent-facing endpoint ops (D-28/D-30): added to the program's tool set so the app's agent
        // target can discover/select/import endpoints. The broker installed above already carries the
        // D-30 discovery audit, so `endpoint.discover`/`refresh` calls through these ops are audited
        // exactly like the `build_agent` path's.
        extra_tools.extend(flux_capabilities::endpoint_tools(
            broker.clone(),
            endpoint_registry.clone(),
        ));
        for p in flux_plugin::discover(&dir) {
            let system = system.clone();
            let backend = backend.clone();
            let caps_system = system.clone();
            let cfg_for_caps = cfg.clone();
            let broker_for_caps = broker.clone();
            let resolver_for_caps = broker.clone() as Arc<dyn flux_plugin::ReferenceResolver>;
            let audit: Arc<dyn flux_plugin::EgressAudit> = Arc::new(EventStoreEgressAudit {
                store: app_events.clone(),
                stream: app_run_stream.clone(),
            });
            let secret_sink = Arc::new(RedactorSecretSink {
                redactor: redactor.clone(),
            }) as Arc<dyn flux_plugin::SecretSink>;
            let make_caps = move |m: &flux_plugin::PluginManifest| {
                let plugin_private_hosts = effective_plugin_private_hosts(&cfg_for_caps, &m.name);
                // Parity with the `build_agent` path (D-20 egress audit + D-27 secret sink) — the SAME
                // function both `run_app`'s wiring and its own unit test call (flux D-65).
                app_plugin_caps(
                    caps_system,
                    backend,
                    m,
                    plugin_private_hosts,
                    resolver_for_caps,
                    audit,
                    secret_sink,
                    broker_for_caps,
                )
            };
            match flux_plugin::load_plugin_tools(
                &system,
                &p.descriptor.program,
                &p.descriptor.args,
                make_caps,
            )
            .await
            {
                Ok(lp) => {
                    plugin_registry.register(
                        lp.manifest.name.clone(),
                        flux_capabilities::ProviderEntry {
                            manifest: Arc::new(lp.manifest.clone()),
                            host: lp.host.clone(),
                            caps: lp.caps.clone(),
                        },
                    );
                    extra_tools.extend(lp.tools);
                }
                Err(e) => eprintln!(
                    "{}",
                    style::dim(&format!("(plugin `{}` failed to load: {e})", p.name))
                ),
            }
        }
    }

    let channel_decls = program.channels.clone();
    // The built-in `strict-review` program's `review_code` journey calls `strict_review`, which fans
    // out to reviewer sub-agents via `task` — the same `build_review_sub_agents` helper `flux review`
    // uses, so the two surfaces delegate through the identical envelope, never a re-derived one.
    let sub_agents = is_builtin_strict_review
        .then(|| build_review_sub_agents(&cwd, &spec, model.clone(), flags.max_tokens));
    let app = std::sync::Arc::new(flux_app::App::with_events(
        program,
        provider,
        model,
        auto_approve,
        extra_tools,
        sub_agents,
        redactor,
        app_events,
    ));
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
fn server_auth_from_config() -> Result<flux_server::ServerAuth> {
    let token = std::env::var("FLUX_SERVER_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let cwd = std::env::current_dir()?;
    let server = flux_config::load(&cwd)?.server;
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
    let (agent, session_id, model_spec, _spawner) = build_agent(&flags).await?;
    flux_tui::run(agent, session_id, auto_approve, Some(model_spec)).await
}

/// The credential-ref **location** column for a record — the `Ref` location string (e.g.
/// `kubernetes/ns/secret/key`) or `none`. NEVER a value: `Ref`'s `Display` is a location by
/// construction (`flux-secret`), and the persisted record carries no material in the first place.
fn credential_location(record: &flux_secret::endpoint::EndpointRecord) -> String {
    record
        .endpoint
        .credential_ref
        .as_ref()
        .map(|r| r.to_string())
        .unwrap_or_else(|| "none".to_string())
}

/// One persisted record as a list row — bare URL (no creds), owner, ttl/health, and the credential
/// *location*. Shared by the `list` renderer and tested directly so the redaction guarantee is pinned.
fn render_endpoint_row(record: &flux_secret::endpoint::EndpointRecord) -> String {
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
fn run_endpoint(action: EndpointAction) -> Result<()> {
    use flux_capabilities::EndpointRegistry;

    // The persisted store. A standalone CLI invocation has no in-memory session registry, so every
    // subcommand operates on `~/.flux/endpoints.toml` (loaded fresh; a missing file is empty).
    let path = EndpointRegistry::default_path()
        .ok_or_else(|| anyhow::anyhow!("HOME is not set (no endpoints store path)"))?;
    let registry = EndpointRegistry::with_path(path.clone());
    registry
        .load()
        .map_err(|e| anyhow::anyhow!("load endpoints store: {e}"))?;

    match action {
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
async fn run_plugin(action: Option<PluginAction>) -> Result<()> {
    let dir = plugins_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
    run_plugin_in(&dir, action).await
}

/// The dir-parameterized body of [`run_plugin`] (tests pass a temp dir so they don't touch `HOME`).
async fn run_plugin_in(dir: &std::path::Path, action: Option<PluginAction>) -> Result<()> {
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
            let cwd = std::env::current_dir()?;
            let cfg = flux_config::load(&cwd).unwrap_or_default();
            let system = Arc::new(System::new(
                Workspace::from_env(&cwd).map_err(|e| anyhow::anyhow!("{e}"))?,
            ));
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
            let (input, problems) = build_invoke_input(&schema, base, &arg, validate);

            if dry_run {
                // Validate-locally: print the coerced input + problems; never call the op.
                let _ = host.shutdown().await;
                let dry = serde_json::json!({
                    "plugin": name,
                    "operation": resolved_op,
                    "valid": problems.is_empty(),
                    "problems": problems,
                    "input": input,
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
            let caps = flux_capabilities::DatasourceHostCaps::new(
                flux_plugin::SystemHostCaps::new(system)
                    .with_manifest(&manifest)
                    .with_private_net_grants(effective_plugin_private_hosts(&cfg, &manifest.name))
                    .with_grant_source(private_net_grant_source_for(&manifest.name)),
                backend.clone(),
            );
            let result = host.call_with_host(&resolved_op, input, &caps).await;
            let _ = host.shutdown().await;
            let value =
                result.map_err(|e| anyhow::anyhow!("plugin `{name}` op `{resolved_op}`: {e}"))?;
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
        } => match local_dir {
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
                        let owned_here = prog
                            .parent()
                            .is_some_and(|p| p == canon_dir.as_path() || p == bin_dir.as_path());
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
                             install), or `--dir [path]` (local scan of a built \
                             `plugins/target/release`) — bare `install` no longer guesses"
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
        },
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

// --- plugin `status`: liveness + declared surface (D-19) -------------------------------

/// Result of probing one plugin's health + surface. `missing` is determined without spawning
/// (the binary does not resolve on `PATH`); `unloadable` means the binary spawned but its
/// manifest would not load (e.g. it is not a flux plugin); `live` means the manifest loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Liveness {
    Live,
    Missing,
    Unloadable(String),
}

#[derive(Debug, Clone)]
struct PluginStatusReport {
    name: String,
    program: String,
    args: Vec<String>,
    pin: Option<String>,
    /// The installed version, if the descriptor carries one (remote installs only — D-47).
    version: Option<String>,
    /// The D-48 verification outcome: the binary on disk **re-hashed** against the descriptor's
    /// recorded `sha256` — `verified`, `hash drift` (also a spawn refusal), or
    /// `unverified (local)` for hashless dev descriptors.
    verification: flux_plugin::Verification,
    liveness: Liveness,
    manifest: Option<flux_plugin::PluginManifest>,
}

/// Resolve `program` (an absolute/relative path, or a bare name on `PATH`) to an existing file.
/// Used for the `missing` vs `unloadable` split in `status` without spawning a process.
fn program_resolves(program: &str) -> bool {
    let p = std::path::Path::new(program);
    if p.parent().is_some() {
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
async fn build_status_report(
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
async fn plugin_status_one(dir: &std::path::Path, name: &str) -> Result<PluginStatusReport> {
    let d = flux_plugin::load_descriptor(dir, name)
        .with_context(|| format!("load descriptor `{name}`"))?
        .ok_or_else(|| anyhow::anyhow!("no such plugin `{name}`"))?;
    build_status_report(name, d).await
}

/// Summarize every installed plugin (sorted by name, matching `discover`).
async fn plugin_status_all(dir: &std::path::Path) -> Result<Vec<PluginStatusReport>> {
    let mut out = Vec::new();
    for p in flux_plugin::discover(dir) {
        out.push(build_status_report(&p.name, p.descriptor).await?);
    }
    Ok(out)
}

/// Spawn the plugin and load its manifest (liveness probe). Reuses the one guarded, D-48
/// hash-verified spawn path (`PluginHost::spawn_verified` over a workspace-rooted `System`), the
/// same boundary `call` and agent discovery use.
async fn spawn_and_load_manifest(
    name: &str,
    d: &flux_plugin::PluginDescriptor,
) -> Result<flux_plugin::PluginManifest> {
    let system = System::new(
        Workspace::from_env(&std::env::current_dir()?).map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    let mut host = flux_plugin::PluginHost::spawn_verified(&system, name, d)
        .await
        .with_context(|| format!("spawn `{}`", d.program))?;
    let m = host.manifest().await.context("fetch plugin manifest")?;
    let _ = host.shutdown().await;
    Ok(m)
}

/// Print one plugin's status: header (name → program args, pin) + liveness label, then the
/// declared surface (version, op/auth/endpoint/datasource counts, requested capabilities).
fn print_plugin_status_report(r: &PluginStatusReport) {
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

/// Describe how a declared auth purpose would resolve right now — which env key (if any) is set,
/// or whether a stored OAuth token exists — without ever printing the resolved secret value.
fn describe_auth_resolution(plugin: &str, m: &flux_plugin::AuthMethod) -> String {
    if m.oauth2.is_some() {
        let key = format!("plugin:{plugin}:{}", m.purpose);
        if flux_credentials::load_token(&key).is_some() {
            return format!(
                "✓ {} — stored OAuth token (`flux auth login {plugin}`)",
                m.purpose
            );
        }
    }
    for key in &m.env {
        if std::env::var(key).is_ok() {
            return format!("✓ {} — env ${key}", m.purpose);
        }
    }
    match (m.oauth2.is_some(), m.env.is_empty()) {
        (true, true) => format!(
            "· {} — not configured (`flux auth login {plugin}`)",
            m.purpose
        ),
        (true, false) => format!(
            "· {} — not configured (env: {}, or `flux auth login {plugin}`)",
            m.purpose,
            m.env.join(", ")
        ),
        (false, true) => format!("· {} — no env keys declared", m.purpose),
        (false, false) => format!(
            "· {} — not configured (env: {})",
            m.purpose,
            m.env.join(", ")
        ),
    }
}

/// Describe how a declared endpoint would resolve right now. Base URLs are not secret, so the
/// resolved value itself is shown (the plugin-declared `default` fallback is likewise not secret).
fn describe_endpoint_resolution(ep: &flux_plugin::EndpointSpec) -> String {
    for key in &ep.env {
        if let Ok(v) = std::env::var(key) {
            return format!("✓ {} — {v} (env ${key})", ep.name);
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

fn resolve_plugin_operation_name(
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
fn resolve_field_schema<'a>(node: &'a Value, defs: &'a Value) -> &'a Value {
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
fn field_base_type(node: &Value) -> Option<String> {
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
fn coerce_arg_value(field_schema: &Value, defs: &Value, raw: &str) -> Result<Value> {
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
fn build_invoke_input(
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

fn available_plugin_operations(manifest: &flux_plugin::PluginManifest) -> String {
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
async fn run_skill(type_: Option<skill_cmd::SkillType>, install: bool, global: bool) -> Result<()> {
    if global && !install {
        bail!("--global requires --install");
    }

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

async fn render_generated_skill(kind: skill_cmd::SkillType) -> Result<skill_cmd::RenderedSkill> {
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
fn skill_ops_registry() -> Result<(ToolRegistry, Vec<flux_evidence::ToolGroup>)> {
    let mut registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut registry);
    flux_eval::register_eval_ops(&mut registry);
    flux_tools::register_reflect(&mut registry);
    // Native web ops for the catalog render (no egress config / audit — this registry never fetches).
    flux_web::register_web(&mut registry, &flux_web::WebOptions::default());
    flux_capabilities::register_datasource_ops(
        &mut registry,
        Arc::new(flux_capabilities::MemoryBackend::new()),
    );

    let cwd = std::env::current_dir()?;
    let mut groups = flux_tools::groups::builtin_groups();
    groups.push(flux_eval::eval_group());
    groups.push(flux_web::browser_group());
    let groups = flux_config::merge_groups(groups, flux_config::load_groups(&cwd));
    Ok((registry, groups))
}

/// The generated skill root directory: project `.flux/skills`, or global `~/.claude/skills`.
fn skills_root_dir(global: bool) -> Result<std::path::PathBuf> {
    if global {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        Ok(home.join(".claude").join("skills"))
    } else {
        Ok(std::env::current_dir()?.join(".flux").join("skills"))
    }
}

fn write_generated_skill(
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
async fn load_plugin_manifests(
    dir: &std::path::Path,
) -> Result<Vec<(String, flux_plugin::PluginManifest)>> {
    let mut plugins: Vec<(String, flux_plugin::PluginManifest)> = Vec::new();
    // Plugins launch through the one guarded spawn path, which needs a workspace-rooted System.
    let system = System::new(
        Workspace::from_env(&std::env::current_dir()?).map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    for p in flux_plugin::discover(dir) {
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
async fn run_plugin_skill(
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

/// Find every `flux-plugin-<name>` (or, on Windows, `flux-plugin-<name>.exe`) executable in `dir`,
/// returning `(name, absolute-program-path)` pairs sorted by name. Skips sidecar files (e.g.
/// `*.d`). Missing dir is an error (the caller reports).
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

/// `flux auth status | login <provider>`.
/// Map a resolved `provider/model` spec to the `flux auth status` row it authenticates against, so
/// the status view can flag the active default provider. Returns `None` for specs that need no
/// listed credential (local `ollama*`, or `aws`, which isn't a listed row).
fn auth_row_for_spec(spec: &str) -> Option<&'static str> {
    // The offline `mock` provider needs no credential (bare `mock` resolves to `anthropic` in
    // `spec_provider_prefix` for provider construction, but there is no key to flag here).
    if spec == "mock" {
        return None;
    }
    match spec_provider_prefix(spec)? {
        "anthropic" => Some("anthropic"),
        "claude" => Some("claude"),
        "openai" => Some("openai"),
        "codex" => Some("codex"),
        "openrouter" | "openrouter-anthropic" => Some("openrouter"),
        // `aws` (not a listed status row) and local `ollama*` (keyless) have no row to mark active.
        _ => None,
    }
}

/// Render `flux auth status` grouped by state (Available / Not configured), with a summary line and
/// an active-default-provider marker. Pure (returns the block) so it is unit-testable.
fn format_auth_status(
    rows: &[flux_credentials::ProviderAuth],
    default_spec: &str,
    active: Option<&str>,
) -> String {
    let total = rows.len();
    let avail = rows.iter().filter(|r| r.available).count();
    let mut out = String::new();
    out.push_str(&format!("Providers · {avail} of {total} configured\n"));

    // Default-model line: name the resolved default provider and whether its credential is present.
    match active {
        Some(p) => {
            let mark = match rows.iter().find(|r| r.provider == p).map(|r| r.available) {
                Some(true) => " ✓",
                Some(false) => " ·",
                None => "",
            };
            out.push_str(&format!("default model: {default_spec} → {p}{mark}\n"));
        }
        None => out.push_str(&format!("default model: {default_spec}\n")),
    }

    let w = rows.iter().map(|r| r.provider.len()).max().unwrap_or(0);
    let available: Vec<_> = rows.iter().filter(|r| r.available).collect();
    let missing: Vec<_> = rows.iter().filter(|r| !r.available).collect();

    if !available.is_empty() {
        out.push_str("\n  Available\n");
        let show_marker = available.iter().any(|r| active == Some(r.provider));
        for r in &available {
            if show_marker {
                let act = if active == Some(r.provider) {
                    "← active"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "    ✓ {:<w$}   {:<8}   {}\n",
                    r.provider, act, r.source
                ));
            } else {
                out.push_str(&format!("    ✓ {:<w$}   {}\n", r.provider, r.source));
            }
        }
    }
    if !missing.is_empty() {
        out.push_str("\n  Not configured\n");
        // Mark the active default here too if it's unconfigured — otherwise the `← active` tag would
        // vanish exactly when the user most needs to see which missing provider is the default.
        let show_marker = missing.iter().any(|r| active == Some(r.provider));
        for r in &missing {
            let hint = r.hint.as_deref().unwrap_or(r.source.as_str());
            if show_marker {
                let act = if active == Some(r.provider) {
                    "← active"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "    · {:<w$}   {:<8}   {}\n",
                    r.provider, act, hint
                ));
            } else {
                out.push_str(&format!("    · {:<w$}   {}\n", r.provider, hint));
            }
        }
    }
    out
}

async fn run_auth(action: Option<AuthAction>) -> Result<()> {
    match action.unwrap_or(AuthAction::Status) {
        AuthAction::Status => {
            let cwd = std::env::current_dir().unwrap_or_default();
            let cfg = flux_config::load(&cwd).unwrap_or_default();
            let default_spec = resolve_model_spec(&None, &cfg);
            let active = auth_row_for_spec(&default_spec);
            let rows = flux_credentials::auth_status();
            print!("{}", format_auth_status(&rows, &default_spec, active));
            Ok(())
        }
        AuthAction::Login { provider, password } => match provider.as_str() {
            "claude" => login_claude().await,
            "codex" => login_codex().await,
            // Any other name is treated as an installed OAuth2 plugin (plugin-oauth, D-82).
            name => login_plugin(name, password).await,
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

/// Interactive Codex (ChatGPT subscription) PKCE login. Unlike claude's paste-the-code flow, the
/// codex client's registered redirect is `http://localhost:1455/auth/callback` (the upstream codex
/// CLI's pattern), so flux listens there and the code arrives without pasting.
async fn login_codex() -> Result<()> {
    codex_login_flow(flux_credentials::CODEX_TOKEN_URL, |url, _state| async move {
        println!(
            "Open this URL and approve access — flux is listening on localhost:{} for the redirect:\n\n{url}\n",
            flux_credentials::CODEX_REDIRECT_PORT
        );
        wait_for_codex_callback().await
    })
    .await?;
    println!("\u{2713} stored Codex subscription credentials in ~/.flux/credentials.toml");
    Ok(())
}

/// Drive the codex PKCE login: generate the PKCE pair + CSRF state, hand the authorize URL (and
/// the state, for test injection) to `callback`, then exchange the returned `code#state` against
/// `token_url` and persist under the `codex` provider. The interactive path passes the real token
/// endpoint + the localhost:1455 listener; the hermetic test passes a loopback stub + a canned
/// callback (no browser, no network).
async fn codex_login_flow<F, Fut>(token_url: &str, callback: F) -> Result<()>
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let pkce = flux_credentials::generate_pkce();
    let state = flux_credentials::generate_state();
    let url = flux_credentials::codex_authorize_url(&pkce, &state);
    let code = callback(url, state.clone()).await?;
    flux_credentials::codex_exchange_and_store_at(token_url, &code, &state, &pkce.verifier)
        .await
        .context("exchange authorization code")
}

/// Bind the codex client's registered redirect address (`localhost:1455`) and wait for the OAuth
/// redirect, answering the browser with a small confirmation page. Non-callback requests (e.g.
/// `/favicon.ico`) get a 404 and the wait continues. Returns the callback as `code#state` — the
/// shape `codex_exchange_and_store` binds against the login's CSRF state.
async fn wait_for_codex_callback() -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener =
        tokio::net::TcpListener::bind(("127.0.0.1", flux_credentials::CODEX_REDIRECT_PORT))
            .await
            .with_context(|| {
                format!(
            "bind localhost:{} for the OAuth callback (is another login or the codex CLI running?)",
            flux_credentials::CODEX_REDIRECT_PORT
        )
            })?;
    loop {
        let (mut sock, _) = listener.accept().await.context("accept OAuth callback")?;
        // The callback is a small GET; one read is enough for the request line we parse.
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
        // "GET <target> HTTP/1.1" — take the target.
        let target = req.split_whitespace().nth(1).unwrap_or("");
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != flux_credentials::CODEX_REDIRECT_PATH {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let result = parse_codex_callback(query);
        let page = match &result {
            Ok(_) => "Login complete — you can return to the terminal.",
            Err(_) => "Login failed — see the terminal for details.",
        };
        let body = format!("<!doctype html><html><body><p>{page}</p></body></html>");
        let _ = sock
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let (code, state) = result?;
        return Ok(format!("{code}#{state}"));
    }
}

/// Extract `code`/`state` (or the provider's `error`) from the OAuth callback query string.
fn parse_codex_callback(query: &str) -> Result<(String, String)> {
    let (mut code, mut state, mut error) = (None, None, None);
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(v);
        match k {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "error" => error = Some(v),
            _ => {}
        }
    }
    if let Some(e) = error {
        bail!("authorization failed: {e}");
    }
    match (code, state) {
        (Some(c), Some(s)) if !c.is_empty() => Ok((c, s)),
        _ => bail!("OAuth callback did not include an authorization code and state"),
    }
}

/// Log in to an installed OAuth2 plugin (plugin-oauth, D-82): load its manifest, resolve its declared
/// OAuth2 endpoint, run the browser PKCE `authorization_code` flow (or the `--password` grant), and
/// store the tokens under `plugin:<name>:<purpose>` — the same key the host resolves at call time, so
/// a subsequent `flux plugin call` needs no env token.
async fn login_plugin(name: &str, password: bool) -> Result<()> {
    let dir = plugins_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set — no plugin store"))?;
    let desc = flux_plugin::load_descriptor(&dir, name)
        .context("load plugin descriptor")?
        .ok_or_else(|| anyhow::anyhow!("no such plugin `{name}` — install it first"))?;
    let manifest = spawn_and_load_manifest(name, &desc).await?;
    let method = manifest
        .auth
        .iter()
        .find(|a| a.oauth2.is_some())
        .ok_or_else(|| anyhow::anyhow!("plugin `{name}` declares no OAuth2 auth method"))?;
    let oauth = method.oauth2.as_ref().expect("filtered to Some above");
    let base = resolve_manifest_endpoint(&manifest, &oauth.endpoint).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot resolve OAuth endpoint `{}` for plugin `{name}` — set its declared env or default",
            oauth.endpoint
        )
    })?;
    let token_url = join_endpoint_path(&base, &oauth.token_path);
    let key = format!("plugin:{name}:{}", method.purpose);
    let scope = oauth.scopes.join(" ");

    let token = if password {
        let username = prompt_line("username: ")?;
        let secret = rpassword::prompt_password("password: ").context("read password")?;
        flux_credentials::oauth_token_grant(
            &token_url,
            &[
                ("grant_type", "password"),
                ("username", username.trim()),
                ("password", &secret),
                ("client_id", &oauth.client_id),
                ("scope", &scope),
            ],
        )
        .await
        .context("password grant")?
    } else {
        let redirect = oauth.redirect.as_ref().ok_or_else(|| {
            anyhow::anyhow!("plugin `{name}` OAuth2 declares no loopback redirect; use --password")
        })?;
        let redirect_uri = format!("http://localhost:{}{}", redirect.port, redirect.path);
        let authorize_url = join_endpoint_path(&base, &oauth.authorize_path);
        let (port, path) = (redirect.port, redirect.path.clone());
        plugin_oauth_code_grant(
            &token_url,
            &authorize_url,
            &oauth.client_id,
            &scope,
            &redirect_uri,
            |url, _state| async move {
                println!(
                    "Open this URL and approve access — flux is listening on localhost:{port} for the redirect:\n\n{url}\n"
                );
                wait_for_oauth_callback(port, &path).await
            },
        )
        .await?
    };
    flux_credentials::save_token(&key, &token)?;
    println!(
        "\u{2713} stored OAuth credentials for plugin `{name}` (purpose `{}`) in ~/.flux/credentials.toml",
        method.purpose
    );
    Ok(())
}

/// The `authorization_code` + PKCE half of a plugin login (plugin-oauth, D-82): build the authorize
/// URL, run the browser callback (injected — the interactive path binds the loopback listener; the
/// test injects a canned callback), verify the CSRF state, and exchange the code against `token_url`.
async fn plugin_oauth_code_grant<F, Fut>(
    token_url: &str,
    authorize_url: &str,
    client_id: &str,
    scope: &str,
    redirect_uri: &str,
    callback: F,
) -> Result<flux_credentials::OAuthToken>
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let pkce = flux_credentials::generate_pkce();
    let state = flux_credentials::generate_state();
    let url = flux_credentials::oauth_authorize_url(
        authorize_url,
        client_id,
        redirect_uri,
        scope,
        &pkce,
        &state,
    );
    let code_state = callback(url, state.clone()).await?;
    let (code, ret_state) = code_state
        .split_once('#')
        .unwrap_or((code_state.as_str(), ""));
    if ret_state != state {
        bail!("OAuth callback state mismatch — possible CSRF, aborting login");
    }
    flux_credentials::oauth_token_grant(
        token_url,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", &pkce.verifier),
        ],
    )
    .await
    .context("exchange authorization code")
}

/// Resolve a manifest endpoint's base URL for login (declared env keys → default). Templated
/// endpoints are resolved host-side at call time, not here.
fn resolve_manifest_endpoint(m: &flux_plugin::PluginManifest, name: &str) -> Option<String> {
    let ep = m.endpoints.iter().find(|e| e.name == name)?;
    for k in &ep.env {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    ep.default.clone()
}

/// Join an endpoint base URL and a declared path (`https://host` + `/oauth/token`).
fn join_endpoint_path(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Prompt on the terminal and read one trimmed line (visible echo — for a non-secret like a username).
fn prompt_line(msg: &str) -> Result<String> {
    print!("{msg}");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

/// Bind `127.0.0.1:{port}` and wait for the OAuth redirect at `path`, answering the browser with a
/// small confirmation page (plugin-oauth, D-82 — the generic form of [`wait_for_codex_callback`],
/// with a bounded wait). Non-callback requests get a 404 and the wait continues. Returns `code#state`.
async fn wait_for_oauth_callback(port: u16, path: &str) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| {
            format!("bind localhost:{port} for the OAuth callback (is another login running?)")
        })?;
    let accept = async {
        loop {
            let (mut sock, _) = listener.accept().await.context("accept OAuth callback")?;
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let target = req.split_whitespace().nth(1).unwrap_or("");
            let (req_path, query) = target.split_once('?').unwrap_or((target, ""));
            if req_path != path {
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                continue;
            }
            let result = parse_codex_callback(query);
            let page = if result.is_ok() {
                "Login complete — you can return to the terminal."
            } else {
                "Login failed — see the terminal for details."
            };
            let body = format!("<!doctype html><html><body><p>{page}</p></body></html>");
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let (code, state) = result?;
            return Ok(format!("{code}#{state}"));
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(300), accept).await {
        Ok(r) => r,
        Err(_) => bail!("timed out waiting for the OAuth callback on localhost:{port}"),
    }
}

/// Minimal percent-decoding for OAuth callback query values (`+` is left as-is — codes and states
/// are URL-safe base64, never space-bearing).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
        app_plugin_caps, build_datasources, build_invoke_input, coerce_arg_value, cost_annotation,
        credential_location, format_evidence, loop_machinery_label, new_render_suffix,
        plugin_binaries_in, plugin_status_one, render_endpoint_row, render_review_markdown,
        resolve_plugin_operation_name, run_corpus_export_with, run_plugin_in, run_usage_with,
        should_fail, tool_preview, truncate, usage_annotation, write_generated_skill, EventStore,
        EventStoreCrossPluginAudit, EventStoreEgressAudit, Liveness, PluginAction,
        RedactorSecretSink, ReviewSeverity,
    };
    use flux_flow::AgentSink;
    use serde_json::json;

    /// C-11: every subcommand builds providers through the ONE factory (`build_provider` /
    /// `provider_for`), and the factory owns the aws chain — with static env creds present the
    /// chain no-ops (no network) and the `aws` provider constructs from any (sync) caller, which
    /// is exactly the `flux review` sub-agent-factory path that used to fail
    /// "AWS_ACCESS_KEY_ID is not set".
    #[test]
    fn provider_factory_constructs_aws_from_static_env() {
        // Serialized implicitly: this is the only flux-cli test touching AWS_* env.
        std::env::set_var("AWS_ACCESS_KEY_ID", "AKIATEST");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "secret");
        std::env::set_var("AWS_REGION", "us-east-1");
        let (native, provider, model) =
            super::build_provider("aws/sonnet").expect("factory constructs aws from static env");
        assert_eq!(provider, "aws");
        assert_eq!(model, "us.anthropic.claude-sonnet-4-6");
        drop(native);
        let boxed = super::provider_for("aws/sonnet").expect("sub-agent factory path too");
        drop(boxed);
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        std::env::remove_var("AWS_REGION");
    }

    /// C-11: the lazy provider used by deterministic execution paths (`flow run`, `preset --run`)
    /// constructs WITHOUT touching any credential; its display name is the provider prefix.
    #[test]
    fn lazy_provider_constructs_without_credentials() {
        use flux_provider::Provider as _;
        let p = super::LazyProvider::new("anthropic/claude-sonnet-4-6".to_string());
        assert_eq!(p.name(), "anthropic");
    }

    /// F6: `flux plugin list` is accepted as an alias of the terse `ls` default.
    #[test]
    fn plugin_list_is_alias_for_ls() {
        use super::{Cli, Commands};
        use clap::Parser;
        let cli = Cli::try_parse_from(["flux", "plugin", "list"]).expect("`plugin list` parses");
        assert!(
            matches!(
                cli.command,
                Some(Commands::Plugin {
                    action: Some(PluginAction::Ls)
                })
            ),
            "`plugin list` should resolve to the Ls action"
        );
        // The terse form still resolves the same way.
        let cli2 = Cli::try_parse_from(["flux", "plugin", "ls"]).expect("`plugin ls` parses");
        assert!(matches!(
            cli2.command,
            Some(Commands::Plugin {
                action: Some(PluginAction::Ls)
            })
        ));
    }

    /// F2: the zero-arg ambient reads (`now`/`cwd`/`home_dir`/`sys_info`) are pre-allowed by the
    /// default permission set, so a `now()` in a stored flow never reaches the approval gate (which
    /// auto-denies on a non-TTY). Workspace reads stay allowed; a mutating op still gates.
    #[test]
    fn default_allow_covers_ambient_reads() {
        use flux_runtime::{PermDecision, PermissionManager};
        let allow: Vec<String> = super::DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect();
        let m = PermissionManager::from_rules(&allow, &[]);
        for op in ["now", "cwd", "home_dir", "sys_info", "read"] {
            assert_eq!(
                m.check(op, &[]),
                PermDecision::Allow,
                "`{op}` should be pre-allowed by the default permission set"
            );
        }
        // A mutating op is not in the default set — it still gates.
        assert_eq!(m.check("write", &[]), PermDecision::Ask);
    }

    /// The grouped `flux auth status` renderer: summary line, active-default marker, the two state
    /// groups, and per-provider setup hints.
    #[test]
    fn auth_status_groups_by_state() {
        use flux_credentials::ProviderAuth;
        let rows = vec![
            ProviderAuth {
                provider: "anthropic",
                available: true,
                source: "ANTHROPIC_API_KEY (env)".into(),
                hint: None,
            },
            ProviderAuth {
                provider: "claude",
                available: false,
                source: "not found".into(),
                hint: Some("flux auth login claude".into()),
            },
            ProviderAuth {
                provider: "openai",
                available: true,
                source: "OPENAI_API_KEY (env)".into(),
                hint: None,
            },
        ];
        let out = super::format_auth_status(&rows, "sonnet", Some("anthropic"));
        assert!(out.contains("Providers · 2 of 3 configured"), "{out}");
        assert!(out.contains("default model: sonnet → anthropic ✓"), "{out}");
        assert!(out.contains("Available"));
        assert!(out.contains("Not configured"));
        assert!(out.contains("flux auth login claude"), "{out}");
        // The active marker lands on anthropic only.
        let active_line = out
            .lines()
            .find(|l| l.contains("← active"))
            .expect("an active row");
        assert!(active_line.contains("anthropic"));
        assert!(!out.contains("openai   ← active"));
    }

    /// The `provider/model` spec → auth-status-row mapping used to flag the active provider.
    #[test]
    fn auth_row_mapping() {
        assert_eq!(super::auth_row_for_spec("sonnet"), Some("anthropic"));
        assert_eq!(super::auth_row_for_spec("claude/sonnet"), Some("claude"));
        assert_eq!(
            super::auth_row_for_spec("openrouter-anthropic/x"),
            Some("openrouter")
        );
        assert_eq!(super::auth_row_for_spec("ollama/llama"), None);
    }

    #[test]
    fn app_serve_provider_honors_mock() {
        // A-60 / F-014: a served program under `--serve -m mock` must resolve to the offline mock
        // provider, not fall through to the Anthropic path (which fails on low credits).
        let (provider, model) = super::app_provider_for("mock");
        assert_eq!(model, "mock");
        assert_eq!(
            provider.expect("mock provider built").name(),
            "mock",
            "served -m mock resolves to the offline mock, not Anthropic"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reset_sigpipe_installs_sig_dfl() {
        // A-61 / F-006: after the reset, SIGPIPE must be SIG_DFL — Rust's std defaults it to SIG_IGN,
        // which is exactly what makes `println!` panic on a broken pipe. `signal()` returns the
        // PREVIOUS disposition, so reading it back right after the reset proves it installed SIG_DFL
        // (a no-op reset would read back SIG_IGN and fail this).
        super::reset_sigpipe();
        let prev = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
        assert_eq!(prev, libc::SIG_DFL, "reset_sigpipe installs SIG_DFL");
    }

    #[test]
    fn diagnostics_header_matches_the_failure_class() {
        // A-62 / F-010: the "references unknown operations" header/refusal must appear ONLY when every
        // diagnostic is genuinely an unknown-op error — a non-unknown-op failure under that header
        // misleads both the reader and the repair-reading planner.
        use flux_flow::analyze::Diagnostic;
        let unknown = vec![Diagnostic::new("unknown operation: `foo`")];
        assert!(super::diagnostics_all_unknown_op(&unknown));
        let other = vec![Diagnostic::new(
            "a value template (`obj`/`list`) may only contain pure value leaves",
        )];
        assert!(
            !super::diagnostics_all_unknown_op(&other),
            "a non-unknown-op failure is not labeled 'unknown operations'"
        );
        let mixed = vec![
            Diagnostic::new("unknown operation: `foo`"),
            Diagnostic::new("`return` is not allowed inside a `parallel` branch"),
        ];
        assert!(
            !super::diagnostics_all_unknown_op(&mixed),
            "a mixed set is not all-unknown-op"
        );
        assert!(
            !super::diagnostics_all_unknown_op(&[]),
            "empty is not unknown-op"
        );
    }

    /// L-02: skill discovery layers CLI `--skill-dir` above `[skills] dirs` from config, above the
    /// well-known defaults — earlier layers win a name clash.
    #[test]
    fn load_skills_layers_cli_over_config_over_defaults() {
        let root = std::env::temp_dir().join(format!("flux-cli-skills-{}", std::process::id()));
        for (dir, body) in [
            (".flux/skills", "from default"),
            ("cfg-skills", "from config"),
            ("cli-skills", "from cli"),
        ] {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("s.md"),
                format!("---\nname: l02-cli-layering\n---\n{body}"),
            )
            .unwrap();
        }
        let cfg = flux_config::Config {
            skills: flux_config::SkillsConfig {
                dirs: vec!["cfg-skills".to_string()],
            },
            ..Default::default()
        };

        // Config layer beats the well-known default...
        let skills = super::load_skills(&root, &cfg, &[]);
        let s = skills
            .iter()
            .find(|s| s.name == "l02-cli-layering")
            .unwrap();
        assert_eq!(s.body, "from config");

        // ...and a CLI --skill-dir beats the config layer.
        let skills = super::load_skills(&root, &cfg, &[root.join("cli-skills")]);
        let s = skills
            .iter()
            .find(|s| s.name == "l02-cli-layering")
            .unwrap();
        assert_eq!(s.body, "from cli");
        std::fs::remove_dir_all(&root).ok();
    }

    /// C-08: `flux auth login codex` drives a full PKCE flow — authorize URL with challenge+state,
    /// callback code exchanged (form-encoded `authorization_code` grant with the verifier), token
    /// persisted under the `codex` provider. Hermetic: a loopback stub stands in for
    /// auth.openai.com's token endpoint and the callback is injected (no browser, no port 1455).
    /// Serialized implicitly: the only flux-cli test that repoints HOME (the store is ~/.flux).
    #[tokio::test]
    async fn auth_login_codex_runs_pkce_flow() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let home = std::env::temp_dir().join(format!("flux-login-codex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);

        // Stub token endpoint: answers one POST with a token response, captures the request.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if req.len() >= head_end + 4 + len {
                        break;
                    }
                }
            }
            let body =
                r#"{"access_token":"at_cli_c08","refresh_token":"rt_cli_c08","expires_in":3600}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });

        // Injected callback: assert the authorize URL carries PKCE + this login's state, then
        // return the `code#state` shape the real localhost:1455 listener produces.
        super::codex_login_flow(
            &format!("http://{addr}/oauth/token"),
            |url, state| async move {
                assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
                assert!(url.contains("code_challenge="));
                assert!(url.contains("code_challenge_method=S256"));
                assert!(url.contains(&format!("state={state}")));
                Ok(format!("cli-test-code#{state}"))
            },
        )
        .await
        .expect("login flow completes against the stub endpoint");

        // The exchange was a PKCE authorization_code grant…
        let req = server.await.unwrap();
        assert!(req.contains("grant_type=authorization_code"));
        assert!(req.contains("code=cli-test-code"));
        assert!(req.contains("code_verifier="));

        // …and the token landed under the `codex` provider, in the same store import fills.
        let store = std::fs::read_to_string(home.join(".flux").join("credentials.toml")).unwrap();
        std::fs::remove_dir_all(&home).ok();
        assert!(store.contains("[codex]"), "stored under `codex`: {store}");
        assert!(store.contains("at_cli_c08"));
    }

    /// D-82: the plugin `authorization_code` login builds a PKCE authorize URL from the manifest
    /// config and exchanges the callback code against the token endpoint, yielding a storable token
    /// (the store→resolve path a later `plugin call` uses is covered in flux-plugin). No `$HOME`
    /// mutation, so it can't race the codex login test.
    #[tokio::test]
    async fn plugin_oauth_code_grant_builds_pkce_url_and_exchanges() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if req.len() >= head_end + 4 + len {
                        break;
                    }
                }
            }
            let body =
                r#"{"access_token":"at_plugin","refresh_token":"rt_plugin","expires_in":3600}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });

        let token = super::plugin_oauth_code_grant(
            &format!("http://{addr}/oauth/token"),
            "https://auth.example.com/oauth/authorize",
            "plugin-client",
            "read write",
            "http://localhost:9876/cb",
            |url, state| async move {
                assert!(url.starts_with("https://auth.example.com/oauth/authorize?"));
                assert!(url.contains("client_id=plugin-client"));
                assert!(url.contains("code_challenge="));
                assert!(url.contains("code_challenge_method=S256"));
                assert!(url.contains(&format!("state={state}")));
                Ok(format!("plugin-code#{state}"))
            },
        )
        .await
        .expect("plugin code grant completes against the stub endpoint");

        assert_eq!(token.access, "at_plugin");
        assert_eq!(token.refresh.as_deref(), Some("rt_plugin"));

        let req = server.await.unwrap();
        assert!(req.contains("grant_type=authorization_code"));
        assert!(req.contains("code=plugin-code"));
        assert!(req.contains("code_verifier="));
        assert!(req.contains("client_id=plugin-client"));
    }

    /// C-08: the OAuth callback parser — happy path, provider error, and junk.
    #[test]
    fn parse_codex_callback_extracts_code_and_state() {
        let (code, state) =
            super::parse_codex_callback("code=abc%2F123&state=st8&scope=openid").unwrap();
        assert_eq!(code, "abc/123");
        assert_eq!(state, "st8");
        let err = super::parse_codex_callback("error=access_denied&state=st8").unwrap_err();
        assert!(err.to_string().contains("access_denied"));
        assert!(super::parse_codex_callback("foo=bar").is_err());
    }

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

    /// `build_datasources` ingests an `openapi` source (via the existing `ingest_openapi`) alongside a
    /// `markdown` one, so a declarative bot's help-center docs AND its OpenAPI spec are both searchable —
    /// the `flux app run` knowledge gap D-11 closes.
    #[tokio::test]
    async fn build_datasources_ingests_markdown_and_openapi_searchable() {
        use flux_datasource::SearchInput;
        use flux_lang::program::DatasourceDecl;
        use flux_system::{System, Workspace};

        let dir = std::env::temp_dir().join(format!("flux-ds-oa-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("guide.md"),
            "# Booking\nHow to book a widget appointment.",
        )
        .unwrap();
        std::fs::write(
            dir.join("api.json"),
            r#"{"openapi":"3.0.0","paths":{"/widgets":{"get":{"operationId":"listWidgets","summary":"List widgets"}}}}"#,
        )
        .unwrap();
        let system = System::new(Workspace::new(&dir).unwrap());

        let decls = vec![
            DatasourceDecl {
                name: "docs".into(),
                kind: "markdown".into(),
                path: Some(".".into()),
                settings: serde_json::Value::Null,
            },
            DatasourceDecl {
                name: "api".into(),
                kind: "openapi".into(),
                path: Some("api.json".into()),
                settings: serde_json::Value::Null,
            },
        ];
        let backend = build_datasources(&decls, &system).await.unwrap();

        // The markdown note is indexed as a `file.document`...
        let md = backend
            .search(&SearchInput {
                query: "book widget appointment".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(
            md.iter().any(|h| h.record.entity == "file.document"),
            "markdown ingested as a file.document record"
        );
        // ...and the OpenAPI operation as an `openapi.operation`.
        let oa = backend
            .search(&SearchInput {
                query: "list widgets".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(
            oa.iter().any(|h| h.record.entity == "openapi.operation"),
            "OpenAPI op ingested as an openapi.operation record"
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
            "flux-plugin-jira.exe", // a Windows binary — must be picked up, not skipped (D-47)
            "flux-plugin-slack.d",  // a cargo sidecar — must be skipped
            "flux-plugin-slack.exe.d", // a sidecar on a Windows-shaped name — must also be skipped
            "flux-plugin-",         // empty name — skipped
            "flux-plugin-.exe",     // empty name, `.exe` — skipped
            "not-a-plugin",         // wrong prefix — skipped
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let found = plugin_binaries_in(&dir).unwrap();
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["gitlab", "jira", "slack"]);
        // programs are absolute (canonicalized) paths to the binaries
        assert!(found.iter().all(|(_, p)| p.contains("flux-plugin-")));
        // the Windows binary's registered program path keeps the `.exe` suffix
        assert!(
            found
                .iter()
                .any(|(n, p)| n == "jira" && p.ends_with("flux-plugin-jira.exe")),
            "{found:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `flux plugin uninstall <name>` removes the descriptor; a missing name is a clean error
    /// (non-zero), never a panic (D-19).
    #[tokio::test]
    async fn plugin_uninstall_removes_descriptor() {
        let dir = std::env::temp_dir().join(format!("flux-uninstall-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        flux_plugin::add_descriptor(
            &dir,
            "p",
            &flux_plugin::PluginDescriptor {
                program: "/bin/true".into(),
                args: vec![],
                pinned: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            flux_plugin::discover(&dir).len(),
            1,
            "the descriptor is registered"
        );

        run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "p".into(),
                purge: false,
            }),
        )
        .await
        .unwrap();
        assert!(
            flux_plugin::discover(&dir).is_empty(),
            "uninstall removed the descriptor"
        );

        // A missing name is a clean error, not a panic.
        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "ghost".into(),
                purge: false,
            }),
        )
        .await;
        assert!(err.is_err(), "uninstall of a missing name is a clean error");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// N-003: `flux plugin install --dir` prunes a stale LOCAL descriptor whose binary is absent
    /// from the re-scanned dir (a partial pack build), but never touches a verified pack install or
    /// a plugin registered from elsewhere, and an empty scan prunes nothing.
    #[tokio::test]
    async fn plugin_install_dir_prunes_absent_local_descriptors() {
        let base =
            std::env::temp_dir().join(format!("flux-installdir-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let desc_dir = base.join("descriptors");
        let bin_dir = base.join("bin");
        std::fs::create_dir_all(&desc_dir).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();
        let write_bin =
            |name: &str| std::fs::write(bin_dir.join(format!("flux-plugin-{name}")), b"x").unwrap();
        write_bin("alpha");
        write_bin("beta");

        let install = |d: &std::path::Path| PluginAction::Install {
            names: vec![],
            all: false,
            dir: Some(d.to_string_lossy().into_owned()),
        };
        let names = |d: &std::path::Path| {
            let mut v: Vec<String> = flux_plugin::discover(d)
                .into_iter()
                .map(|p| p.name)
                .collect();
            v.sort();
            v
        };

        // First scan registers both local binaries.
        run_plugin_in(&desc_dir, Some(install(&bin_dir)))
            .await
            .unwrap();
        assert_eq!(names(&desc_dir), vec!["alpha", "beta"]);

        // A plugin `add`ed from elsewhere and a synthetic VERIFIED pack install — both must survive.
        flux_plugin::add_descriptor(
            &desc_dir,
            "gamma",
            &flux_plugin::PluginDescriptor {
                program: "/bin/true".into(),
                ..Default::default()
            },
        )
        .unwrap();
        flux_plugin::add_descriptor(
            &desc_dir,
            "delta",
            &flux_plugin::PluginDescriptor {
                program: bin_dir
                    .join("flux-plugin-delta")
                    .to_string_lossy()
                    .into_owned(),
                sha256: Some("deadbeef".into()),
                version: Some("1.0.0".into()),
                source: Some("plugins-v1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // `beta` fails to rebuild: its binary disappears from the scan dir.
        std::fs::remove_file(bin_dir.join("flux-plugin-beta")).unwrap();
        run_plugin_in(&desc_dir, Some(install(&bin_dir)))
            .await
            .unwrap();
        assert_eq!(
            names(&desc_dir),
            vec!["alpha", "delta", "gamma"],
            "absent local `beta` is pruned; alpha (present), gamma (elsewhere), delta (verified) \
             survive"
        );

        // An empty scan dir prunes NOTHING (a typo'd `--dir` can't wipe the set).
        let empty_dir = base.join("empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        run_plugin_in(&desc_dir, Some(install(&empty_dir)))
            .await
            .unwrap();
        assert_eq!(
            names(&desc_dir),
            vec!["alpha", "delta", "gamma"],
            "an empty scan prunes nothing"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// `flux plugin uninstall <name>` rejects a path-traversal name (non-zero) and deletes nothing
    /// outside the plugins dir (D-35). A name like `../../config` would otherwise `remove_file` a
    /// path outside `dir`.
    #[tokio::test]
    async fn plugin_uninstall_rejects_traversal_names() {
        let dir =
            std::env::temp_dir().join(format!("flux-uninstall-traversal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A sentinel file *outside* `dir`, reachable via `..`. An unsanitized `uninstall` would
        // delete `<dir>/../../flux-uninstall-traversal-sentinel.toml`.
        let outside = dir
            .parent()
            .unwrap()
            .join("flux-uninstall-traversal-sentinel.toml");
        std::fs::write(&outside, b"keep me").unwrap();

        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "../../flux-uninstall-traversal-sentinel".into(),
                purge: false,
            }),
        )
        .await;
        assert!(
            err.is_err(),
            "uninstall of a traversal name is a clean error, not a destructive delete"
        );
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "keep me",
            "the traversal name did not delete a file outside the plugins dir"
        );

        // An absolute name is also rejected.
        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "/etc/passwd".into(),
                purge: false,
            }),
        )
        .await;
        assert!(
            err.is_err(),
            "uninstall of an absolute name is a clean error"
        );

        std::fs::remove_file(&outside).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-48 acceptance: `status` re-hashes the binary against the descriptor's recorded sha256 —
    /// drift shows in the verification column (and the doomed liveness probe is skipped);
    /// a matching hash reports `Verified`; a hashless dev descriptor stays `UnverifiedLocal`.
    #[tokio::test]
    async fn status_reports_hash_drift() {
        let dir = std::env::temp_dir().join(format!("flux-status-drift-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("flux-plugin-alpha");
        std::fs::write(&bin, b"alpha-bytes").unwrap();
        let good = flux_plugin::pack::sha256_hex(b"alpha-bytes");
        flux_plugin::add_descriptor(
            &dir,
            "alpha",
            &flux_plugin::PluginDescriptor {
                program: bin.to_string_lossy().into_owned(),
                sha256: Some(good.clone()),
                version: Some("0.9.0".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // Untampered: verified. (The probe still runs and fails — a text file is no plugin —
        // but the verification column is independent of liveness.)
        let r = plugin_status_one(&dir, "alpha").await.unwrap();
        assert_eq!(r.verification, flux_plugin::Verification::Verified);

        // Tamper the binary → drift, and the spawn probe is refused/skipped.
        std::fs::write(&bin, b"tampered-bytes").unwrap();
        let r = plugin_status_one(&dir, "alpha").await.unwrap();
        match &r.verification {
            flux_plugin::Verification::HashDrift { expected, actual } => {
                assert_eq!(expected, &good);
                assert_eq!(actual, &flux_plugin::pack::sha256_hex(b"tampered-bytes"));
            }
            other => panic!("expected drift, got {other:?}"),
        }
        assert!(
            matches!(&r.liveness, Liveness::Unloadable(msg) if msg.contains("hash drift")),
            "drift refuses the probe: {:?}",
            r.liveness
        );

        // Hashless dev descriptor: unverified (local), exactly as before D-48.
        flux_plugin::add_descriptor(
            &dir,
            "dev",
            &flux_plugin::PluginDescriptor {
                program: bin.to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        let r = plugin_status_one(&dir, "dev").await.unwrap();
        assert_eq!(r.verification, flux_plugin::Verification::UnverifiedLocal);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-48 acceptance: `uninstall --purge` also removes the plugin's versioned-store directory;
    /// without `--purge` the store is left in place (unchanged pre-D-48 behavior).
    #[tokio::test]
    async fn uninstall_purge_removes_versioned_store() {
        let dir = std::env::temp_dir().join(format!("flux-uninst-purge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let seed = |name: &str| {
            let store = dir.join("bin").join(name).join("0.9.0");
            std::fs::create_dir_all(&store).unwrap();
            std::fs::write(store.join(format!("flux-plugin-{name}")), b"bytes").unwrap();
            flux_plugin::add_descriptor(
                &dir,
                name,
                &flux_plugin::PluginDescriptor {
                    program: "/bin/true".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        };

        // Without --purge: descriptor gone, store kept (unchanged behavior).
        seed("keep");
        run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "keep".into(),
                purge: false,
            }),
        )
        .await
        .unwrap();
        assert!(flux_plugin::load_descriptor(&dir, "keep")
            .unwrap()
            .is_none());
        assert!(
            dir.join("bin").join("keep").exists(),
            "store kept without --purge"
        );

        // With --purge: descriptor AND the versioned store dir are gone.
        seed("gone");
        run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "gone".into(),
                purge: true,
            }),
        )
        .await
        .unwrap();
        assert!(flux_plugin::load_descriptor(&dir, "gone")
            .unwrap()
            .is_none());
        assert!(
            !dir.join("bin").join("gone").exists(),
            "--purge removed the store"
        );

        // --purge on a name with no descriptor still cleans an orphaned store dir.
        let orphan = dir.join("bin").join("orphan").join("0.9.0");
        std::fs::create_dir_all(&orphan).unwrap();
        run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "orphan".into(),
                purge: true,
            }),
        )
        .await
        .unwrap();
        assert!(
            !dir.join("bin").join("orphan").exists(),
            "orphaned store purged"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `flux plugin status <name>` reports a registered-but-missing binary as `missing`, not a
    /// crash — and never spawns a process to find out (D-19).
    #[tokio::test]
    async fn plugin_status_reports_manifest_and_liveness() {
        let dir = std::env::temp_dir().join(format!("flux-status-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        flux_plugin::add_descriptor(
            &dir,
            "ghost",
            &flux_plugin::PluginDescriptor {
                program: "/nonexistent/binary".into(),
                args: vec![],
                pinned: None,
                ..Default::default()
            },
        )
        .unwrap();

        let r = plugin_status_one(&dir, "ghost").await.unwrap();
        assert_eq!(
            r.liveness,
            Liveness::Missing,
            "a missing binary is `missing`, not a crash"
        );
        assert!(
            r.manifest.is_none(),
            "no manifest is loaded for a missing binary"
        );

        // A name that is not registered at all is a clean error (the caller surfaces it).
        let err = plugin_status_one(&dir, "nope").await;
        assert!(err.is_err(), "an unknown name is a clean error");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Bare `flux plugin install` (no names, no `--all`, no `--dir`) is a clean error naming both
    /// modes — the pre-D-47 implicit default (`plugins/target/release`) no longer applies (clean
    /// cutover, no guessing).
    #[tokio::test]
    async fn plugin_install_bare_errors_naming_both_modes() {
        let dir = std::env::temp_dir().join(format!("flux-install-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Install {
                names: vec![],
                all: false,
                dir: None,
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--all"), "{msg}");
        assert!(msg.contains("--dir"), "{msg}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `--dir` (local scan) and explicit names/`--all` (remote install) are exclusive modes.
    #[tokio::test]
    async fn plugin_install_dir_rejects_combination_with_names_or_all() {
        let dir = std::env::temp_dir().join(format!("flux-install-combo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Install {
                names: vec!["gitlab".into()],
                all: false,
                dir: Some("plugins/target/release".into()),
            }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--dir"), "{err}");

        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Install {
                names: vec![],
                all: true,
                dir: Some("plugins/target/release".into()),
            }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--dir"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `flux plugin install --dir <path>` (the pre-D-47 local scan) registers a hashless descriptor
    /// — `ls`/`status` label it `unverified (local)`, never `verified`.
    #[tokio::test]
    async fn plugin_install_dir_scan_registers_unverified_local_descriptor() {
        let dir = std::env::temp_dir().join(format!("flux-install-dirscan-{}", std::process::id()));
        let bin_dir = dir.join("bin");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("flux-plugin-gitlab"), b"x").unwrap();

        run_plugin_in(
            &dir,
            Some(PluginAction::Install {
                names: vec![],
                all: false,
                dir: Some(bin_dir.to_string_lossy().into_owned()),
            }),
        )
        .await
        .unwrap();

        let desc = flux_plugin::load_descriptor(&dir, "gitlab")
            .unwrap()
            .unwrap();
        assert!(
            desc.version.is_none(),
            "a local-scan descriptor carries no version"
        );
        assert!(
            desc.sha256.is_none(),
            "a local-scan descriptor carries no sha256"
        );
        assert_eq!(
            flux_plugin::verify_descriptor(&desc),
            flux_plugin::Verification::UnverifiedLocal
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-48 superseded D-47's descriptor-field-only `verified` label: a hash-carrying descriptor
    /// is now **re-hashed** — one whose binary cannot be read is drift (never a silent
    /// `verified`), and a hashless (local/dev) one stays `unverified (local)`.
    #[tokio::test]
    async fn plugin_status_rehashes_hash_carrying_descriptors() {
        let dir = std::env::temp_dir().join(format!("flux-status-verified-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        flux_plugin::add_descriptor(
            &dir,
            "remote-plugin",
            &flux_plugin::PluginDescriptor {
                program: "/nonexistent/remote-plugin".into(),
                version: Some("0.9.0".into()),
                sha256: Some("deadbeef".into()),
                source: Some("plugins-v0.9.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
        flux_plugin::add_descriptor(
            &dir,
            "local-plugin",
            &flux_plugin::PluginDescriptor {
                program: "/nonexistent/local-plugin".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let remote = plugin_status_one(&dir, "remote-plugin").await.unwrap();
        assert!(
            matches!(
                &remote.verification,
                flux_plugin::Verification::HashDrift { expected, .. } if expected == "deadbeef"
            ),
            "a recorded hash over an unreadable binary is drift, not verified: {:?}",
            remote.verification
        );
        assert_eq!(remote.version.as_deref(), Some("0.9.0"));

        let local = plugin_status_one(&dir, "local-plugin").await.unwrap();
        assert_eq!(
            local.verification,
            flux_plugin::Verification::UnverifiedLocal,
            "a hashless descriptor is unverified (local)"
        );
        assert!(local.version.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plugin_call_resolves_short_op_to_manifest_qualified_name() {
        let manifest = flux_plugin::PluginManifest {
            name: "grafana".into(),
            operations: vec![flux_plugin::OperationSpec {
                name: "grafana.search".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            resolve_plugin_operation_name("grafana", "search", &manifest).unwrap(),
            "grafana.search"
        );
    }

    #[test]
    fn plugin_call_preserves_explicit_fully_qualified_op() {
        let manifest = flux_plugin::PluginManifest {
            name: "grafana".into(),
            operations: vec![flux_plugin::OperationSpec {
                name: "grafana.search".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            resolve_plugin_operation_name("grafana", "grafana.search", &manifest).unwrap(),
            "grafana.search"
        );
    }

    #[test]
    fn plugin_call_unknown_op_lists_available_ops() {
        let manifest = flux_plugin::PluginManifest {
            name: "grafana".into(),
            operations: vec![flux_plugin::OperationSpec {
                name: "grafana.search".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = resolve_plugin_operation_name("grafana", "dashboards", &manifest)
            .unwrap_err()
            .to_string();
        assert!(err.contains("tried `grafana.dashboards`"), "{err}");
        assert!(err.contains("grafana.search"), "{err}");
    }

    // ─── Track A1: `flux plugin call/run --arg` schema-coerced input building ──────────

    /// A representative schemars-derived op schema (a string field, a required integer, a
    /// nullable boolean, an enum, a string-array, and an unknown/extra field path).
    fn sample_op_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "count": {"type": "integer"},
                "flag": {"type": ["boolean", "null"]},
                "mode": {"type": "string", "enum": ["a", "b"]},
                "tags": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["count"]
        })
    }

    #[test]
    fn build_invoke_input_coerces_arg_types() {
        let schema = sample_op_schema();
        let args = vec![
            "count=42".to_string(),
            "flag=true".to_string(),
            "mode=b".to_string(),
            "tags=foo,bar,baz".to_string(),
        ];
        let (input, problems) = build_invoke_input(&schema, None, &args, true);
        assert!(problems.is_empty(), "problems: {problems:?}");
        assert_eq!(input["count"], 42);
        assert_eq!(input["flag"], true);
        assert_eq!(input["mode"], "b");
        assert_eq!(input["tags"], serde_json::json!(["foo", "bar", "baz"]));
    }

    #[test]
    fn build_invoke_input_reports_type_and_enum_and_required_problems() {
        let schema = sample_op_schema();
        let args = vec![
            "count=notanint".to_string(),
            "mode=zzz".to_string(),
            "unknownfield=x".to_string(),
        ];
        let (input, problems) = build_invoke_input(&schema, None, &args, true);
        // Required `count` is present (as a string fallback), so only the coercion/enum/unknown
        // problems fire — not the missing-required one.
        assert_eq!(problems.len(), 3, "problems: {problems:?}");
        assert!(problems
            .iter()
            .any(|p| p.contains("`count`") && p.contains("integer")));
        assert!(problems
            .iter()
            .any(|p| p.contains("`mode`") && p.contains("not one of")));
        assert!(problems
            .iter()
            .any(|p| p.contains("`unknownfield`") && p.contains("not a declared field")));
        // The count fallback is inserted as a string so the call can still proceed under --no-validate.
        assert_eq!(input["count"], "notanint");
    }

    #[test]
    fn build_invoke_input_flags_missing_required() {
        let schema = sample_op_schema();
        let (input, problems) = build_invoke_input(&schema, None, &[], true);
        assert_eq!(input, serde_json::json!({}));
        assert!(problems
            .iter()
            .any(|p| p.contains("missing required field `count`")));
    }

    #[test]
    fn build_invoke_input_merges_args_over_json_base() {
        let schema = sample_op_schema();
        let base = serde_json::json!({"count": 1, "name": "base"});
        let args = vec!["count=99".to_string(), "flag=false".to_string()];
        let (input, problems) = build_invoke_input(&schema, Some(base), &args, true);
        assert!(problems.is_empty(), "problems: {problems:?}");
        assert_eq!(input["count"], 99); // arg overrides base
        assert_eq!(input["name"], "base"); // base preserved
        assert_eq!(input["flag"], false);
    }

    #[test]
    fn build_invoke_input_no_validate_passes_strings_through() {
        let schema = sample_op_schema();
        let args = vec!["count=notanint".to_string(), "unknownfield=x".to_string()];
        let (input, problems) = build_invoke_input(&schema, None, &args, false);
        assert!(
            problems.is_empty(),
            "--no-validate should produce no problems: {problems:?}"
        );
        assert_eq!(input["count"], "notanint");
        assert_eq!(input["unknownfield"], "x");
    }

    #[test]
    fn build_invoke_input_parses_json_array_literal() {
        let schema = sample_op_schema();
        let args = vec!["count=1".to_string(), "tags=[\"x\",\"y\"]".to_string()];
        let (input, problems) = build_invoke_input(&schema, None, &args, true);
        assert!(problems.is_empty(), "problems: {problems:?}");
        assert_eq!(input["tags"], serde_json::json!(["x", "y"]));
    }

    #[test]
    fn coerce_arg_value_handles_nullable_and_refs() {
        // schemars nullable form: type: ["string","null"].
        let nullable = serde_json::json!({"type": ["string", "null"]});
        assert_eq!(
            coerce_arg_value(&nullable, &serde_json::json!({}), "hi").unwrap(),
            "hi"
        );
        // enum via anyOf → $ref → definitions (schemars Option<Enum> shape).
        let schema = serde_json::json!({
            "definitions": { "Mode": {"type": "string", "enum": ["on", "off"]} },
            "anyOf": [{"$ref": "#/definitions/Mode"}, {"type": "null"}]
        });
        let defs = schema["definitions"].clone();
        assert_eq!(coerce_arg_value(&schema, &defs, "on").unwrap(), "on");
        let err = coerce_arg_value(&schema, &defs, "nope").unwrap_err();
        assert!(err.to_string().contains("not one of"));
    }

    #[test]
    fn generated_skill_install_writes_skill_dir_and_references() {
        let root = std::env::temp_dir().join(format!("flux-skill-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let skill = super::skill_cmd::RenderedSkill {
            name: "flux-test".into(),
            skill_md: "---\nname: flux-test\ndescription: test\n---\nbody\n".into(),
            references: vec![("ops".into(), "# Ops\n".into())],
        };

        let dir = write_generated_skill(&root, &skill).unwrap();
        assert_eq!(dir, root.join("flux-test"));
        assert!(dir.join("SKILL.md").is_file());
        assert!(dir.join("references").join("ops.md").is_file());

        std::fs::remove_dir_all(&root).ok();
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
            ..Default::default()
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

    /// C-06 cache-aware surfacing: `usage_annotation` must show cache-WRITE tokens and reasoning
    /// tokens too — before C-06 only cache-READ appeared, silently dropping the other tiers a
    /// caching-heavy or reasoning-heavy turn actually spent. Combined with `cost_annotation` (the
    /// dollar-cost suffix `CliSink::cost_inline` appends alongside this), the turn-end rule shows
    /// every tier + cost — the story's named failing-first test.
    #[test]
    fn usage_annotation_includes_cache_and_cost() {
        use flux_core::{Money, Usage};

        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_creation_input_tokens: 2_000,
            cache_read_input_tokens: 9_000,
            reasoning_tokens: 300,
            ..Default::default()
        };
        let s = usage_annotation(&u);
        assert!(s.contains("cache 9.0k"), "cache-read still shown: {s}");
        assert!(
            s.contains("cache write 2.0k"),
            "cache-WRITE tokens must be surfaced too (previously dropped entirely): {s}"
        );
        assert!(
            s.contains("reasoning 300"),
            "reasoning tokens must be surfaced: {s}"
        );

        // Zero cache-write / zero reasoning ⇒ neither segment appears (no clutter on an ordinary
        // metered turn that never wrote to cache or reasoned).
        let plain = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_input_tokens: 9_000,
            ..Default::default()
        };
        let s2 = usage_annotation(&plain);
        assert!(!s2.contains("cache write"));
        assert!(!s2.contains("reasoning"));

        // The dollar-cost suffix (rendered alongside, via `cost_annotation`) completes the picture:
        // the turn-end rule shows tokens (this function) AND cost (this one) together.
        let cost = cost_annotation(&Money {
            usd: 0.0456,
            subscription: false,
            source: flux_core::CostSource::Estimated,
        });
        assert_eq!(format!("{s}{cost}"), format!("{s} · $0.0456"));
    }

    /// `cost_annotation` formats metered spend as `$X`, subscription spend (claude/codex) as the
    /// *equivalent metered cost* `~$X (sub)` (it bills against a flat sub, not the API), and a
    /// zero-cost turn as empty (C-05).
    #[test]
    fn cost_annotation_labels_metered_vs_subscription() {
        use flux_core::Money;
        // Metered spend → raw dollar amount.
        let metered = cost_annotation(&Money {
            usd: 0.0023,
            subscription: false,
            source: flux_core::CostSource::Estimated,
        });
        assert_eq!(metered, " · $0.0023");
        // Subscription spend → equivalent metered cost, tagged `(sub)`.
        let sub = cost_annotation(&Money {
            usd: 0.0023,
            subscription: true,
            source: flux_core::CostSource::Estimated,
        });
        assert_eq!(sub, " · ~$0.0023 (sub)");
        // A zero-cost turn (e.g. fully cached, or no usage) → empty, so the rule stays clean.
        assert_eq!(
            cost_annotation(&Money {
                usd: 0.0,
                subscription: false,
                source: flux_core::CostSource::Estimated,
            }),
            ""
        );
        assert_eq!(
            cost_annotation(&Money {
                usd: 0.0,
                subscription: true,
                source: flux_core::CostSource::Estimated,
            }),
            ""
        );
    }

    /// `flux usage` reports per-model tokens + cost for the current (latest) session AND an
    /// all-sessions total — the story's named failing-first test. Two sessions on different models,
    /// each with a `CallUsage`-carrying turn: the latest session's report must show ONLY its own
    /// model, while the all-sessions total rolls up both.
    #[test]
    fn flux_usage_reports_per_model_cost() {
        use flux_core::Usage;

        let store = EventStore::in_memory().unwrap();

        let older = store.create_session("claude-opus-4-8").unwrap();
        let t1 = store
            .begin_turn(&older, "first", "claude-opus-4-8")
            .unwrap();
        store
            .record_call_usage(
                &older,
                t1,
                "claude-opus-4-8",
                Usage {
                    input_tokens: 1_000_000,
                    output_tokens: 1_000_000,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&older, t1, "accepted", 1, "done", None)
            .unwrap();

        let latest = store.create_session("claude-sonnet-4-6").unwrap();
        let t2 = store
            .begin_turn(&latest, "second", "claude-sonnet-4-6")
            .unwrap();
        store
            .record_call_usage(
                &latest,
                t2,
                "claude-sonnet-4-6",
                Usage {
                    input_tokens: 500_000,
                    output_tokens: 50_000,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&latest, t2, "accepted", 1, "done", None)
            .unwrap();

        assert_eq!(
            store.latest_session().unwrap().as_deref(),
            Some(latest.as_str()),
            "the second session is the most recently active"
        );

        let pricing = flux_core::PricingTable::builtin();
        // Doesn't panic and (indirectly, via the projection it wraps) reports the right rows —
        // asserted precisely below through the projection it's built on, since `run_usage_with`
        // itself only prints.
        run_usage_with(&store, &pricing).unwrap();

        // The precise per-model figures `run_usage_with` prints, checked directly:
        let latest_rows = store.cost_summary(&latest, &pricing).unwrap();
        assert_eq!(
            latest_rows.len(),
            1,
            "the latest session shows only its own model"
        );
        assert_eq!(latest_rows[0].model, "claude-sonnet-4-6");
        assert_eq!(latest_rows[0].usage.input_tokens, 500_000);

        let all_rows = store.cost_summary_all(&pricing).unwrap();
        assert_eq!(
            all_rows.len(),
            2,
            "the all-sessions total rolls up both models"
        );
        let opus = all_rows
            .iter()
            .find(|r| r.model == "claude-opus-4-8")
            .unwrap();
        assert_eq!(opus.usage.input_tokens, 1_000_000);
        assert!(opus.cost.unwrap().usd > 0.0);
    }

    /// D-53's failing-first test: `flux corpus export` over a seeded events.db with two accepted
    /// plans — one carrying `plan_source` (L-38), one in the pre-L-38 shape (no `plan_source`) —
    /// exports exactly the ONE qualifying row, paired with its OWN turn's user instruction, in the
    /// documented JSONL shape, and counts (never silently drops) the row it skipped.
    #[test]
    fn flux_corpus_export_pairs_accepted_plan_with_its_turn_and_skips_pre_l38_row() {
        let store = EventStore::in_memory().unwrap();
        let session = store.create_session("m").unwrap();

        // Pre-L-38 accepted plan: no plan_source recorded — must be skipped and counted, not paired.
        let t1 = store
            .begin_turn(&session, "old-style request", "m")
            .unwrap();
        store
            .record_plan_attempt(
                &session,
                t1,
                flux_events::PlanAttempt {
                    step: 0,
                    outcome: "accepted".into(),
                    plan_text: Some("$x = read(\"a\")".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&session, t1, "accepted", 1, "done", None)
            .unwrap();

        // A real L-38 accepted plan: must export, paired with THIS turn's own user_input. The
        // instruction carries a credential-shaped token (raw `user_input` is NOT redacted at
        // record time — only the agent's own outputs are) to exercise the export-time nl_goal scrub.
        let t2 = store
            .begin_turn(
                &session,
                "summarize the README using key AKIAABCDEFGHIJKLMNOP",
                "m",
            )
            .unwrap();
        store
            .record_plan_attempt(
                &session,
                t2,
                flux_events::PlanAttempt {
                    step: 0,
                    outcome: "accepted".into(),
                    phase: Some("execute".into()),
                    plan_source: Some("flow\n  $x = read(\"README.md\")".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&session, t2, "accepted", 1, "done", None)
            .unwrap();

        // Cross-check against the underlying projection (already unit-tested in flux-events) so this
        // test's expectations about *which* row qualifies don't drift from it.
        let (expected_rows, _) = store.corpus_rows_all().unwrap();
        assert_eq!(
            expected_rows.len(),
            1,
            "one row qualifies before export-time re-parsing: {expected_rows:?}"
        );

        let mut buf: Vec<u8> = Vec::new();
        let summary = run_corpus_export_with(&store, "test-rev", &mut buf).unwrap();

        assert_eq!(summary.exported, 1, "{summary:?}");
        assert_eq!(
            summary.no_plan_source, 1,
            "the pre-L-38 row is counted, not silently dropped"
        );
        assert_eq!(summary.ambiguous_pairing, 0);
        assert_eq!(summary.unparseable_at_head, 0);

        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 1, "exactly one exported JSONL row: {text:?}");

        let row: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(row["id"], expected_rows[0].id);
        assert_eq!(
            row["nl_goal"], "summarize the README using key [redacted]",
            "the credential-shaped token is scrubbed from the raw user_input at export time"
        );
        assert_eq!(row["source"], "flow\n  $x = read(\"README.md\")");
        assert_eq!(row["provenance"]["session"], session);
        assert_eq!(row["provenance"]["turn"], t2);
        assert_eq!(row["flux_rev"], "test-rev");
    }

    /// A `CliSink` with an attached model spec + pricing table prices a turn's usage through the
    /// cost model end-to-end (the wiring that makes C-05's `cost()` live, not dead code). The codex
    /// path resolves on `gpt-5.5` and is labelled subscription spend (C-03 model resolution + C-05).
    #[tokio::test]
    async fn sink_prices_a_codex_turn_as_subscription() {
        use flux_core::Usage;
        let sink = super::CliSink::new(0).with_cost(
            "codex/gpt-5.5".to_string(),
            flux_core::PricingTable::builtin(),
        );
        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        };
        let inline = sink.cost_inline(Some(&u));
        assert!(
            inline.contains("(sub)"),
            "codex spend is subscription-labelled, got: {inline}"
        );
        assert!(
            inline.contains('$'),
            "a non-zero turn shows a dollar cost, got: {inline}"
        );
        // A metered spec on the same usage is not tagged `(sub)`.
        let metered = super::CliSink::new(0)
            .with_cost(
                "anthropic/claude-sonnet-4-6".to_string(),
                flux_core::PricingTable::builtin(),
            )
            .cost_inline(Some(&u));
        assert!(
            !metered.contains("(sub)"),
            "anthropic is metered, got: {metered}"
        );
        // No spec attached → no cost suffix (sub-paths that don't show cost).
        assert_eq!(super::CliSink::new(0).cost_inline(Some(&u)), "");
    }

    /// C-30: an attached METERED CLOUD model missing from the pricing table renders the visible
    /// ` · $? (unpriced)` marker — never silent nothing (silence hid real spend); local
    /// (`ollama*`) and unknown/mock specs stay silent so hermetic e2e output is byte-identical.
    #[tokio::test]
    async fn unpriced_model_renders_visible_marker() {
        use flux_core::Usage;
        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        };
        let table = flux_core::PricingTable::builtin();
        // A cloud provider with a model the table doesn't know → marker.
        let unpriced = super::CliSink::new(0)
            .with_cost("openrouter/acme/not-in-table".into(), table.clone())
            .cost_inline(Some(&u));
        assert_eq!(unpriced, " · $? (unpriced)", "got: {unpriced:?}");
        // Local ollama and unknown/mock specs: silent, as before.
        for quiet in ["ollama/llama3", "mock", "some-ad-hoc-model"] {
            let s = super::CliSink::new(0)
                .with_cost(quiet.into(), table.clone())
                .cost_inline(Some(&u));
            assert_eq!(s, "", "`{quiet}` must stay silent, got: {s:?}");
        }
        // No usage → silent regardless.
        let none = super::CliSink::new(0)
            .with_cost("openrouter/acme/not-in-table".into(), table)
            .cost_inline(None);
        assert_eq!(none, "");
    }

    /// C-34: a call that reported its own cost (OpenRouter, both wires) prices even though the
    /// static builtin table has no row for it — the `$? (unpriced)` marker (and its once-per-run
    /// note) must NOT fire; `cost_suffix` takes the `Some(money) => cost_annotation` branch, never
    /// reaching `unpriced_marker_applies`/`note_unpriced_once` at all.
    #[tokio::test]
    async fn cost_suffix_prefers_reported_cost_over_unpriced_marker() {
        use flux_core::Usage;
        let u = Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            reported_cost_usd: Some(0.0023),
            ..Default::default()
        };
        let table = flux_core::PricingTable::builtin();
        let inline = super::CliSink::new(0)
            .with_cost("openrouter/deepseek/deepseek-v4-flash:nitro".into(), table)
            .cost_inline(Some(&u));
        assert!(
            !inline.contains("$?"),
            "reported cost must beat the unpriced marker, got: {inline:?}"
        );
        assert_eq!(inline, " · $0.0023", "the real reported figure, not $?");
    }

    /// C-30: the REPL's per-turn sink derives its spec from the LIVE engine — the same
    /// `canonical_model_spec` derivation loop_host stamps usage with — so a `/model` switch
    /// changes what the next sink prices, and an openrouter passthrough keeps its serving
    /// provider (metered), while a claude switch turns the suffix subscription-shaped.
    #[tokio::test]
    async fn repl_sink_cost_derives_from_the_live_engine_spec() {
        use flux_core::Usage;
        let u = Usage {
            input_tokens: 100_000,
            output_tokens: 5_000,
            ..Default::default()
        };
        let table = flux_core::PricingTable::builtin();
        // The derivation the TurnCost factory applies (provider name + live model string):
        let spec = flux_core::canonical_model_spec(
            Some("openrouter-anthropic"),
            "anthropic/claude-sonnet-4.6",
        );
        assert_eq!(spec, "openrouter-anthropic/anthropic/claude-sonnet-4.6");
        let inline = super::CliSink::new(0)
            .with_cost(spec, table.clone())
            .cost_inline(Some(&u));
        assert!(
            inline.contains('$') && !inline.contains("(sub)") && !inline.contains("$?"),
            "openrouter passthrough is metered and priced, got: {inline}"
        );
        // Simulated /model switch to a subscription provider: the NEXT sink derives the new spec.
        let spec = flux_core::canonical_model_spec(Some("claude"), "claude-opus-4-8");
        let inline = super::CliSink::new(0)
            .with_cost(spec, table)
            .cost_inline(Some(&u));
        assert!(
            inline.contains("(sub)"),
            "a switched-to claude model is subscription-labelled, got: {inline}"
        );
    }

    /// A-15 named acceptance (`phase_observations_emitted_per_pass`'s surface half): each
    /// `loop.phase` observation updates the phase-labeled spinner — "orienting…"/"gathering…" for
    /// the collect passes, "planning…" for the execute phase's first round this turn, and
    /// "revising…" once the execute phase has already produced a round (no `--show-loop` needed —
    /// this is the spinner, not the machinery). A phase-less turn (no `loop.phase` observed at
    /// all, e.g. the `/plan` REPL path) keeps today's "composing plan…".
    #[test]
    fn loop_phase_observations_drive_the_phase_labeled_spinner() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "composing plan…",
            "no loop.phase observed yet -> byte-compatible fallback"
        );

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "orient" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "orienting…"
        );
        assert!(!sink.gather_mode);

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "gather" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "gathering…"
        );
        assert!(sink.gather_mode, "a gather-phase round renders compact");

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "execute" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "planning…",
            "the execute phase's first round this turn is a plain plan, not a revision"
        );
        assert!(!sink.gather_mode, "execute is never a gather round");

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "execute" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "revising…",
            "a second execute-phase round this turn means the prior one didn't finish"
        );
    }

    /// A-15: a `flow.brief` observation marks gather mode (a brief only ever accompanies a
    /// `gather: true` plan, per `compile.rs`'s `parse_brief` call site) even when it arrives right
    /// after `orient` — the only phase where a gather round is otherwise indistinguishable from a
    /// full plan emitted directly. `brief_lines` renders the grounding artifact immediately and
    /// compactly: `◆ goal: …` plus a dim needs list.
    #[test]
    fn flow_brief_observation_marks_gather_mode_and_formats_goal_and_needs() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "orient" }),
        ));
        assert!(!sink.gather_mode);

        sink.observation(&Observation::new(
            "flow.brief",
            Phase::Turn,
            serde_json::json!({ "goal": "find the bug", "needs": ["stack trace", "repro steps"] }),
        ));
        assert!(
            sink.gather_mode,
            "the brief that just landed accompanies orient's gather plan"
        );

        let lines = super::brief_lines(&serde_json::json!({
            "goal": "find the bug",
            "needs": ["stack trace", "repro steps"],
        }));
        assert_eq!(lines[0], "◆ goal: find the bug");
        assert_eq!(lines[1], "  needs: stack trace, repro steps");

        // No needs -> just the goal line (an empty needs list adds no clutter).
        let goal_only = super::brief_lines(&serde_json::json!({ "goal": "answer a question" }));
        assert_eq!(goal_only, vec!["◆ goal: answer a question".to_string()]);
    }

    /// A-15: a gather plan (small, read-only) renders as a compact one-liner — op names pulled off
    /// the plan's call nodes, joined `·`-separated after a `gathering` label — never the full tree
    /// + risk badge a full execution plan keeps (`render_plan`, unchanged by this story).
    #[test]
    fn gather_plan_renders_as_a_compact_one_liner_not_the_full_tree() {
        use flux_flow::ast::{DraftAst, Node};

        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "a".into(),
                    value: Box::new(Node::Call {
                        op: "read".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!({ "path": "Cargo.toml" }),
                        }],
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Call {
                    op: "grep".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!({ "pattern": "LoopHost" }),
                    }],
                },
            ],
            ..Default::default()
        };
        let data = serde_json::json!({
            "plan_ast": serde_json::to_value(&ast).unwrap(),
            "plan": "flow\n└─ ...",
            "risk": "low",
            "ops": 2,
        });
        let line = super::gather_compact_line(&data);
        assert!(
            line.starts_with("gathering · "),
            "compact one-liner, not a tree: {line}"
        );
        assert!(line.contains("read"), "op names: {line}");
        assert!(line.contains("Cargo.toml"), "and their args: {line}");
        assert!(line.contains("grep"), "every call node listed: {line}");
        assert!(
            !line.contains('\n'),
            "one line, not the multi-line tree render: {line}"
        );

        // An AST-less payload (defensive) falls back to a bare op count rather than panicking.
        let bare = super::gather_compact_line(&serde_json::json!({ "ops": 3 }));
        assert_eq!(bare, "gathering · 3 ops");
    }

    /// A-15: the `flow.plan` dispatch itself — `observation()` picks the compact render while
    /// `gather_mode` is set (entered via a `gather`-phase `loop.phase`) and the full tree once
    /// `execute` clears it back. This only smoke-tests that both paths run without panicking (the
    /// terminal painting itself goes straight to stderr, like every other `CliSink` render in this
    /// file); the render CONTENT is covered by `gather_compact_line` above and the pre-existing
    /// `flow.plan` full-tree behavior this story leaves untouched.
    #[test]
    fn flow_plan_dispatches_compact_or_full_by_gather_mode() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        let plan_data = serde_json::json!({
            "plan": "flow\n└─ $x = read(\"README.md\")   !read",
            "risk": "low",
            "ops": 1,
        });

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "gather" }),
        ));
        assert!(sink.gather_mode);
        sink.observation(&Observation::new(
            "flow.plan",
            Phase::Turn,
            plan_data.clone(),
        ));

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "execute" }),
        ));
        assert!(!sink.gather_mode);
        sink.observation(&Observation::new("flow.plan", Phase::Turn, plan_data));
    }

    /// A-17 (closes the A-15 residual): `flow.plan`'s own `gather` field is honored directly, even
    /// when it DISAGREES with the surface's tracked `gather_mode` state — this is exactly the gap
    /// A-15 recorded (an orient-phase gather plan the state machine couldn't tell apart from orient
    /// emitting the full plan directly). The direct field must win.
    #[test]
    fn flow_plan_gather_field_is_honored_directly_even_when_state_inference_disagrees() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        // `orient` clears the surface's own `gather_mode` inference to false...
        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "orient" }),
        ));
        assert!(!sink.gather_mode);
        // ...but the plan itself says otherwise (`gather: true`) — the direct field must be
        // consulted at dispatch time, not the stale inferred state. Smoke-tests only that the
        // gather branch runs without panicking; content is covered by `gather_compact_line`.
        sink.observation(&Observation::new(
            "flow.plan",
            Phase::Turn,
            serde_json::json!({ "plan": "flow\n└─ ...", "risk": "low", "ops": 1, "gather": true }),
        ));

        // A payload with NO `gather` field at all (a phase-less/stale caller) falls back to the
        // tracked state — backward compatible with the pre-A-17 wire shape.
        sink.observation(&Observation::new(
            "flow.plan",
            Phase::Turn,
            serde_json::json!({ "plan": "flow\n└─ ...", "risk": "low", "ops": 1 }),
        ));
    }

    /// A-17: `halt_line` formats a `flow.halt` observation's `data` as the design's `✗ step N/M <op>
    /// failed — revising…` line, falling back to a plain "failed" when the op isn't derivable.
    #[test]
    fn flow_halt_observation_renders_the_step_and_op() {
        let with_op = super::halt_line(&serde_json::json!({ "step": 4, "of": 9, "op": "edit" }));
        assert_eq!(with_op, "✗ step 4/9 edit failed — revising…");

        let without_op = super::halt_line(&serde_json::json!({ "step": 2, "of": 2 }));
        assert_eq!(without_op, "✗ step 2/2 failed — revising…");
    }

    /// A-17: `render_halt`'s dispatch — smoke-tests that a `flow.halt` observation reaches the
    /// sink without panicking (the rendered CONTENT is covered by `halt_line` above).
    #[test]
    fn flow_halt_dispatches_to_render_halt() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        sink.observation(&Observation::new(
            "flow.halt",
            Phase::Turn,
            serde_json::json!({ "step": 1, "of": 2, "op": "boom", "kind": "runtime", "fatal": false }),
        ));
    }

    /// A-39 (`--trace-loop`/`FLUX_TRACE_LOOP`): `trace_node_line` formats every structural `loop.node`
    /// kind the interpreter can emit, table-driven like `halt_line`'s test above — including the
    /// defensive fallback for a `node` kind this formatter hasn't been taught yet.
    #[test]
    fn trace_node_line_formats_every_structural_kind() {
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (
                serde_json::json!({"node": "call", "op": "plan", "bind": "draft"}),
                "· plan → $draft",
            ),
            (serde_json::json!({"node": "call", "op": "grep"}), "· grep"),
            (
                serde_json::json!({"node": "when", "cond": "$draft", "branch": "then"}),
                "· when $draft → then",
            ),
            (
                serde_json::json!({"node": "when", "branch": "else"}),
                "· when → else",
            ),
            (
                serde_json::json!({"node": "unless", "cond": "$done", "entered": false}),
                "· unless $done → skip",
            ),
            (
                serde_json::json!({"node": "unless", "entered": true}),
                "· unless → enter",
            ),
            (
                serde_json::json!({
                    "node": "match",
                    "subject": "$kind",
                    "value": "\"chat\"",
                    "arm": "case \"chat\"",
                }),
                "· match $kind = \"chat\" → case \"chat\"",
            ),
            (
                serde_json::json!({"node": "match", "value": "1", "arm": "default"}),
                "· match 1 → default",
            ),
            (
                serde_json::json!({"node": "return", "value": "$answer"}),
                "· return $answer",
            ),
            (serde_json::json!({"node": "return"}), "· return"),
            (
                serde_json::json!({"node": "repeat", "until_hit": true, "rounds": 3, "max": 25}),
                "· until hit — exit after 3/25",
            ),
            (
                serde_json::json!({"node": "parallel.branch", "name": "left"}),
                "· parallel branch $left",
            ),
        ];
        for (data, expected) in cases {
            assert_eq!(super::trace_node_line(&data), expected, "data: {data}");
        }

        // An unrecognized `node` kind falls back to the raw JSON rather than panicking (defensive:
        // the interpreter's trace helper is meant to grow new emission sites over time).
        let unknown = serde_json::json!({"node": "each", "foo": "bar"});
        assert_eq!(super::trace_node_line(&unknown), format!("· {unknown}"));
    }

    /// A-39: `loop.round`/`loop.node` observations dispatch without panicking (the rendered CONTENT
    /// is covered by `trace_node_line` above).
    #[test]
    fn loop_round_and_node_dispatch_without_panicking() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        sink.observation(&Observation::new(
            "loop.round",
            Phase::Turn,
            serde_json::json!({ "round": 1, "max": 25 }),
        ));
        sink.observation(&Observation::new(
            "loop.node",
            Phase::Turn,
            serde_json::json!({ "node": "call", "op": "plan", "bind": "draft" }),
        ));
    }

    /// A-17: a resumed/halted plan's marker-prefixed text is colored per line (✓ green / ✗ red / ·
    /// dim) rather than left plain — the CLI/TUI residual this story closes (the `flow.plan`
    /// observation carries markers, but the surface used to always reconstruct an unmarked tree
    /// from `plan_ast` instead of rendering them).
    #[test]
    fn style_marked_plan_colors_each_line_by_its_status_marker() {
        // Color is off by default in tests (no tty) — style::* helpers no-op, so this proves the
        // per-line DISPATCH logic (which marker maps to which styler) without depending on a tty.
        let text = "✓ 0: $a = echo(\"first\")\n✗ 1: boom()\n· 2: $b = echo(\"fixed\")";
        let styled = super::style_marked_plan(text);
        // With color disabled the bytes are unchanged, but every line must still be present in
        // order (the function must not drop or reorder lines).
        for line in text.lines() {
            assert!(styled.contains(line), "{styled}");
        }
        assert_eq!(styled.lines().count(), 3);
    }

    /// A-17: `render_plan`'s dispatch — a `resumed: true` payload prefers the marked `plan` text
    /// (smoke-tested; content covered by `style_marked_plan`), a normal payload still prefers
    /// `plan_ast` (pre-existing behavior, unchanged).
    #[test]
    fn render_plan_prefers_marked_text_when_resumed() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        sink.observation(&Observation::new(
            "flow.plan",
            Phase::Turn,
            serde_json::json!({
                "plan": "✓ 0: $a = echo(\"first\")\n✗ 1: boom()",
                "plan_ast": {"body": [
                    {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"first"}]}},
                    {"kind":"call","op":"boom","args":[]}
                ]},
                "risk": "low",
                "ops": 2,
                "resumed": true,
            }),
        ));
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
            "app",
            "eval",
            "flow",
            "review",
            "loop",
            "sessions",
            "auth",
            "plugin",
            "skill",
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
    /// the promoted mode flags (`tui`/`plan`) leak onto it — they live on the subcommands now (`--serve`
    /// likewise lives on `app run`, never the top level). Inspecting the declared arguments (not the
    /// rendered text) avoids false hits on flag names that appear inside a subcommand's *description*.
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

    /// `flux skill` is the generated-skill surface: optional type plus install/global flags.
    #[test]
    fn skill_help_documents_types_and_install_flags() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let skill = cmd.find_subcommand("skill").expect("skill subcommand");
        let help = skill.clone().render_long_help().to_string();
        for want in ["--install", "--global", "cli", "lang", "plugin", "ops"] {
            assert!(help.contains(want), "`flux skill --help` missing {want:?}");
        }
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

    /// `flux plugin …` help tells the truth about the current lifecycle and follows the naming
    /// trio (the protocol crate / a pack binary / the CLI, D-49): verified remote install from the
    /// signed pack with `--dir` as the local-scan mode (D-47), and enforced pin/rollback over the
    /// versioned store (D-48).
    #[test]
    fn plugin_help_documents_install_modes_and_pin_rollback() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let plugin = cmd.find_subcommand("plugin").expect("plugin subcommand");
        let top = plugin.clone().render_long_help().to_string();
        assert!(
            top.contains("plugin CLI"),
            "`flux plugin --help` should name the plugin CLI leg of the trio"
        );
        for want in ["install", "pin", "rollback", "status", "uninstall", "skill"] {
            assert!(top.contains(want), "`flux plugin --help` missing {want:?}");
        }
        let sub_help = |name: &str| {
            plugin
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("plugin subcommand {name}"))
                .clone()
                .render_long_help()
                .to_string()
        };
        let install = sub_help("install");
        for want in [
            "signed",
            "sha256",
            "versioned store",
            "--dir",
            "flux-plugin-*",
        ] {
            assert!(
                install.contains(want),
                "`flux plugin install --help` missing {want:?}"
            );
        }
        let pin = sub_help("pin");
        for want in ["versioned store", "sha256", "spawn", "rollback"] {
            assert!(
                pin.contains(want),
                "`flux plugin pin --help` missing {want:?}"
            );
        }
        let rollback = sub_help("rollback");
        for want in ["offline", "versioned store"] {
            assert!(
                rollback.contains(want),
                "`flux plugin rollback --help` missing {want:?}"
            );
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
        // The agent-path subcommands (`run`/`plan`/`tui`/`review`) carry the turn flags; `eval` has
        // its own `-m` but not `--max-tokens`.
        for agent_cmd in ["run", "plan", "tui", "review"] {
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
    fn endpoint_list_redacts() {
        // The `flux endpoint list` row renders the credential REFERENCE LOCATION, never a value: a
        // record with a kubernetes-scheme credential_ref shows `kubernetes/<ns>/<name>/<key>` and the
        // bare URL — and no secret-shaped string.
        use flux_secret::endpoint::{EndpointRecord, EndpointRef};
        use flux_secret::Ref;
        let rec = EndpointRecord {
            owner: "kubernetes".into(),
            ttl_secs: Some(900),
            health: Some("ok".into()),
            ..EndpointRecord::config(EndpointRef {
                credential_ref: Some(Ref::kubernetes("prod", "rds-creds", "password")),
                protocol: Some("postgres".into()),
                ..EndpointRef::discovered(
                    "prod-orders",
                    "postgres://orders.prod.svc:5432",
                    "postgres",
                )
            })
        };
        let row = render_endpoint_row(&rec);
        // The credential column is the LOCATION string only.
        assert!(
            row.contains("credential: kubernetes/prod/rds-creds/password"),
            "row must show the credential location: {row}"
        );
        // The bare URL + owner + ttl/health are present.
        assert!(row.contains("postgres://orders.prod.svc:5432"));
        assert!(row.contains("owner=kubernetes"));
        assert!(row.contains("ttl=900s") && row.contains("health=ok"));
        // No secret value leaks (the location names the key, never a value; nothing "secret"-shaped).
        assert!(!row.to_lowercase().contains("secret"));
        assert!(!row.contains("Bearer "));
        // A credential-less record renders `none`, not a placeholder value.
        let plain = EndpointRecord::config(EndpointRef::discovered(
            "svc-1",
            "https://svc.internal",
            "service",
        ));
        assert_eq!(credential_location(&plain), "none");
        assert!(render_endpoint_row(&plain).contains("credential: none"));
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

    // -----------------------------------------------------------------------
    // `flux review` (L-13): exit-code logic + output rendering
    // -----------------------------------------------------------------------

    fn finding(severity: &str) -> flux_tools::cognition::ReviewFinding {
        flux_tools::cognition::ReviewFinding {
            fingerprint: format!("fp-{severity}"),
            severity: severity.to_string(),
            category: "correctness".to_string(),
            file: Some("src/lib.rs".to_string()),
            line: Some(42),
            title: format!("a {severity} finding"),
            evidence: "some evidence".to_string(),
            recommendation: "fix it".to_string(),
            confidence: 0.8,
            reviewer: "correctness".to_string(),
            agreement: 1,
        }
    }

    fn report_with(severities: &[&str]) -> flux_tools::cognition::ReviewReport {
        flux_tools::cognition::ReviewReport {
            summary: "test report".to_string(),
            findings: severities.iter().map(|s| finding(s)).collect(),
            checked_files: vec!["src/lib.rs".to_string()],
            reviewers: vec!["correctness".to_string()],
            gaps: Vec::new(),
        }
    }

    /// `should_fail` is the pure decision factored out of `run_review` so the exit-code logic is
    /// unit-testable without going through `std::process::exit`: `None` (no `--fail-on`) never fails;
    /// a threshold fails iff some finding's severity is at or above it.
    #[test]
    fn should_fail_is_off_by_default() {
        let report = report_with(&["critical"]);
        assert!(
            !should_fail(&report, None),
            "no --fail-on must never fail, regardless of findings"
        );
    }

    #[test]
    fn should_fail_trips_at_or_above_the_threshold_only() {
        let report = report_with(&["low", "medium"]);
        assert!(
            !should_fail(&report, Some(ReviewSeverity::High)),
            "no finding reaches High"
        );
        assert!(
            should_fail(&report, Some(ReviewSeverity::Medium)),
            "the medium finding meets a Medium threshold"
        );
        assert!(
            should_fail(&report, Some(ReviewSeverity::Low)),
            "Low is at-or-above the Low threshold too"
        );
    }

    #[test]
    fn should_fail_is_false_when_there_are_no_findings() {
        let report = report_with(&[]);
        assert!(!should_fail(&report, Some(ReviewSeverity::Info)));
    }

    /// An unrecognized/malformed severity string must fail safe: it trips even the strictest
    /// (`Critical`) threshold rather than silently being ranked as harmless.
    #[test]
    fn should_fail_treats_an_unrecognized_severity_as_critical() {
        let report = report_with(&["not-a-real-severity"]);
        assert!(should_fail(&report, Some(ReviewSeverity::Critical)));
    }

    /// `--format json` must emit valid, round-trippable `ReviewReport` JSON — the CLI's own
    /// `serde_json::to_string_pretty` output parses back into an equivalent report.
    #[test]
    fn review_report_serializes_to_valid_json() {
        let report = report_with(&["high", "low"]);
        let s = serde_json::to_string_pretty(&report).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        assert_eq!(parsed["findings"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["summary"], "test report");
    }

    /// `render_review_markdown`'s default output mode names each finding's severity/title/category
    /// and reports the checked files + reviewers — a human-readable summary, not raw JSON.
    #[test]
    fn render_review_markdown_lists_findings_and_metadata() {
        let report = report_with(&["critical", "low"]);
        let md = render_review_markdown(&report);
        assert!(md.contains("# Strict review"));
        assert!(md.contains("test report"));
        assert!(md.contains("CRITICAL"));
        assert!(md.contains("a critical finding"));
        assert!(md.contains("correctness"));
        assert!(md.contains("src/lib.rs:42"));
    }

    #[test]
    fn render_review_markdown_reports_no_findings_and_gaps() {
        let mut report = report_with(&[]);
        report.gaps.push("dropped malformed entry".to_string());
        let md = render_review_markdown(&report);
        assert!(md.contains("No findings."));
        assert!(md.contains("## Gaps"));
        assert!(md.contains("dropped malformed entry"));
    }

    /// L-25 — `flux flow run --resume <session|last>`'s own session-resolution logic (the CLI-level
    /// seam, distinct from flux-flow's engine-level fast-forward tests): a literal session id passes
    /// straight through; an unnamed flow can't use `last` (nothing disambiguates it from any other
    /// unnamed halted plan, including an ordinary chat turn's inner `run_plan` halt — same store,
    /// same ledger machinery); and `last` finds the most recent halted session matching THIS flow's
    /// declared name, skipping a more-recent halted session that belongs to a different flow.
    #[test]
    fn resolve_resume_session_passes_through_literals_and_last_matches_by_flow_name() {
        use flux_flow::ast::{DraftAst, FailureKind, NodeId, RunEvent};
        use flux_flow::state::FlowStore;
        use std::sync::Arc;

        let events = Arc::new(EventStore::in_memory().unwrap());
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let named = DraftAst {
            name: Some("greet".into()),
            ..Default::default()
        };

        // A literal (non-"last") argument passes straight through, whatever it is — the caller
        // finds out soon enough (via `open_halted_plan` returning `None`) if it's wrong.
        assert_eq!(
            super::resolve_resume_session(&events, &flow, &named, "s_999").unwrap(),
            "s_999"
        );

        // An unnamed flow can't use `last` — refused with a clear, actionable error.
        let unnamed = DraftAst::default();
        let err = super::resolve_resume_session(&events, &flow, &unnamed, "last")
            .unwrap_err()
            .to_string();
        assert!(err.contains("declare a name"), "{err}");

        // `last` with nothing halted yet for this name is a clean error, not a silent no-op.
        assert!(super::resolve_resume_session(&events, &flow, &named, "last").is_err());

        // OLDER session, halted under THIS flow's name.
        let this_flow_session = events.create_session("mock").unwrap();
        flow.append_event(
            &this_flow_session,
            &RunEvent::PlanHalted {
                plan: "greet#aaaaaaaaaaaaaaaa".into(),
                node: NodeId(0),
                stmt: "s1".into(),
                op: None,
                kind: FailureKind::Runtime,
                error: "boom".into(),
            },
        )
        .unwrap();
        // NEWER session, halted under a DIFFERENT flow's name — `last` must not just grab the
        // newest halted session overall.
        let other_flow_session = events.create_session("mock").unwrap();
        flow.append_event(
            &other_flow_session,
            &RunEvent::PlanHalted {
                plan: "other-flow#bbbbbbbbbbbbbbbb".into(),
                node: NodeId(0),
                stmt: "s1".into(),
                op: None,
                kind: FailureKind::Runtime,
                error: "boom".into(),
            },
        )
        .unwrap();

        assert_eq!(
            super::resolve_resume_session(&events, &flow, &named, "last").unwrap(),
            this_flow_session,
            "matches by flow name, not just recency"
        );
    }

    // --- D-65: app-path redaction + audit parity -----------------------------------------------

    /// Direct unit test of the `flux_plugin::EgressAudit` L6 binding both the `build_agent` and
    /// `flux app run` plugin-wiring sites construct (`EventStoreEgressAudit`): appends a
    /// `PrivateNetAdmit` event onto the given run's stream — never a fabricated one — so a private-net
    /// admission is auditable regardless of which surface's plugin loop installed the hook.
    #[test]
    fn egress_audit_adapter_records_private_net_admit_on_the_runs_stream() {
        use flux_plugin::EgressAudit;
        use std::sync::Arc;

        let events = Arc::new(EventStore::in_memory().unwrap());
        let stream = events.create_session("mock").unwrap();
        let audit = EventStoreEgressAudit {
            store: events.clone(),
            stream: stream.clone(),
        };
        audit.record_private_admit("some-plugin", "127.0.0.1", "config:plugin/some-plugin");

        let recorded = events.load_by_kind(&stream, "private_net_admit").unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "exactly one PrivateNetAdmit landed on this run's stream"
        );
        match &recorded[0].kind {
            flux_events::EventKind::PrivateNetAdmit {
                caller,
                host,
                grant_source,
            } => {
                assert_eq!(caller, "some-plugin");
                assert_eq!(host, "127.0.0.1");
                assert_eq!(grant_source, "config:plugin/some-plugin");
            }
            other => panic!("expected PrivateNetAdmit, got {other:?}"),
        }
    }

    /// D-96: the ephemeral `--allow-private-net` override widens the *operator* grant to `*` for this
    /// process and stamps a distinct `cli:--allow-private-net` audit grant-source, while its absence
    /// preserves deny-by-default (an empty config yields no private grant). The manifest-declaration
    /// intersection that still gates each plugin lives in `flux_plugin::SystemHostCaps` and is covered
    /// there; this pins the CLI-surface wiring.
    #[test]
    fn allow_private_net_override_widens_grant_and_labels_audit() {
        let cfg = flux_config::Config::default();

        // Off (default): deny-by-default. Empty config → no private grant; audit source is the normal
        // per-plugin config label (matching SystemHostCaps::with_manifest's default).
        std::env::remove_var("FLUX_ALLOW_PRIVATE_NET");
        assert!(!super::private_net_cli_override());
        assert!(super::effective_plugin_private_hosts(&cfg, "gitlab").is_empty());
        assert!(super::effective_web_private_hosts(&cfg).is_empty());
        assert_eq!(
            super::private_net_grant_source_for("gitlab"),
            "config:plugin/gitlab"
        );

        // On: the operator grant widens to `*` and the audit source becomes the CLI-flag label.
        std::env::set_var("FLUX_ALLOW_PRIVATE_NET", "1");
        assert!(super::private_net_cli_override());
        assert_eq!(
            super::effective_plugin_private_hosts(&cfg, "gitlab"),
            vec!["*".to_string()]
        );
        assert_eq!(
            super::effective_web_private_hosts(&cfg),
            vec!["*".to_string()]
        );
        assert_eq!(
            super::private_net_grant_source_for("gitlab"),
            "cli:--allow-private-net"
        );

        std::env::remove_var("FLUX_ALLOW_PRIVATE_NET");
    }

    /// Direct unit test of the `flux_capabilities::CrossPluginAudit` L6 binding
    /// (`EventStoreCrossPluginAudit`): records a `CrossPluginResolve` per successful cross-plugin
    /// credential resolution (D-27) and an `EndpointDiscovered` per provider whose discovery returned
    /// candidates (D-30), both onto the given run's stream. The SAME struct backs
    /// `.with_cross_plugin_audit(...)` on both the `build_agent` and `flux app run` paths' brokers.
    #[test]
    fn cross_plugin_audit_adapter_records_resolve_and_discovery_on_the_runs_stream() {
        use flux_capabilities::CrossPluginAudit;
        use std::sync::Arc;

        let events = Arc::new(EventStore::in_memory().unwrap());
        let stream = events.create_session("mock").unwrap();
        let audit = EventStoreCrossPluginAudit {
            store: events.clone(),
            stream: stream.clone(),
        };
        audit.record_cross_plugin_resolve("consumer", "kubernetes", "kubernetes/ns/name/key");
        audit.record_discovery("postgres", "kubernetes", 3);

        let resolves = events
            .load_by_kind(&stream, "cross_plugin_resolve")
            .unwrap();
        assert_eq!(resolves.len(), 1);
        match &resolves[0].kind {
            flux_events::EventKind::CrossPluginResolve {
                consumer,
                provider,
                reference_location,
            } => {
                assert_eq!(consumer, "consumer");
                assert_eq!(provider, "kubernetes");
                assert_eq!(reference_location, "kubernetes/ns/name/key");
            }
            other => panic!("expected CrossPluginResolve, got {other:?}"),
        }

        let discoveries = events.load_by_kind(&stream, "endpoint_discovered").unwrap();
        assert_eq!(discoveries.len(), 1);
        match &discoveries[0].kind {
            flux_events::EventKind::EndpointDiscovered {
                product,
                provider,
                count,
            } => {
                assert_eq!(product, "postgres");
                assert_eq!(provider, "kubernetes");
                assert_eq!(*count, 3);
            }
            other => panic!("expected EndpointDiscovered, got {other:?}"),
        }
    }

    /// D-65's acceptance centerpiece — mirror of flux-app's C-13 seeding guarantee, but through the
    /// CROSS-PLUGIN credential path (`SystemHostCaps`'s `credential` capability, resolved via the
    /// endpoint broker) that both the `build_agent` and `flux app run` plugin-wiring sites install a
    /// `RedactorSecretSink` on. Drives `app_plugin_caps` — the SAME function `run_app`'s plugin loop
    /// calls to build a plugin's caps — so a regression in the production wiring (e.g. dropping
    /// `.with_secret_sink(...)`) fails this test too, not just a hand-rolled re-implementation. A
    /// credential resolved this way must land in the SAME redactor an executor dispatches with, so it
    /// is scrubbed from model-visible tool output even though the trusted plugin binary received the
    /// raw value.
    #[tokio::test]
    async fn cross_plugin_credential_resolution_seeds_the_redactor_used_by_dispatch() {
        use async_trait::async_trait;
        use flux_capabilities::{
            CredentialReader, CrossPluginGrants, EndpointBroker, EndpointRegistry,
            HostProviderInvoker, MemoryBackend, PluginRegistry,
        };
        use flux_plugin::{PluginCapabilities, PluginManifest};
        use flux_runtime::{
            AllowApprover, Approver, Executor, PermissionManager, ToolContext, ToolRegistry,
            ToolResult,
        };
        use flux_secret::{Redactor, Ref};
        use flux_system::{System, Workspace};
        use std::sync::Arc;

        /// A fake credential reader (mirrors flux-capabilities' own broker-test double) so the
        /// cross-plugin gate resolves without a provider subprocess.
        struct FakeReader {
            value: String,
        }
        #[async_trait]
        impl CredentialReader for FakeReader {
            async fn read(&self, _provider: &str, _reference: &Ref) -> Result<String, String> {
                Ok(self.value.clone())
            }
        }

        let secret = "k8s-pg-password-d65";
        let broker = Arc::new(
            EndpointBroker::new(
                Arc::new(HostProviderInvoker::new(Arc::new(PluginRegistry::new()))),
                Arc::new(PluginRegistry::new()),
                Arc::new(EndpointRegistry::new()),
            )
            .with_credential_reader(Arc::new(FakeReader {
                value: secret.to_string(),
            }))
            .with_cross_plugin_grants(CrossPluginGrants::new(vec!["consumer:kubernetes".into()])),
        );

        let redactor = Redactor::new();
        let secret_sink = Arc::new(RedactorSecretSink {
            redactor: redactor.clone(),
        }) as Arc<dyn flux_plugin::SecretSink>;
        let events = Arc::new(EventStore::in_memory().unwrap());
        let stream = events.create_session("mock").unwrap();
        let audit: Arc<dyn flux_plugin::EgressAudit> = Arc::new(EventStoreEgressAudit {
            store: events,
            stream,
        });
        let manifest = PluginManifest {
            name: "consumer".into(),
            capabilities: PluginCapabilities {
                credential: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let dir = std::env::temp_dir().join(format!("flux-d65-secret-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let backend =
            Arc::new(MemoryBackend::new()) as Arc<dyn flux_capabilities::DatasourceBackend>;
        let caps = app_plugin_caps(
            system.clone(),
            backend,
            &manifest,
            Vec::new(),
            broker.clone() as Arc<dyn flux_plugin::ReferenceResolver>,
            audit,
            secret_sink,
            broker.clone(),
        );

        let cred = Ref::kubernetes("monitoring", "pg-creds", "password");
        let result = caps
            .handle("credential", &json!({ "credential_ref": cred.to_string() }))
            .await
            .expect("credential capability granted + resolver installed");
        assert_eq!(
            result["value"], secret,
            "the trusted plugin still receives the raw value"
        );

        // The resolved credential is now a known secret to `redactor` — a tool leaking it comes back
        // scrubbed, exactly like flux-app's C-13 guarantee (`journey_executor_scrubs_resolved_secrets_
        // from_tool_output`).
        struct LeakyTool {
            secret: String,
        }
        #[async_trait]
        impl flux_runtime::Tool for LeakyTool {
            fn spec(&self) -> flux_spec::ToolSpec {
                flux_spec::ToolSpec::read_only("search", "leaks", json!({"type": "object"}))
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _params: serde_json::Value,
            ) -> flux_core::Result<ToolResult> {
                Ok(ToolResult::ok(format!("found: {}", self.secret)))
            }
        }
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(LeakyTool {
            secret: secret.to_string(),
        }));
        let ctx = ToolContext::new(system).with_redactor(redactor);
        let perms = PermissionManager::from_rules(&["search".to_string()], &[]);
        let approver: Arc<dyn Approver> = Arc::new(AllowApprover);
        let executor = Executor::new(registry, perms, approver, ctx);
        let r = executor.dispatch("search", json!({})).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            !r.content.contains(secret),
            "the cross-plugin-resolved credential must be scrubbed from tool output: {}",
            r.content
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
