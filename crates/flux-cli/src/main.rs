//! The `flux` binary.
//!
//! Product surface for adaptive agent turns, authored Flux-Lang flows and apps, replay, plugins,
//! authentication, and developer tooling. Every effect enters through the shared guarded runtime.

mod changelog;
mod plugin_skill;
mod preset;
mod skill_cmd;
mod style;
mod usage;

use std::future::Future;
use std::io::{IsTerminal, Write};

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser};
use futures::StreamExt;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_agent::{
    AdaptiveLoopPolicy, AgentLoopSpec, AgentSpec, AgentStagePolicy, DEFAULT_SYSTEM_PROMPT,
};
use flux_core::{Chunk, ContentBlock, StopReason, Usage};
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_flow::state::FlowStore;
use flux_flow::AgentSink;
use flux_orchestrate::{ProviderFactory, Role, RoleRegistry, SubAgents, TaskTool};
use flux_provider::{ChunkStream, Effort, NativeProvider, Provider, Request};
use flux_runtime::context::{EnvContext, GitContext, ProjectFiles, Projector, RepoSignal};
use flux_runtime::{
    AllowApprover, ApprovalChoice, Approver, Executor, PermissionManager, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_spec::IntentSet;
use flux_system::{System, Workspace};
use reedline::{FileBackedHistory, Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};
use std::borrow::Cow;

/// flux — typed model judgment, deterministic execution.
#[derive(Parser, Debug)]
#[command(
    name = "flux",
    version,
    about = "flux — typed model judgment, deterministic execution",
    long_about = "flux — typed model judgment, deterministic execution.\n\n\
        Run the agent with `flux run <prompt>`; with no arguments, `flux` opens the interactive REPL. \
        `flux tui` is the chat UI, `flux flow run <flow.flux>` runs an authored flow, and \
        `flux app run <program.flux>` runs a multi-agent program \
        (add `--serve <addr>` to expose an agent over HTTP/A2A). Run `flux help` for the full list of \
        commands."
)]
struct Cli {
    /// A subcommand (run `flux help` to list them). With none, `flux` opens the interactive REPL.
    #[command(subcommand)]
    command: Option<Commands>,

    /// When to colorize output: auto (stdout AND stderr are terminals, `NO_COLOR` unset),
    /// always, or never.
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
    /// persisted). Plugins still only reach the private hosts their manifest declares; `web.fetch`
    /// is opened for the run (its guard has no manifest safeguard, so this re-exposes cloud-metadata
    /// and RFC-1918 ranges to any fetched URL). Prefer a scoped `[private_net.plugins]` grant for
    /// anything recurring. Exported as `FLUX_ALLOW_PRIVATE_NET` so `app run`/`plugin call` inherit it.
    #[arg(long = "allow-private-net", global = true)]
    allow_private_net: bool,

    /// Turn on OS-level process sandboxing (bubblewrap on Linux, Seatbelt on macOS) for spawned
    /// shell/plugin processes — defense-in-depth underneath the safety envelope, orthogonal to
    /// approvals. Off by default; layers over `[sandbox]` in .flux/config.toml (the strictest of
    /// this flag, a pre-set `FLUX_SANDBOX`, and config wins). Exported as `FLUX_SANDBOX` so
    /// `app run`/`plugin call` and other subprocess/child-flux paths inherit it. If no usable
    /// backend is available (unsupported platform, or the wrapper is missing/blocked) this degrades
    /// to a one-line warning and runs unconfined — unless `[sandbox] require` (or
    /// `FLUX_SANDBOX=require`) is set, which fails closed at startup. `--no-sandbox` is the kill
    /// switch. See docs/designs/process-sandboxing.md.
    #[arg(long = "sandbox", global = true, conflicts_with = "no_sandbox")]
    sandbox: bool,

    /// Force OS-level sandboxing OFF for this invocation — the kill switch, overriding `--sandbox`,
    /// a pre-set `FLUX_SANDBOX`, and `[sandbox]` config.
    #[arg(long = "no-sandbox", global = true, conflicts_with = "sandbox")]
    no_sandbox: bool,
}

/// The flags for running an agent turn — flattened into the agent-path subcommands (`run`,
/// `tui`, `fork`, `app run`), so they live on those commands and stay off every other subcommand's
/// help. (`--color` is `global` on [`Cli`] instead; it applies to every command. `review` carries
/// its own smaller [`ReviewFlags`].) `fork` and `app run <program>` reject the session/turn flags
/// their paths can't honor at runtime (see `run_fork`/`run_app`).
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
    ///   Short aliases `sonnet`, `opus`, `haiku`, `fable` are shorthands for `anthropic/<model>`;
    ///   bare `claude` is shorthand for `claude/sonnet` (the subscription's default model); bare
    ///   `codex` is shorthand for `codex/gpt-5.5` (the ChatGPT-subscription main model; the
    ///   legacy `*-codex` ids are rejected by the backend); bare `aws` (or `aws/sonnet`,
    ///   `aws/opus`, `aws/haiku`) resolves to the region's Bedrock inference profile.
    /// Examples: `claude/claude-sonnet-4-6`, `openai/gpt-4o`, `codex/gpt-5.5`,
    ///   `aws/us.anthropic.claude-sonnet-4-6`, `openrouter-anthropic/z-ai/glm-4.6`.
    /// Overrides `model` in `.flux/config.toml`; falls back to `sonnet` (= `anthropic/claude-sonnet-5`).
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// Ask capable providers/models to expose adaptive thinking for every call owned by this agent.
    #[arg(long)]
    think: bool,

    /// Reasoning effort for intent, exploration, presentation, compaction, cognition, and inherited
    /// sub-agent calls.
    #[arg(long, value_enum)]
    effort: Option<EffortArg>,

    /// Agent outer loop: `adaptive` (default) or a Flux-Lang source file. A file is selected only
    /// when named here; `.flux/agent-loop.flux` has no implicit effect.
    #[arg(long = "loop", value_name = "ADAPTIVE|FILE")]
    agent_loop: Option<String>,

    /// Maximum tokens per model-stage call. A truncated intent, exploration, repair, or presentation
    /// stage fails loudly rather than silently stopping. Zero would fail at the provider, so it is
    /// rejected at parse time.
    #[arg(long, default_value_t = 16384, value_parser = clap::value_parser!(u32).range(1..))]
    max_tokens: u32,

    /// Maximum provider calls across one logical adaptive turn, including intent repairs,
    /// exploration, and every decision resume. Overrides `[agent.adaptive] max_model_calls`.
    #[arg(long, value_parser = parse_positive_usize)]
    max_model_calls: Option<usize>,

    /// Maximum decision/batch iterations in the authored outer loop (1–1,000). This is separate
    /// from model calls: one iteration may execute a batch, ask a question, or continue from a report.
    /// Overrides `[agent] max_iterations`.
    #[arg(long, value_parser = parse_positive_usize)]
    max_iterations: Option<usize>,

    /// Per-turn token budget (all tiers, summed across the turn's model calls): once crossed, the
    /// turn ends honestly with a budget-exceeded answer instead of consulting the model again.
    /// Overrides `FLUX_TURN_TOKEN_BUDGET` and `[limits] turn_token_budget` in .flux/config.toml.
    /// Off by default (no ceiling) — 0 would mean "instantly exceeded", not "off", so it is
    /// rejected at parse time.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    turn_budget: Option<u64>,

    /// (Hidden) Print token usage — accepted for CLI compatibility; currently a no-op (usage/cost
    /// is always shown on the turn-end rule; see also `flux usage`).
    #[arg(long, hide = true)]
    usage: bool,

    /// (Hidden, deprecated) The Flux-Lang engine is the default for a bare prompt; this is a no-op.
    #[arg(long, hide = true)]
    agent: bool,

    /// Auto-approve every tool call (headless). Without it, unmatched calls prompt for approval.
    #[arg(long)]
    yes: bool,

    /// Show tool output in full (no truncation). Action batches and tool inputs are always shown in
    /// full; this also un-caps tool *output* (e.g. large file reads). Also enabled by `FLUX_VERBOSE`.
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Reveal the agent loop: stream its typed stages (`detect_intent`/`explore`/batch approval and
    /// execution/…) that are filtered from the surface by default. Also enabled by
    /// `FLUX_SHOW_LOOP`. See `flux loop show` for the authored loop and `/evidence` for the audit trail.
    #[arg(long)]
    show_loop: bool,

    /// Trace the outer agent loop's structure: one dim line per round (`⟳ round 3/50`) and per
    /// structural node (op calls with bind names, match/when branches taken, return) of the
    /// agent-loop program. Native leaf-operation execution is not traced. Also enabled by
    /// `FLUX_TRACE_LOOP`.
    #[arg(long)]
    trace_loop: bool,

    /// Extra skill directory, layered above `[skills] dirs` from .flux/config.toml and the
    /// well-known set (`.flux/skills`, `.claude/skills`, `~/.flux/skills`, …). Repeatable; earlier
    /// dirs win a skill-name clash.
    #[arg(long = "skill-dir", value_name = "DIR")]
    skill_dirs: Vec<std::path::PathBuf>,

    /// Explicitly enable a discovered skill by name. Repeatable. Skills are never activated from
    /// prompt keywords automatically; `--skill-dir` and config directories only affect discovery.
    #[arg(long = "skill", value_name = "NAME")]
    skills: Vec<String>,

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

/// The flags `flux review` actually consumes — deliberately NOT the full [`AgentFlags`] set.
/// Review runs the embedded strict-review flow through `flux_sdk::FlowClient`, so the turn flags
/// (`--continue`/`--resume`, `--turn-budget`, `--skill-dir`, `--dev`, `-v`, `--yes`, …) have no
/// effect on that path; offering them would accept-and-ignore, so they are rejected at parse time
/// instead. (Review always auto-approves its own fixed, read-only flow — see `run_review`.)
#[derive(clap::Args, Debug)]
struct ReviewFlags {
    /// Fully-qualified `provider/model` spec the reviewer sub-agents run (same forms as `flux run -m`).
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// Maximum tokens per reviewer model call.
    #[arg(long, default_value_t = 16384, value_parser = clap::value_parser!(u32).range(1..))]
    max_tokens: u32,
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
    // NOTE: `agent_flags` (below the enum) must cover every variant that flattens [`AgentFlags`] —
    // it feeds the pre-runtime `apply_agent_env` export in `main`.
    /// Run the agent on a prompt, or a multi-agent program: `flux run <prompt…>` / `flux run <app.flux>`.
    Run {
        #[command(flatten)]
        agent: AgentFlags,
        /// The prompt words, or a path to an `<app.flux>` multi-agent program. Agent flags
        /// (`-m`, `--yes`, …) may appear before or after.
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
    /// envelope — inject a different value, run an edited authored flow, or re-enter the adaptive agent.
    Fork {
        /// Session id (`s_42`), or `last` for the most recent session.
        session: String,
        /// Top-level statement index (0-based) of the run's FINAL executed plan to diverge at.
        #[arg(long)]
        at: usize,
        /// Mode A: inject this JSON value as the fork statement's result, then run the rest live.
        #[arg(long, conflicts_with_all = ["edit", "replan"])]
        inject: Option<String>,
        /// Mode C: continue with this edited flow file (.flux text or JSON DraftAst) — unchanged
        /// leading statements fast-forward against the replayed prefix, edits run live.
        #[arg(long, conflicts_with_all = ["inject", "replan"])]
        edit: Option<String>,
        /// Mode B (default): re-enter the adaptive agent from the forked state.
        #[arg(long)]
        replan: bool,
        /// With --replan (mode B, the default): the instruction for the adaptive tail
        /// (default: continue the recorded task). Meaningless for --inject/--edit, so those
        /// combinations are rejected.
        #[arg(long, conflicts_with_all = ["inject", "edit"])]
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
        #[arg(long, env = "FLUX_A2A_TOKEN", hide_env_values = true)]
        token: Option<String>,
    },
    /// Run a benchmark suite against flux and print a summary.
    #[command(
        after_help = "ADAPTERS:\n  synthetic       real-model coding riddles (fast, no Docker)\n  mock            offline CI fixture (drives -m mock)\n  terminal-bench  the real Docker benchmark\n  multi           several behind one combined score (with --members)\n\nEXAMPLES:\n  flux eval synthetic -m openrouter-anthropic/anthropic/claude-sonnet-4.6 --watch --report r.md\n  flux eval multi --members synthetic,terminal-bench"
    )]
    Eval {
        /// Which suite to run.
        #[arg(value_enum)]
        adapter: EvalAdapter,
        /// Model the suite's agent runs (e.g. `-m mock`, `-m openrouter-anthropic/anthropic/claude-sonnet-4.6`).
        #[arg(short = 'm', long)]
        model: Option<String>,
        /// Restrict to these task ids (comma-separated).
        #[arg(long, value_delimiter = ',')]
        tasks: Vec<String>,
        /// For `multi`: the member adapters to combine (comma-separated). Only meaningful with
        /// the `multi` adapter (checked at startup).
        #[arg(long, value_delimiter = ',')]
        members: Vec<String>,
        /// Cap the number of tasks (0 = all).
        #[arg(long, default_value_t = 0)]
        limit: u64,
        /// Trials per task (>1 averages out single-run model noise).
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..))]
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
    /// `~/.flux/flows` (global): list/run them directly with `flux flow list` / `flux flow run
    /// <name>`, or let the agent use `flow_list` / `flow_run`. Composite `op`s placed there
    /// auto-load as callable ops. (The legacy `~/.flux/ops` / `.flux/ops` dirs are still read.)
    Flow {
        #[command(subcommand)]
        action: FlowAction,
    },
    /// Render a `.flux` file as a self-contained SVG: the highlighted source (default) or the
    /// execution-path plan tree. The non-model entry point to the `flow_render` tool's renderer,
    /// and the generator for flux's own doc images. Prints the SVG to stdout unless `-o` is given.
    Render {
        /// Path to the `.flux` file to render.
        file: String,
        /// Which view to render: `source` (highlighted source; total, malformed input still
        /// renders) or `tree` (execution-path plan tree; a hard parse error exits non-zero).
        #[arg(long, value_enum, default_value_t)]
        view: RenderView,
        /// Write the SVG to this path (workspace-confined, parents created) instead of stdout.
        #[arg(short = 'o', long, value_name = "OUT.svg")]
        out: Option<String>,
    },
    /// Run the strict-review protocol over `--files` and print a `ReviewReport` (flux L-13; design
    /// `docs/designs/strict-review-flows.md`). Self-contained: the reviewer roles and the
    /// `strict_review` flow are embedded in the binary, so this works in any repo — a project's own
    /// `.flux/agents/review-*.md` still overrides the built-in role definitions. Read-only: this never
    /// posts anywhere, it only prints to stdout.
    Review {
        #[command(flatten)]
        flags: ReviewFlags,
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
    /// Inspect or customize the agent loop (the Flux-Lang program that drives every turn).
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
    /// Hermetically replay a recorded session (A-45): host-recorded execution flows re-parse from
    /// durable history, and op outputs come from the C-43 cassette — no model call, no live IO,
    /// side effects never re-fired. Divergence from the recording fails loudly.
    Replay {
        /// Session id (`s_42`), or `last` for the most recent session.
        #[arg(default_value = "last")]
        session: String,
        /// Replay only this turn's recorded flows (1-based — turn 0 is a usage error, not an alias for
        /// the first turn). Cross-turn symbol references fail honestly.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        turn: Option<u64>,
        /// Also replay this session's sub-agent child streams (A-08 correlation), in spawn order.
        #[arg(long)]
        sub_agents: bool,
        /// Emit a machine-readable JSON report instead of the human summary.
        #[arg(long)]
        json: bool,
    },
    /// Diff two recorded runs (C-44): align their executed statements and show exactly where the
    /// FLOW changed (differing statement content) vs where the same flow hit a DIFFERENT WORLD
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
    /// Provider and plugin authentication (status / login / set).
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
        #[arg(long, requires = "install")]
        global: bool,
    },
    /// Show what changed in flux, in plain language (the customer changelog).
    Changelog {
        /// Show a specific version's section (e.g. `0.11.6`).
        #[arg(conflicts_with_all = ["all", "unreleased"])]
        version: Option<String>,
        /// Show every recorded release.
        #[arg(long, conflicts_with = "unreleased")]
        all: bool,
        /// Show the not-yet-released section (development builds).
        #[arg(long)]
        unreleased: bool,
    },
    /// Print a shell completion script to stdout (defaults to fish).
    Completion {
        /// Shell to generate for (defaults to fish). An unknown shell is a usage error (exit 2),
        /// so a scripted `flux completion <shell> > file` can't silently install an empty script.
        #[arg(value_enum)]
        shell: Option<clap_complete::Shell>,
    },
    /// Scaffold or run a parameterized flow recipe.
    Preset {
        /// `list` | `<name> key=value …` (passed through to the preset cookbook).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

impl Commands {
    /// The flattened [`AgentFlags`] of an agent-path subcommand (`run`/`tui`/`fork`/
    /// `app run`), if this is one. `main` uses this to export the flags' env signals BEFORE the
    /// tokio runtime exists — `set_var` must not race worker-thread `getenv`s.
    fn agent_flags(&self) -> Option<&AgentFlags> {
        match self {
            Self::Run { agent, .. }
            | Self::Tui { agent }
            | Self::Fork { agent, .. }
            | Self::App {
                action: AppAction::Run { agent, .. },
            } => Some(agent),
            _ => None,
        }
    }
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
        /// program, serves its agent; with none, serves the built-in coding agent — that no-program
        /// form requires `--yes` (HTTP requests have no interactive approver). Give `<program>` BEFORE
        /// a bare `--serve` (`flux app run prog.flux --serve`) or attach a custom address with `=`
        /// (`--serve=0.0.0.0:8787`) — `--serve <addr>` followed by a program path swallows the path as
        /// the address instead (clap's usual optional-value-flag ambiguity).
        #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "127.0.0.1:8787")]
        serve: Option<String>,
    },
}

/// `flux flow …`
#[derive(clap::Subcommand, Debug)]
enum FlowAction {
    /// List saved flows and composite ops from the project and global flows homes.
    #[command(visible_alias = "ls")]
    List,
    /// Run a checked-in Flux-Lang file or a saved flow by filename stem / declared name.
    Run {
        /// Existing file path, saved-flow filename stem, or declared flow name. Existing files win.
        target: String,
        /// Deterministic flow inputs as one JSON object. Keys must be declared flow parameters.
        #[arg(long, value_name = "JSON")]
        inputs: Option<String>,
        /// Deterministic input override, repeatable. Values are coerced from the parameter TypeRef;
        /// a later duplicate key wins.
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,
        /// Opt in to model-assisted mapping for parameters not covered by --inputs / --arg. The
        /// mapper is recorded in the Flux AST and runs through the normal approval/runtime envelope.
        #[arg(long, value_name = "TEXT")]
        map_inputs: Option<String>,
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
        /// Resume a previously halted run of THIS flow target: a literal session id (printed by the halt
        /// report), or `last` — the most recent halted `flow run` session for this flow's declared
        /// name (`flow <name> -> …`; an unnamed flow can't be disambiguated this way and needs the
        /// explicit session id). Re-resolves/re-parses this (possibly corrected) target, folds the halted
        /// session's statement ledger, fast-forwards the matching completed prefix (values
        /// rehydrated), and executes from the first changed statement.
        #[arg(long, value_name = "SESSION|last")]
        resume: Option<String>,
        /// The payload to bind to a resumed top-level `await` (`$reply = await …`). Parsed as JSON, so
        /// a bare word is a JSON string (`--resume-value hi` binds `"hi"`) and `--resume-value 42`
        /// binds the number. Required when `--resume`-ing a session that halted awaiting a value; omit
        /// it for a plain checkpoint/failure resume. Without it, resuming past an unbound await refuses
        /// with a clear error instead of failing later on `unbound symbol`. Only meaningful with
        /// `--resume` (a fresh run has no halted await to bind), so that pairing is enforced.
        #[arg(long, value_name = "JSON", requires = "resume")]
        resume_value: Option<String>,
    },
}

/// `flux loop …`
#[derive(clap::Subcommand, Debug)]
enum LoopAction {
    /// Print the built-in adaptive agent loop (the default).
    Show,
    /// Write the built-in loop to `.flux/agent-loop.flux` so it can be edited and selected explicitly.
    Eject {
        /// Overwrite an existing ejected copy.
        #[arg(short, long)]
        force: bool,
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
    /// Store a bearer token for an installed plugin's auth purpose (D-126): prompts for the token
    /// (hidden; reads one line from stdin when piped, so it scripts) and writes
    /// `~/.flux/credentials.toml` (0600) — a later session then resolves the purpose WITHOUT the
    /// secret in the process environment. A stored token wins over the declared env keys.
    Set {
        /// Installed plugin name (e.g. `slack`).
        plugin: String,
        /// Manifest auth purpose (e.g. `bot_token`); optional when the plugin declares exactly one.
        purpose: Option<String>,
        /// Remove the stored token for this purpose instead of setting one.
        #[arg(long)]
        clear: bool,
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
    /// over the JSON base). `--dry-run` validates against the op's schema and prints the coerced
    /// input without invoking the op (the plugin process IS spawned to read its manifest);
    /// `--no-validate` skips schema coercion/validation.
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
        /// Validate the input against the op's schema and print the coerced input + any
        /// problems — the op is never invoked. (The plugin process is still spawned once to
        /// read its manifest, which carries the schema.) Contradicts `--no-validate`, so the
        /// combination is rejected.
        #[arg(long = "dry-run", conflicts_with = "no_validate")]
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
        #[arg(long, conflicts_with = "names")]
        all: bool,
        /// Scan a local directory for already-built `flux-plugin-*` binaries instead of the
        /// remote pack channel (local-scan mode; defaults to `plugins/target/release` when given
        /// with no value). The path must be attached with `=` (`--dir=path`) — an optional-value
        /// flag would otherwise swallow a following plugin name as its path.
        #[arg(
            long,
            value_name = "PATH",
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "plugins/target/release",
            conflicts_with_all = ["names", "all"]
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
        #[arg(long, requires = "install")]
        global: bool,
        /// Write the SKILL.md to this single file (references go in a sibling `references/`).
        /// A different destination than `--install`, so combining them is rejected.
        #[arg(long, conflicts_with_all = ["install", "global"])]
        out: Option<String>,
    },
}

/// `flux endpoint …` — the operator mirror of the agent's `endpoint.*` ops over the persisted
/// `~/.flux/endpoints.toml` store. Every subcommand deals in weak references only: it shows the
/// credential *location* (the `credential_ref`), never a value.
#[derive(clap::Subcommand, Debug)]
enum EndpointAction {
    /// Wire a known service so the agent can use it now and in any later session: persist a weak,
    /// credential-free `EndpointRef` to `~/.flux/endpoints.toml`. The canonical case is a Postgres
    /// database. The credential is a *location* (`--credential-ref`), never a value; the URL must be
    /// credential-free. The declarative alternative is a `[[endpoint.static]]` block in
    /// `.flux/config.toml`.
    Add {
        /// The named reference id the agent uses as `endpoint_ref` (a bare name, e.g. `pg-prod`).
        /// Not an `@endpoint/…` id — that prefix is reserved for discovered endpoints.
        id: String,
        /// Bare `scheme://host[:port][/path]` — no embedded credentials (use `--credential-ref`).
        #[arg(long)]
        url: String,
        /// Product class (`postgres`, `prometheus`, …) — drives op surfacing and display.
        #[arg(long)]
        product: Option<String>,
        /// Wire-protocol hint (`postgres`, `http`, `ami`, …).
        #[arg(long)]
        protocol: Option<String>,
        /// Credential *location*: `env/KEY`, `kubernetes/<ns>/<name>/<key>`, or
        /// `plugin/<p>/<i>/<slot>`. Omit for an unauthenticated endpoint. Never a value.
        #[arg(long, value_name = "REF")]
        credential_ref: Option<String>,
        /// Repeatable non-secret label `key=value` (region, tags) for display/filtering.
        #[arg(long = "label", value_name = "K=V")]
        labels: Vec<String>,
    },
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

/// `flux eval <adapter>` — the benchmark suites flux-eval can drive. A typo'd adapter is a parse
/// error listing these, instead of a deep `build_adapter` failure after startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum EvalAdapter {
    /// Real-model coding riddles (fast, no Docker).
    Synthetic,
    /// Offline CI fixture (drives `-m mock`).
    Mock,
    /// The real Docker benchmark.
    TerminalBench,
    /// Several suites behind one combined score (with `--members`).
    Multi,
}

impl EvalAdapter {
    /// The wire name flux-eval's `build_adapter` expects — identical to the clap value name.
    fn as_str(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::Mock => "mock",
            Self::TerminalBench => "terminal-bench",
            Self::Multi => "multi",
        }
    }
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

fn parse_effort(value: &str) -> Result<Effort> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Ok(Effort::Low),
        "medium" => Ok(Effort::Medium),
        "high" => Ok(Effort::High),
        "xhigh" => Ok(Effort::Xhigh),
        "max" => Ok(Effort::Max),
        _ => anyhow::bail!("expected low, medium, high, xhigh, or max; got {value:?}"),
    }
}

fn parse_positive_usize(value: &str) -> std::result::Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("expected a positive integer: {error}"))?;
    if parsed == 0 {
        return Err("expected a positive integer greater than zero".into());
    }
    Ok(parsed)
}

fn adaptive_stage_policy(
    name: &str,
    config: &flux_config::AdaptiveStageConfig,
) -> Result<AgentStagePolicy> {
    if config.max_tokens == Some(0) {
        bail!("[agent.adaptive.{name}] max_tokens must be greater than zero");
    }
    if config.max_calls == Some(0) {
        bail!("[agent.adaptive.{name}] max_calls must be greater than zero");
    }
    let effort = config
        .effort
        .as_deref()
        .map(parse_effort)
        .transpose()
        .with_context(|| format!("[agent.adaptive.{name}] effort"))?;
    Ok(AgentStagePolicy {
        model: config.model.clone(),
        effort,
        max_tokens: config.max_tokens,
        max_calls: config.max_calls,
    })
}

fn adaptive_loop_policy(
    flags: &AgentFlags,
    config: &flux_config::AgentConfig,
) -> Result<AdaptiveLoopPolicy> {
    if config.adaptive.max_model_calls == Some(0) {
        bail!("[agent.adaptive] max_model_calls must be greater than zero");
    }
    Ok(AdaptiveLoopPolicy {
        max_model_calls: flags
            .max_model_calls
            .or(config.adaptive.max_model_calls)
            .unwrap_or(flux_flow::DEFAULT_ADAPTIVE_MODEL_CALLS),
        intent: adaptive_stage_policy("intent", &config.adaptive.intent)?,
        explore: adaptive_stage_policy("explore", &config.adaptive.explore)?,
    })
}

fn agent_max_iterations(flags: &AgentFlags, config: &flux_config::AgentConfig) -> Result<usize> {
    // Resolve under CLI > config > default precedence first, then validate the value that actually
    // takes effect. A valid `--max-iterations` must override bad project/user config, not be
    // defeated by it; retaining the winning source also keeps the startup diagnostic actionable.
    let (resolved, source) = match (flags.max_iterations, config.max_iterations) {
        (Some(value), _) => (value, "`--max-iterations`"),
        (None, Some(value)) => (value, "[agent] max_iterations"),
        (None, None) => (
            flux_flow::DEFAULT_AGENT_LOOP_ITERATIONS,
            "default max_iterations",
        ),
    };
    if resolved == 0 {
        bail!("{source} must be greater than zero");
    }
    if resolved > flux_flow::MAX_AGENT_LOOP_ITERATIONS {
        bail!(
            "{source} must not exceed the maximum of {}",
            flux_flow::MAX_AGENT_LOOP_ITERATIONS
        );
    }
    Ok(resolved)
}

/// `flux render --view` — CLI mirror of [`flux_tools::render::View`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
enum RenderView {
    /// The highlighted source (total — malformed input still renders).
    #[default]
    Source,
    /// The execution-path plan tree (needs parseable source).
    Tree,
}

impl From<RenderView> for flux_tools::render::View {
    fn from(v: RenderView) -> Self {
        match v {
            RenderView::Source => Self::Source,
            RenderView::Tree => Self::Tree,
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

/// Parse a fully-qualified `provider/model` spec and build the matching provider from environment
/// credentials. Thin delegate to the shared [`flux_providers::spec::build`] (D-152 moved the
/// mapping into `flux-providers` so every embedder resolves a spec identically); the `?` folds the
/// library's `flux_core::Error` into the CLI's `anyhow` chain with the same string. Spec forms and
/// validation live in `flux_providers::spec::parse_model_spec`.
fn build_provider(spec: &str) -> Result<(NativeProvider, String, String)> {
    Ok(flux_providers::spec::build(spec)?)
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
        // Size-check via metadata BEFORE reading: this runs on every agent construction, and a
        // stray 500 MB `notes.txt` must not cost a whole-file read+alloc just to be discarded.
        if !matches!(system.file_size(&f).await, Ok(n) if n as usize <= MAX_BYTES) {
            continue;
        }
        if let Ok(text) = system.read_file(&f).await {
            docs.push((f, text));
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
    program_dir: &std::path::Path,
    system: &System,
) -> Result<Arc<dyn flux_capabilities::DatasourceBackend>> {
    const DOC_EXTS: &[&str] = &[".md", ".txt", ".rst", ".adoc", ".mdx"];
    const MAX_DOCS: usize = 1000;
    const MAX_BYTES: usize = 200_000;
    // A datasource path is relative to the PROGRAM FILE's directory (absolute paths pass through), so
    // `path "./docs"` means "beside the .flux file" regardless of the launch cwd. `program_dir` is a
    // read-only root of `system`, so the resulting absolute path is walkable/readable.
    fn resolve_ds_path(program_dir: &std::path::Path, raw: &str) -> String {
        let p = std::path::Path::new(raw);
        if p.is_absolute() {
            raw.to_string()
        } else {
            program_dir.join(p).to_string_lossy().into_owned()
        }
    }
    let backend: Arc<dyn flux_capabilities::DatasourceBackend> =
        datasource_backend(Arc::new(flux_capabilities::MemoryBackend::new()));
    for d in decls {
        match d.kind.as_str() {
            "markdown" => {
                let base = resolve_ds_path(program_dir, d.path.as_deref().unwrap_or("."));
                let files = system.walk_files(&base, 4000).await.unwrap_or_default();
                let mut docs: Vec<(String, String)> = Vec::new();
                for f in files {
                    if docs.len() >= MAX_DOCS {
                        break;
                    }
                    if !DOC_EXTS.iter().any(|e| f.ends_with(e)) {
                        continue;
                    }
                    // Metadata size-check before the read, as in `build_doc_index`.
                    if !matches!(system.file_size(&f).await, Ok(n) if n as usize <= MAX_BYTES) {
                        continue;
                    }
                    if let Ok(text) = system.read_file(&f).await {
                        docs.push((f, text));
                    }
                }
                flux_capabilities::ingest_markdown(&*backend, &d.name, &docs)
                    .map_err(|e| anyhow::anyhow!("datasource `{}` (markdown): {e}", d.name))?;
            }
            "openapi" => {
                let raw = d.path.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("datasource `{}` (openapi) needs a `path`", d.name)
                })?;
                let path = resolve_ds_path(program_dir, raw);
                let text = system
                    .read_file(&path)
                    .await
                    .map_err(|e| anyhow::anyhow!("datasource `{}`: read {raw}: {e}", d.name))?;
                let spec: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                    anyhow::anyhow!("datasource `{}`: parse {raw} as OpenAPI JSON: {e}", d.name)
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
    match std::env::var("FLUX_COMPACT_CHARS") {
        Ok(s) => s.parse().unwrap_or_else(|_| {
            // Warn instead of silently reverting: the user set the knob, so a typo'd value
            // (`48k`) falling back to the default would contradict the documented 0-disables
            // contract without a trace.
            eprintln!(
                "{} FLUX_COMPACT_CHARS is not a number ({s:?}); using the default 48000",
                style::yellow("warning:")
            );
            48_000
        }),
        Err(_) => 48_000,
    }
}

/// Discover skills from the project's `.flux/skills` and `.claude/skills` plus the user/global dirs
/// (`~/.flux/skills`, `~/.agents/skills`, `~/.claude/skills`), with custom dirs layered above the
/// well-known set: `--skill-dir` flags first, then `[skills] dirs` from the layered config (project
/// before user) — earlier dirs win a name clash (L-02). Discovery reads metadata only. `enabled`
/// is the explicit `--skill NAME` allowlist; prompt text never activates a skill automatically.
fn load_skills(
    cwd: &std::path::Path,
    cfg: &flux_config::Config,
    cli_dirs: &[std::path::PathBuf],
    enabled: &[String],
) -> Result<Vec<flux_skill::Skill>> {
    // Manual-only means more than "discover everything, then select nothing": an ordinary turn
    // must not pay to walk every project and global skill directory. Discovery is only useful once
    // the caller has explicitly named at least one skill.
    if enabled.is_empty() {
        return Ok(Vec::new());
    }
    let mut extra: Vec<std::path::PathBuf> = cli_dirs.to_vec();
    extra.extend(cfg.skill_dir_paths());
    let discovered = flux_skill::discover_merged(&flux_skill::skill_dirs(cwd, &extra));
    let mut selected = Vec::new();
    for name in enabled {
        let skill = discovered
            .iter()
            .find(|skill| skill.name == *name)
            .ok_or_else(|| {
                let mut available: Vec<&str> =
                    discovered.iter().map(|skill| skill.name.as_str()).collect();
                available.sort_unstable();
                anyhow::anyhow!(
                    "unknown skill `{name}` (discovered: {})",
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                )
            })?;
        if !selected
            .iter()
            .any(|selected: &flux_skill::Skill| selected.name == skill.name)
        {
            selected.push(skill.clone());
        }
    }
    Ok(selected)
}

/// The plugin descriptor directory `~/.flux/plugins` (None if `HOME` is unset).
fn plugins_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".flux").join("plugins"))
}

const PLUGIN_LOAD_CONCURRENCY: usize = 16;

async fn collect_bounded<F, T>(futures: Vec<F>, limit: usize) -> Result<Vec<T>>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let permits = Arc::new(tokio::sync::Semaphore::new(limit.max(1)));
    let tasks = futures.into_iter().map(|future| {
        let permits = permits.clone();
        tokio::spawn(async move {
            let _permit = permits.acquire_owned().await.map_err(|error| {
                anyhow::anyhow!("plugin-load concurrency limiter closed: {error}")
            })?;
            Ok::<T, anyhow::Error>(future.await)
        })
    });
    let joined: Vec<_> = futures::stream::iter(tasks)
        .buffer_unordered(limit.max(1))
        .collect()
        .await;
    let mut values = Vec::with_capacity(joined.len());
    for result in joined {
        values.push(result.context("plugin-load task failed")??);
    }
    Ok(values)
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
                    // The fork session is always minted from `session_arg` — the session flags can't apply here
                    // and silently accepting them would suggest they did something.
    if flags.continue_ || flags.resume {
        bail!("`flux fork` always forks the given session — `--continue`/`--resume` don't apply");
    }
    if flags.agent_loop.is_some() {
        bail!("`--loop` selects complete agent turns and does not apply to `flux fork` tail continuation");
    }
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
    // parent's messages so an adaptive tail has the recorded context.
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
        // Mode B: a live turn on the forked session — the adaptive loop sees the copied
        // conversation plus the replayed prefix's symbols and continues through the full envelope.
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
            format!("{}…", s[..end].replace('\n', " "))
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

/// `flux loop [show|eject]` — inspect or copy the built-in adaptive Flux-Lang outer loop.
async fn run_loop_cmd(action: Option<LoopAction>) -> Result<()> {
    use flux_flow::engine::{agent_loop_source, builtin_agent_loop};

    let cwd = std::env::current_dir().context("current dir")?;
    match action.unwrap_or(LoopAction::Show) {
        LoopAction::Show => {
            let (_source, text) = agent_loop_source(&cwd);
            eprintln!("{} built-in adaptive preset", style::bold("source:"));
            eprintln!();
            // The loop text goes to stdout so `flux loop show` is pipeable.
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        LoopAction::Eject { force } => {
            let system =
                System::new(Workspace::new(&cwd).map_err(|error| anyhow::anyhow!("{error}"))?);
            let relative = ".flux/agent-loop.flux";
            let path = cwd.join(relative);
            if system
                .path_exists(relative)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?
                && !force
            {
                bail!(
                    "{} already exists — edit it directly, or pass --force to overwrite with the built-in",
                    path.display()
                );
            }
            system
                .write_file(relative, builtin_agent_loop())
                .await
                .map_err(|error| anyhow::anyhow!("write {}: {error}", path.display()))?;
            eprintln!(
                "{} {} — reference this file explicitly from an agent, app, role, or config",
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

/// L6 binding of the L5 [`flux_web::RecordSink`] seam: contributes the `web.page` records `web.fetch`
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
    /// Unresolved provider-local default model carried by the engine until first construction.
    default_model: String,
    cell: tokio::sync::OnceCell<(Box<dyn Provider>, String)>,
}

impl LazyProvider {
    fn new(spec: String) -> Self {
        let display = spec.split('/').next().unwrap_or("model").to_string();
        let default_model = spec
            .split_once('/')
            .map(|(_, model)| model.to_string())
            .unwrap_or_else(|| spec.clone());
        Self {
            spec,
            display,
            default_model,
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
        // The engine's inherited model is unresolved on this lazy path, so replace only that exact
        // default. An explicitly configured same-provider stage model is already provider-local and
        // must survive instead of being silently overwritten by the parent default.
        if req.model == self.default_model {
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
                thinking: None,
                effort: None,
                agent_loop: None,
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

/// The session-ambient group-surfacing signals known to the host at startup (D-115): `endpoint`
/// when the loaded endpoints store has records — so an operator who registered a Postgres
/// endpoint sees the endpoint ops without a kubeconfig. Computed from the startup-loaded registry
/// (an in-memory emptiness check), never by re-reading `~/.flux/endpoints.toml` per turn;
/// sticky-monotonic surfacing makes a startup-static answer sufficient.
fn session_ambient_signals(endpoints: &flux_capabilities::EndpointRegistry) -> Vec<String> {
    if endpoints.is_empty() {
        Vec::new()
    } else {
        vec!["endpoint".to_string()]
    }
}

/// Put a plugin's otherwise-ungrouped visible operations behind one turn-intent group. Explicit
/// manifest membership and per-op group tags remain authoritative; this only changes the legacy
/// `group = None` case that would otherwise classify hundreds of installed integration ops as core
/// and inject them into every adaptive model-stage request.
fn implicit_plugin_group(
    manifest: &flux_plugin::PluginManifest,
    specs: &[flux_spec::ToolSpec],
) -> Option<flux_evidence::ToolGroup> {
    let explicitly_grouped: std::collections::HashSet<&str> = manifest
        .groups
        .iter()
        .flat_map(|group| group.tools.iter().map(String::as_str))
        .collect();
    let mut tools: Vec<String> = specs
        .iter()
        .filter(|spec| spec.group.is_none() && !explicitly_grouped.contains(spec.name.as_str()))
        .map(|spec| spec.name.clone())
        .collect();
    tools.sort();
    tools.dedup();
    if tools.is_empty() {
        return None;
    }

    let intent = manifest.name.to_lowercase();
    let mut routing = std::collections::BTreeSet::from([intent.clone()]);
    routing.extend(
        manifest
            .capabilities
            .http_hosts
            .iter()
            .chain(
                manifest
                    .endpoints
                    .iter()
                    .flat_map(|endpoint| endpoint.http_hosts.iter()),
            )
            .map(|host| host.trim().trim_start_matches("*.").to_lowercase())
            .filter(|host| !host.is_empty()),
    );
    Some(flux_evidence::ToolGroup {
        name: format!("plugin.{intent}"),
        description: format!(
            "Operations from the live `{}` integration. Routing hints: {}.",
            manifest.name,
            routing.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        tools,
        surface_when: routing
            .into_iter()
            .map(|signal| flux_evidence::SignalMatch {
                kind: flux_evidence::KIND_TURN_INTENT.into(),
                signal: Some(signal),
            })
            .collect(),
    })
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

/// Build the workspace view used by every saved-flow consumer. Agent construction creates the two
/// global homes (preserving its existing behavior); read-only CLI listing/resolution merely
/// registers homes that already exist, so `flux flow list` has no session/provider side effects.
fn workspace_with_flow_roots(cwd: &std::path::Path, create_global: bool) -> Result<Workspace> {
    let mut workspace = Workspace::from_env(cwd).context("workspace")?;
    if let Some(home) = std::env::var_os("HOME") {
        let flux_dir = std::path::PathBuf::from(home).join(".flux");
        for (name, sub) in [("global_flows", "flows"), ("global_ops", "ops")] {
            let dir = flux_dir.join(sub);
            if create_global {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("create {}", dir.display()))?;
            }
            if dir.is_dir() {
                workspace
                    .add_named_root(name, &dir)
                    .with_context(|| format!("register {}", dir.display()))?;
            }
        }
    }
    Ok(workspace)
}

/// The D-130 sandbox posture resolved from the environment — the counterpart to
/// `workspace_with_flow_roots`'s custom [`Workspace`] construction. Call sites that build a
/// `System` from a hand-assembled workspace (rather than `System::from_env`) attach this via
/// `System::with_sandbox` so they still pick up `FLUX_SANDBOX`/`[sandbox]` like every other
/// production entry point.
fn resolved_sandbox() -> flux_system::sandbox::Sandbox {
    flux_system::sandbox::Sandbox::resolve(flux_system::sandbox::SandboxSettings::from_env())
}

/// Resolve an explicit outer-loop selector. The built-in preset needs no IO; a file is read through
/// the guarded workspace rather than by the engine probing a magic path behind the caller's back.
async fn resolve_agent_loop(selection: Option<&str>, system: &System) -> Result<AgentLoopSpec> {
    let Some(selection) = selection.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(AgentLoopSpec::default());
    };
    if selection.eq_ignore_ascii_case("adaptive") {
        return Ok(AgentLoopSpec::default());
    }
    let source = system
        .read_file(selection)
        .await
        .with_context(|| format!("read explicit agent loop `{selection}`"))?;
    AgentLoopSpec::parse(&source).map_err(|error| anyhow::anyhow!("{error}"))
}

async fn build_agent_with(
    flags: &AgentFlags,
    eager_provider: bool,
    session_override: Option<String>,
) -> Result<(FlowEngine, String, String, Arc<dyn flux_runtime::Spawner>)> {
    // Guarded system rooted at the current directory; layered config loaded from it.
    let cwd = std::env::current_dir().context("current dir")?;
    let cfg = flux_config::load(&cwd).context("load .flux/config.toml")?;
    // Validate this input-driven expansion bound before provider, plugin, or agent assembly work.
    let max_iterations = agent_max_iterations(flags, &cfg.agent)?;
    // Opt into the generic `bash` op when config enables it — via the runtime's in-process
    // override, NOT `set_var` (we're on a live multi-threaded runtime here). A user who set
    // `FLUX_ENABLE_BASH` directly is honored too (we only ever turn it on here, never off).
    if cfg.enable_shell {
        flux_runtime::set_shell_opt_in(true);
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

    // Global roots for agent-reusable definitions: `~/.flux/flows` is the home for flows +
    // composite ops (discovered by `flow_list`, run by `flow_run`, ops auto-loaded); `~/.flux/ops`
    // is the legacy location, still read during the ops→flows unification.
    let system = Arc::new(
        System::new(workspace_with_flow_roots(&cwd, true)?).with_sandbox(resolved_sandbox()),
    );

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
            .with_reasoning(flags.think, flags.effort.map(Into::into))
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
    // `mock` this is a fresh, hermetic `MockCliProvider`).
    let cog_provider: Option<Box<dyn Provider>> =
        if model_spec == "mock" || model_spec.starts_with("mock/") {
            Some(Box::<MockCliProvider>::default())
        } else if !eager_provider {
            // C-11: the lazy path must honor its "no credential read, no chain resolution at
            // startup" guarantee for the sibling too — an eager `provider_for` here made
            // `flux replay` (which advertises "no model call, no live IO") run the aws
            // credential chain over the network. Deferred like the engine's own provider; the
            // construction error, when a flow DOES call an ai.* op, is the same one the eager
            // path raises.
            Some(Box::new(LazyProvider::new(model_spec.clone())))
        } else {
            // Eager path: if the sibling can't be built we skip the pack rather than fail
            // startup — the rest of the agent is unaffected.
            match provider_for(&model_spec) {
                Ok(p) => Some(p),
                Err(e) => {
                    eprintln!(
                        "{}",
                        style::dim(&format!("(cognition pack not wired: {e})"))
                    );
                    None
                }
            }
        };
    if let Some(cog_provider) = cog_provider {
        flux_cognition::CognitionPack::new(Arc::from(cog_provider), model.clone())
            .with_reasoning(flags.think, flags.effort.map(Into::into))
            .register(&mut registry);
    }

    // Eval / self-improvement ops (the ones the improve flows orchestrate). Registered on the
    // top-level registry only — never on `sub_registry`, so worker sub-agents can't run eval/git ops.
    flux_eval::register_eval_ops(&mut registry);

    // Authored-loop stages are registered for `agent-loop.flux` but tagged to the never-surfaced
    // `reflect` group, so they stay OUT of native model catalogs. `op.register` remains model-facing
    // and delegates to the engine-installed composite registrar.
    flux_tools::register_reflect(&mut registry);

    // Flow discovery/run: `flow_list` (enumerate .flux/flows + ~/.flux/flows) and `flow_run`
    // (run a stored flow by name in the current session). Model-facing, so the agent can
    // discover and run authored flows.
    flux_tools::register_flows(&mut registry);

    // `flow_render`: Flux-Lang source/plan → syntax-highlighted SVG (source + tree views), for
    // surfaces that can't highlight .flux themselves (READMEs, Slack, docs, chat panels).
    flux_tools::register_render(&mut registry);

    // Auto-index workspace docs (markdown/text, capped & cheap) into the knowledge datasource, and
    // register the retrieval ops (`search`/`get`/`list`/`relation`/`batch_get`/`sources`). The
    // backend is also the sink `web.fetch` contributes `web.page` records to (below), so read pages
    // are groundable.
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

    // Native web capabilities (flux-web): `http.request` (tier 1), `web.fetch` + `html_to_markdown`
    // (tier 2), all under the family-wide `[private_net] web` egress scope. Registered here — after
    // the session is resolved — because the `PrivateNetAdmit` audit sink needs the event store +
    // session id, and `web.fetch` contributes `web.page` records to the datasource backend.
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
    // Session-ambient group-surfacing signals (D-115), computed below from the loaded endpoint
    // registry — the engine appends them to every turn's workspace-probed signals.
    let mut ambient_signals: Vec<String> = Vec::new();
    if let Some(dir) = plugins_dir() {
        // The cross-plugin endpoint-discovery broker (D-26/D-27): a registry of loaded plugins + the
        // shared endpoint registry, so a consumer plugin's `endpoint.discover` capability fans out to
        // providers, and (D-27) the broker is the host-side `ReferenceResolver` for ref-based IO +
        // gated cross-plugin credential resolution.
        let plugin_registry = Arc::new(flux_capabilities::PluginRegistry::new());
        let endpoint_registry = Arc::new(flux_capabilities::EndpointRegistry::with_path(
            flux_capabilities::EndpointRegistry::default_path().unwrap_or_default(),
        ));
        // A corrupt store must be HEARD, not swallowed: since D-115 the loaded registry decides
        // whether the endpoint group surfaces at all, so a parse failure silently costing the
        // operator their endpoint ops would be undebuggable — surface the "fix or remove it"
        // message and continue with an empty registry.
        if let Err(e) = endpoint_registry.load() {
            eprintln!(
                "{}",
                style::dim(&format!("(endpoints store not loaded: {e})"))
            );
        }
        // D-115: a non-empty endpoints store is session evidence the endpoint ops matter — an
        // operator who registered a Postgres endpoint sees them without a kubeconfig. Asked once
        // of the registry we JUST loaded (never a per-turn re-read of endpoints.toml); surfacing
        // is sticky-monotonic, so a startup-static answer is enough. Known gap: a store that
        // becomes non-empty mid-session (a pre-authored flow writing through the still-gated
        // ops, or an import from another terminal) doesn't surface the group until next session.
        // D-116: merge operator-declared `[[endpoint.static]]` bindings into the registry as
        // config-bound records BEFORE computing ambient signals, so a declaratively-wired endpoint
        // also surfaces the endpoint group (D-115) — identically to a `flux endpoint add` record.
        merge_static_endpoints(&endpoint_registry, &cfg);
        ambient_signals = session_ambient_signals(&endpoint_registry);
        let invoker = Arc::new(flux_capabilities::HostProviderInvoker::new(
            plugin_registry.clone(),
        ));
        // The static config resolver is the first link of the broker's resolver chain (D-116): it
        // binds every config-bound named ref — from `flux endpoint add` (persisted) or
        // `[[endpoint.static]]` (merged above) — plus its Env credential. Discovered `@endpoint/*`
        // refs resolve from the registry in the broker.
        let static_resolver = Arc::new(flux_capabilities::StaticResolver::new(
            system.clone(),
            endpoint_registry.config_bindings(),
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
        let (plugins, stale) = split_stale_plugins(flux_plugin::discover(&dir));
        warn_stale_plugins(&stale);
        let loads: Vec<_> = plugins
            .into_iter()
            .map(|p| {
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
                    let plugin_private_hosts =
                        effective_plugin_private_hosts(&cfg_for_caps, &m.name);
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
                async move {
                    let name = p.name.clone();
                    let loaded =
                        flux_plugin::load_plugin_tools(&system, &p.name, &p.descriptor, make_caps)
                            .await;
                    (name, loaded)
                }
            })
            .collect();
        let mut loaded_plugins = collect_bounded(loads, PLUGIN_LOAD_CONCURRENCY).await?;
        // Completion order is nondeterministic; registration order stays name-stable so prompt
        // catalogs and group merges remain cache-stable across invocations.
        loaded_plugins.sort_by(|a, b| a.0.cmp(&b.0));
        for (plugin_name, loaded) in loaded_plugins {
            match loaded {
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
                    let specs: Vec<flux_spec::ToolSpec> =
                        lp.tools.iter().map(|tool| tool.spec()).collect();
                    plugin_groups.extend(lp.manifest.groups.clone());
                    if let Some(group) = implicit_plugin_group(&lp.manifest, &specs) {
                        plugin_groups.push(group);
                    }
                    // The registered tools hold the host alive for the session.
                    for t in lp.tools {
                        registry.register(t);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        style::dim(&format!("(plugin `{plugin_name}` failed to load: {e})"))
                    )
                }
            }
        }
    }

    // Config-authored model stages are ordinary typed operations. Register them only after every
    // built-in/plugin operation is known so name collisions and missing gather-tool wiring fail at
    // startup instead of silently shadowing a live capability.
    let mut model_stages = std::collections::BTreeMap::new();
    for (name, stage) in &cfg.agent.stages {
        if name.trim().is_empty() || registry.get(name).is_some() {
            anyhow::bail!(
                "[agent.stages.{name}] must have a non-empty operation name that does not collide with a registered tool"
            );
        }
        if stage.max_tokens == 0 {
            anyhow::bail!("[agent.stages.{name}] max_tokens must be greater than zero");
        }
        for tool in &stage.tools {
            let registered = registry.get(tool).ok_or_else(|| {
                anyhow::anyhow!(
                    "[agent.stages.{name}] tool `{tool}` is not registered and wired on this CLI path"
                )
            })?;
            if !flux_flow::statically_gather_safe(registered.as_ref()) {
                anyhow::bail!(
                    "[agent.stages.{name}] tool `{tool}` is not statically gather-safe (it must be low-risk, side-effect-free, non-mutating, and not capture-only; freshness/non-cacheability is allowed)"
                );
            }
        }
        let effort = stage
            .effort
            .as_deref()
            .map(parse_effort)
            .transpose()
            .with_context(|| format!("[agent.stages.{name}] effort"))?;
        flux_tools::reflect::register_model_stage(
            &mut registry,
            name.clone(),
            format!("Run the configured `{name}` model stage."),
            stage.input_schema.clone(),
            stage.output_schema.clone(),
        );
        model_stages.insert(
            name.clone(),
            flux_flow::ModelStageDefinition {
                prompt: stage.prompt.clone(),
                input_schema: stage.input_schema.clone(),
                output_schema: stage.output_schema.clone(),
                model: stage.model.clone(),
                tools: stage.tools.clone(),
                max_tokens: stage.max_tokens,
                effort,
            },
        );
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

    let ctx = ToolContext::new(system.clone())
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
    // The audit record must show EVERY gating input: the workspace-probed signals AND the
    // session-ambient ones (D-115) the engine appends each turn — otherwise "why did this group
    // surface?" is unanswerable from startup evidence.
    executor.observe(flux_evidence::Observation::new(
        "project.signals",
        flux_evidence::Phase::Startup,
        serde_json::json!({ "signals": signals, "ambient": &ambient_signals }),
    ));

    let flow = open_flow_store(events.clone())?;
    // Assemble the engine: this installs the authored-loop host and loads the selected Flux-Lang
    // outer loop (the turn loop is Flux-Lang, not Rust).
    let spec = AgentSpec {
        model,
        system_prompt,
        skills: load_skills(&cwd, &cfg, &flags.skill_dirs, &flags.skills)?,
        max_tokens: flags.max_tokens,
        max_iterations,
        thinking: flags.think,
        effort: flags.effort.map(Into::into),
        agent_loop: resolve_agent_loop(
            flags
                .agent_loop
                .as_deref()
                .or(cfg.agent.loop_spec.as_deref()),
            system.as_ref(),
        )
        .await?,
        groups,
        adaptive_policy: adaptive_loop_policy(flags, &cfg.agent)?,
        ambient_signals,
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
    agent.loop_host.set_model_stages(model_stages);
    // Per-turn token ceiling (A-10), default OFF. Precedence: --turn-budget > FLUX_TURN_TOKEN_BUDGET
    // > config [limits] turn_token_budget. A malformed env value is a hard error, not a silent
    // fall-through: this is a spend/safety ceiling, and `FLUX_TURN_TOKEN_BUDGET=1_000_000` quietly
    // running unbounded is exactly the failure the ceiling exists to prevent.
    let env_budget = match std::env::var("FLUX_TURN_TOKEN_BUDGET") {
        Ok(v) => Some(v.trim().parse::<u64>().map_err(|e| {
            anyhow::anyhow!("FLUX_TURN_TOKEN_BUDGET is not a token count ({v:?}): {e}")
        })?),
        Err(_) => None,
    };
    let turn_budget = flags
        .turn_budget
        .or(env_budget)
        .or(cfg.limits.turn_token_budget);
    agent.loop_host.set_token_budget(turn_budget);
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
    let outcome = agent.run_turn(&session_id, &prompt, &mut sink).await;
    // Persist "always allow" choices made DURING the turn even when the turn itself later fails —
    // the user answered the prompt either way, and losing the choice means re-prompting next run.
    persist_new_rules(&initial_rules, &agent.executor.allow_rules());
    outcome.context("agent turn")?;
    Ok(())
}

/// `flux eval <adapter> [--tasks a,b] [--members a,b] [--limit N] [-m model] [--trials N]
/// [--report out.md] [--watch]` — run a benchmark suite ad-hoc through flux-eval and print a summary
/// (same adapters + scoring the `eval_run` op and improve loop use). `--watch` streams each task's
/// agent activity live; `--report` writes the categorized Markdown report.
#[allow(clippy::too_many_arguments)]
async fn run_eval_cmd(
    adapter: EvalAdapter,
    tasks: Vec<String>,
    members: Vec<String>,
    limit: u64,
    trials: u64,
    report_path: Option<String>,
    watch: bool,
    model: Option<String>,
) -> Result<()> {
    // `--members` only means something to the `multi` adapter — reject the pairing errors up
    // front instead of silently ignoring the list (or failing deep inside flux-eval).
    if adapter == EvalAdapter::Multi && members.is_empty() {
        bail!("the `multi` adapter needs `--members <adapter,adapter,…>` to combine");
    }
    if adapter != EvalAdapter::Multi && !members.is_empty() {
        bail!(
            "`--members` only applies to the `multi` adapter (got `{}`)",
            adapter.as_str()
        );
    }
    let mut params = serde_json::json!({
        "adapter": adapter.as_str(),
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
    flags: &ReviewFlags,
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
    // prompt-compiled plan, so `review` doesn't offer `--yes` at all (see [`ReviewFlags`]).
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

/// `flux render <file.flux> [--view source|tree] [-o out.svg]` (L-77) — the non-gated entry point
/// to the L-76 renderer, and the generator for flux's own doc images (replaces the
/// flux-tree-sitter repo's `scripts/render-example.mjs`). Builds the workspace from the
/// environment like every production construction site, then delegates to [`run_render_in`].
async fn run_render(file: &str, view: RenderView, out: Option<&str>) -> Result<()> {
    let system = System::from_env(std::env::current_dir()?).map_err(|e| anyhow::anyhow!("{e}"))?;
    run_render_in(&system, file, view, out).await
}

/// The testable core of `flux render`. The INPUT is read like the sibling file-input subcommands
/// (`flow run`, `app run`): a plain filesystem read relative to the invocation cwd, so `../` and
/// absolute paths work — only the `-o` WRITE is workspace-confined (through `System::write_file`;
/// SVG is text, parents are created). A UTF-8 BOM is stripped before parsing (a PowerShell/
/// Notepad-authored file would otherwise fail the parser with an invisible U+FEFF in the first
/// token). Without `out` the SVG streams to stdout; an early-closing consumer (`flux render
/// x.flux | head`) never panics — on Unix the process ends with the conventional SIGPIPE exit
/// (`main` resets `SIG_DFL`, A-61), on Windows the `BrokenPipe` write error is treated as
/// success. A hard parse error in `tree` view propagates — the CLI exits non-zero with the
/// parser's message — while `source` view is total.
async fn run_render_in(
    system: &System,
    file: &str,
    view: RenderView,
    out: Option<&str>,
) -> Result<()> {
    let source = std::fs::read_to_string(file).with_context(|| format!("read {file}"))?;
    let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
    let svg = flux_tools::render::render_flux_svg(source, view.into())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    match out {
        Some(path) => {
            system
                .write_file(path, &svg)
                .await
                .map_err(|e| anyhow::anyhow!("write {path}: {e}"))?;
            let view_word = match view {
                RenderView::Source => "source",
                RenderView::Tree => "tree",
            };
            eprintln!("rendered {file} ({view_word} view) → {path}");
        }
        None => {
            use std::io::Write;
            // Not `println!`: a consumer that stops reading early (`| head`, a converter erroring
            // out) must not turn the write into a panic. On Unix this arm is normally moot —
            // `main`'s A-61 `reset_sigpipe` restores `SIG_DFL`, so the process ends on SIGPIPE
            // (conventional exit 141, like `cat`) before the write ever returns EPIPE. The arm IS
            // the path on Windows (no SIGPIPE — the closed pipe surfaces as a BrokenPipe io
            // error) and under std's default SIG_IGN (unit tests). A broken pipe means the
            // consumer has everything it wants — exit cleanly.
            let mut stdout = std::io::stdout();
            match stdout
                .write_all(svg.as_bytes())
                .and_then(|()| stdout.flush())
            {
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
                r => r.context("write SVG to stdout")?,
            }
        }
    }
    Ok(())
}

struct LoadedCliFlow {
    ast: flux_flow::ast::DraftAst,
    composites: Vec<flux_lang::program::CompositeOpDecl>,
}

/// `flux flow list` / `ls`: discovery only. This deliberately constructs just a guarded `System`
/// and the shared catalog — no provider, event store, session, plugin process, or agent engine.
fn run_flow_list() -> Result<()> {
    let cwd = std::env::current_dir().context("current dir")?;
    let system =
        System::new(workspace_with_flow_roots(&cwd, false)?).with_sandbox(resolved_sandbox());
    println!("{}", flux_tools::StoredFlowCatalog::load(&system).render());
    Ok(())
}

/// Parse an existing path using the long-standing file semantics. JSON DraftAst files remain
/// supported; a native module path must still select exactly one flow/journey.
fn parse_cli_flow_source(label: &str, source: &str) -> Result<LoadedCliFlow> {
    if source.trim_start().starts_with('{') {
        return Ok(LoadedCliFlow {
            ast: serde_json::from_str(source)
                .with_context(|| format!("parse {label} as a Flux-Lang DraftAst (JSON)"))?,
            composites: Vec::new(),
        });
    }
    match flux_lang::program::Module::parse_str(source)
        .map_err(|e| anyhow::anyhow!("parse {label} as Flux-Lang text: {e}"))?
    {
        flux_lang::program::Module::Flow(ast) => Ok(LoadedCliFlow {
            ast,
            composites: Vec::new(),
        }),
        flux_lang::program::Module::Program(program) => {
            let ast = match (program.flows.as_slice(), program.journeys.as_slice()) {
                ([flow], []) => flow.clone(),
                ([], [journey]) => journey.flow.clone(),
                _ => bail!(
                    "`flux flow run` needs a bare flow or a module with exactly one flow/journey"
                ),
            };
            Ok(LoadedCliFlow {
                ast,
                composites: program.ops,
            })
        }
    }
}

/// Resolve the positional target as a real file first, then as a saved-flow filename stem or
/// declaration. Saved-name runs do not return their file's ops as module-local declarations: those
/// ops are already in the engine's auto-loaded composite snapshot and must be installed once.
fn load_cli_flow_target(target: &str) -> Result<LoadedCliFlow> {
    if std::path::Path::new(target).is_file() {
        let source =
            std::fs::read_to_string(target).with_context(|| format!("read flow {target}"))?;
        return parse_cli_flow_source(target, &source);
    }

    let cwd = std::env::current_dir().context("current dir")?;
    let system =
        System::new(workspace_with_flow_roots(&cwd, false)?).with_sandbox(resolved_sandbox());
    let resolved = flux_tools::StoredFlowCatalog::load(&system)
        .resolve(target)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(LoadedCliFlow {
        ast: resolved.ast,
        composites: Vec::new(),
    })
}

fn validate_flow_input_value(
    key: &str,
    value: &serde_json::Value,
    ty: &flux_lang::ast::TypeRef,
) -> Result<()> {
    use flux_lang::ast::TypeRef;
    let valid = match ty {
        TypeRef::Any | TypeRef::Named(_) => true,
        TypeRef::Bool => value.is_boolean(),
        TypeRef::Number => value.is_number(),
        TypeRef::String => value.is_string(),
        TypeRef::List(inner) => value
            .as_array()
            .is_some_and(|items| items.iter().all(|item| value_matches_type(item, inner))),
    };
    if valid {
        Ok(())
    } else {
        bail!(
            "input `{key}` expects {}, got {}",
            ty.label(),
            json_value_kind(value)
        )
    }
}

fn value_matches_type(value: &serde_json::Value, ty: &flux_lang::ast::TypeRef) -> bool {
    use flux_lang::ast::TypeRef;
    match ty {
        TypeRef::Any | TypeRef::Named(_) => true,
        TypeRef::Bool => value.is_boolean(),
        TypeRef::Number => value.is_number(),
        TypeRef::String => value.is_string(),
        TypeRef::List(inner) => value
            .as_array()
            .is_some_and(|items| items.iter().all(|item| value_matches_type(item, inner))),
    }
}

fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "Bool",
        serde_json::Value::Number(_) => "Number",
        serde_json::Value::String(_) => "String",
        serde_json::Value::Array(_) => "List",
        serde_json::Value::Object(_) => "object",
    }
}

/// Coerce one final (last-wins) `--arg` value from its declared TypeRef. Any/named values accept
/// either JSON or plain text; concrete scalar/list types are deliberately strict.
fn coerce_flow_arg(
    key: &str,
    raw: &str,
    ty: &flux_lang::ast::TypeRef,
) -> Result<serde_json::Value> {
    use flux_lang::ast::TypeRef;
    let value = match ty {
        TypeRef::String => serde_json::Value::String(raw.to_string()),
        TypeRef::Any | TypeRef::Named(_) => {
            serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
        }
        TypeRef::Number | TypeRef::Bool | TypeRef::List(_) => serde_json::from_str(raw)
            .with_context(|| format!("--arg {key} expects {} JSON", ty.label()))?,
    };
    validate_flow_input_value(key, &value, ty)?;
    Ok(value)
}

fn mapper_schema(params: &[flux_lang::ast::Param]) -> serde_json::Value {
    let properties: serde_json::Map<String, serde_json::Value> = params
        .iter()
        .map(|param| (param.name.0.clone(), schema_for_type(&param.ty)))
        .collect();
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": params.iter().map(|param| param.name.0.clone()).collect::<Vec<_>>(),
    })
}

fn schema_for_type(ty: &flux_lang::ast::TypeRef) -> serde_json::Value {
    use flux_lang::ast::TypeRef;
    match ty {
        TypeRef::Any => serde_json::json!({}),
        TypeRef::Bool => serde_json::json!({"type": "boolean"}),
        TypeRef::Number => serde_json::json!({"type": "number"}),
        TypeRef::String => serde_json::json!({"type": "string"}),
        TypeRef::List(inner) => {
            serde_json::json!({"type": "array", "items": schema_for_type(inner)})
        }
        TypeRef::Named(name) => {
            serde_json::json!({"description": format!("Flux value of type {name}")})
        }
    }
}

fn used_flow_symbols(ast: &flux_flow::ast::DraftAst) -> std::collections::HashSet<String> {
    use flux_flow::ast::Node;
    let mut used: std::collections::HashSet<String> = ast
        .params
        .iter()
        .map(|param| param.name.0.clone())
        .collect();
    flux_lang::analyze::for_each_node(&ast.body, &mut |node| match node {
        Node::Bind { name, .. }
        | Node::Memo { name, .. }
        | Node::Peek { name }
        | Node::Var { name } => {
            used.insert(name.0.clone());
        }
        Node::Each { item, collect, .. } => {
            used.insert(item.0.clone());
            if let Some(name) = collect {
                used.insert(name.0.clone());
            }
        }
        Node::Repeat {
            collect: Some(name),
            ..
        } => {
            used.insert(name.0.clone());
        }
        Node::Pipe { bind, .. }
        | Node::Seq { bind, .. }
        | Node::Retry { bind, .. }
        | Node::Loop { bind, .. }
        | Node::Fallback { bind, .. }
        | Node::Timeout { bind, .. }
        | Node::Budget { bind, .. }
        | Node::CapScope { bind, .. }
        | Node::Scope { bind, .. }
        | Node::Once { bind, .. } => {
            if let Some(name) = bind {
                used.insert(name.0.clone());
            }
        }
        Node::Race { bind, branches, .. } => {
            if let Some(name) = bind {
                used.insert(name.0.clone());
            }
            used.extend(branches.iter().map(|branch| branch.name.0.clone()));
        }
        Node::Try {
            catch: Some(name), ..
        } => {
            used.insert(name.0.clone());
        }
        Node::Await {
            binding: Some(name),
            ..
        } => {
            used.insert(name.0.clone());
        }
        Node::Parallel { branches } => {
            used.extend(branches.iter().map(|branch| branch.name.0.clone()));
        }
        Node::Ctx {
            name,
            include,
            exclude,
            ..
        } => {
            used.insert(name.0.clone());
            used.extend(include.iter().chain(exclude).map(|name| name.0.clone()));
        }
        Node::CtxAppend { ctx, add } => {
            used.insert(ctx.0.clone());
            used.extend(add.iter().map(|name| name.0.clone()));
        }
        _ => {}
    });
    used
}

fn fresh_mapper_symbol(
    base: &str,
    used: &mut std::collections::HashSet<String>,
) -> flux_lang::ast::SymbolName {
    let mut candidate = base.to_string();
    let mut suffix = 0usize;
    while used.contains(&candidate) {
        suffix += 1;
        candidate = format!("{base}_{suffix}");
    }
    used.insert(candidate.clone());
    candidate.into()
}

/// Lower opt-in natural-language mapping into ordinary, recorded Flux nodes. Strict `jq` field
/// reads make a missing field/non-object fatal before the original body begins; bind annotations
/// retain each declared TypeRef in the plan.
fn mapper_nodes(
    ast: &flux_flow::ast::DraftAst,
    missing: &[flux_lang::ast::Param],
    text: &str,
) -> Result<Vec<flux_flow::ast::Node>> {
    use flux_flow::ast::{FlowEffect, Node, TypeRef};
    let mut used = used_flow_symbols(ast);
    let raw = fresh_mapper_symbol("__flux_map_raw", &mut used);
    let parsed = fresh_mapper_symbol("__flux_map_json", &mut used);
    let object = fresh_mapper_symbol("__flux_map_args", &mut used);
    let schema =
        serde_json::to_string(&mapper_schema(missing)).context("serialize input schema")?;

    let call_fields = [
        (
            "ask".to_string(),
            Box::new(Node::Lit {
                value: serde_json::Value::String(
                    "Extract exactly one argument object for the requested flow parameters. Return a JSON array containing exactly that one object and no prose."
                        .into(),
                ),
            }),
        ),
        (
            "from".to_string(),
            Box::new(Node::Lit {
                value: serde_json::Value::String(text.to_string()),
            }),
        ),
        (
            "schema".to_string(),
            Box::new(Node::Lit {
                value: serde_json::Value::String(schema),
            }),
        ),
    ]
    .into_iter()
    .collect();

    let mut nodes = vec![
        Node::Bind {
            name: raw.clone(),
            value: Box::new(Node::Call {
                op: "ai.extract".into(),
                args: vec![Node::Obj {
                    fields: call_fields,
                }],
            }),
            ty: Some(TypeRef::String),
            effect: Some(FlowEffect::Model),
        },
        Node::Bind {
            name: parsed.clone(),
            value: Box::new(Node::Parse {
                value: Box::new(Node::Var { name: raw }),
                as_type: "json".into(),
            }),
            ty: Some(TypeRef::List(Box::new(TypeRef::Any))),
            effect: None,
        },
        Node::Assert {
            cond: Box::new(Node::Expr {
                formula: "len(items) == 1".into(),
                vars: [(
                    "items".to_string(),
                    Box::new(Node::Var {
                        name: parsed.clone(),
                    }),
                )]
                .into_iter()
                .collect(),
            }),
            message: Some(
                "--map-inputs must return exactly one argument object in a JSON array".into(),
            ),
        },
        Node::Bind {
            name: object.clone(),
            value: Box::new(Node::Jq {
                path: "[0]".into(),
                input: Box::new(Node::Var { name: parsed }),
                optional: false,
            }),
            ty: Some(TypeRef::Any),
            effect: None,
        },
    ];
    nodes.extend(missing.iter().map(|param| Node::Bind {
        name: param.name.clone(),
        value: Box::new(Node::Jq {
            path: format!(".{}", param.name.0),
            input: Box::new(Node::Var {
                name: object.clone(),
            }),
            optional: false,
        }),
        ty: Some(param.ty.clone()),
        effect: None,
    }));
    Ok(nodes)
}

/// Apply the CLI-only strict parameter contract and prepend the normalized AST nodes. Merge order:
/// mapper base, then `--inputs`, then repeatable `--arg` (last duplicate wins).
fn prepare_cli_flow_inputs(
    ast: &mut flux_flow::ast::DraftAst,
    inputs: Option<&str>,
    args: &[String],
    map_inputs: Option<&str>,
) -> Result<()> {
    let mut deterministic = match inputs {
        Some(raw) => {
            let value: serde_json::Value = serde_json::from_str(raw)
                .with_context(|| "--inputs must be a valid JSON object")?;
            value
                .as_object()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("--inputs must be a JSON object"))?
        }
        None => serde_json::Map::new(),
    };

    // Preserve last-wins semantics even when an earlier duplicate is malformed for the declared
    // type: only the final raw value is coerced.
    let mut raw_args = std::collections::BTreeMap::new();
    for arg in args {
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--arg expects KEY=VALUE (got `{arg}`)"))?;
        if key.is_empty() {
            bail!("--arg expects a non-empty key in KEY=VALUE");
        }
        raw_args.insert(key.to_string(), value.to_string());
    }

    let declared: std::collections::HashMap<&str, &flux_lang::ast::Param> = ast
        .params
        .iter()
        .map(|param| (param.name.0.as_str(), param))
        .collect();
    let unknown: std::collections::BTreeSet<String> = deterministic
        .keys()
        .chain(raw_args.keys())
        .filter(|key| !declared.contains_key(key.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        bail!(
            "unknown flow input parameter(s): {} — declared parameters: {}",
            unknown.into_iter().collect::<Vec<_>>().join(", "),
            ast.params
                .iter()
                .map(|param| param.name.0.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    for (key, raw) in raw_args {
        let param = declared[&key.as_str()];
        deterministic.insert(key.clone(), coerce_flow_arg(&key, &raw, &param.ty)?);
    }
    for param in &ast.params {
        if let Some(value) = deterministic.get(&param.name.0) {
            validate_flow_input_value(&param.name.0, value, &param.ty)?;
        }
    }

    let missing: Vec<flux_lang::ast::Param> = ast
        .params
        .iter()
        .filter(|param| !deterministic.contains_key(&param.name.0))
        .cloned()
        .collect();
    if !missing.is_empty() && map_inputs.is_none() {
        bail!(
            "missing required flow parameter(s): {} — pass --inputs, --arg, or opt in with --map-inputs",
            missing
                .iter()
                .map(|param| format!("{} ({})", param.name.0, param.ty.label()))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let mut prefix = Vec::new();
    // If deterministic overlays cover the whole contract, skip the mapper (and therefore the model)
    // even when --map-inputs was supplied.
    if !missing.is_empty() {
        if let Some(text) = map_inputs {
            prefix.extend(mapper_nodes(ast, &missing, text)?);
        }
    }
    prefix.extend(ast.params.iter().filter_map(|param| {
        deterministic
            .get(&param.name.0)
            .map(|value| flux_flow::ast::Node::Bind {
                name: param.name.clone(),
                value: Box::new(flux_flow::ast::Node::Lit {
                    value: value.clone(),
                }),
                ty: Some(param.ty.clone()),
                effect: None,
            })
    }));
    prefix.append(&mut ast.body);
    ast.body = prefix;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_flow(
    target: &str,
    inputs: Option<String>,
    args: Vec<String>,
    map_inputs: Option<String>,
    model: Option<String>,
    yes: bool,
    resumable: bool,
    resume: Option<String>,
    resume_value: Option<String>,
) -> Result<()> {
    let LoadedCliFlow {
        mut ast,
        composites,
    } = load_cli_flow_target(target)?;
    prepare_cli_flow_inputs(&mut ast, inputs.as_deref(), &args, map_inputs.as_deref())?;

    // Build the agent only after target/input validation, so malformed deterministic input cannot
    // create a session and no flow effect can run before the strict contract passes.
    let flags = AgentFlags::from_model_yes(model.as_deref(), yes);
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
/// `flux flow run <name|file>` and `flux preset <name> --run`. Builds the agent, validates the flow
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
        // A recording failure (locked/full events.db) must be VISIBLE at record time — silently
        // dropping it would only surface later as replay's "no stored plan_source … skipped",
        // with the cause long gone.
        let recorded = engine
            .events
            .begin_turn(&session_id, "<flow run>", &engine.model)
            .and_then(|turn_id| {
                let source = flux_lang::format::format(ast);
                let redactor = &engine.executor.context().redactor;
                engine.events.record_plan_attempt(
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
                )
            });
        if let Err(e) = recorded {
            eprintln!(
                "{} this run won't be replayable — recording the plan failed: {e}",
                style::yellow("warning:")
            );
        }
    }
    engine
        .composites
        .ensure_session_loaded(&engine.flow, &session_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut active_composites = engine.composites.active_for_session(&session_id);
    // A path-loaded module owns its local declarations. If that path also lives under a flows home,
    // the same ops are already present in the auto-loaded snapshot; remove those copies before
    // installing the explicit declarations so module-local ops shadow rather than collide. A
    // saved-NAME run passes no explicit declarations and therefore uses the auto-loaded copy once.
    let explicit_names: std::collections::HashSet<&str> =
        composites.iter().map(|op| op.name.as_str()).collect();
    active_composites.retain(|op| !explicit_names.contains(op.name.as_str()));
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
    // A store read error must propagate — swallowed into an empty set it would resurface as a
    // bogus "unbound symbol" diagnostic on resume, pointing at the flow instead of the store.
    let session_symbols: std::collections::HashSet<String> = engine
        .flow
        .view(&session_id)
        .map(|v| v.symbols.into_iter().map(|s| s.name.0).collect())
        .map_err(|e| anyhow::anyhow!("read session symbols from flow store: {e}"))?;
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

    // Denied-statement resume guard: a statement policy or the user already
    // refused must never be silently re-dispatched just because it re-appears unchanged in a
    // corrected file. Checked BEFORE executing anything.
    if let Some(open) = &open_halt {
        if flux_flow::runtime::denied_resume_guard(&ast.body, &open.halt) {
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

    // Point the installed loop host at this run's session + sink. A flow may call `ai_segment` or
    // `flow_run`; the shared sink keeps nested stage and operation events on one surface.
    let shared: Arc<std::sync::Mutex<dyn AgentSink>> = Arc::new(std::sync::Mutex::new(
        CliSink::new(0).with_cost(model_spec, flux_credentials::load_pricing_table()),
    ));
    // `None` advertised set: this is the pre-authored `flow run` path, which is deliberately
    // unrestricted by surfacing because the authored file names its operations explicitly.
    engine.loop_host.set_turn(
        session_id.clone(),
        Some(engine.system_prompt.clone()),
        shared.clone(),
        None,
        None,
    );

    let mut sink = flux_flow::loop_host::SharedSink::new(shared.clone());
    let outcome = if resumable {
        // A failing top-level statement reifies onto `outcome.failure` instead of
        // propagating `Err`; `open_halt`'s ledger (when resuming) fast-forwards the matching prefix.
        flux_flow::runtime::execute_flow_resumable_with_composites(
            engine.flow.as_ref(),
            engine.executor.as_ref(),
            &session_id,
            ast,
            &active_composites,
            open_halt.as_ref().map(|o| &o.ledger),
            &mut sink,
        )
        .await
    } else {
        // Also the no-composites case (empty slice is equivalent): this entry point self-wires
        // the C-43 cassette scope from the store — plain `execute_flow` deliberately does not
        // (it is shared with the outer agent loop, whose host stages are never cassetted).
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
    // reached a model op via `ai_segment` reports its real spend.
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
/// bare `h:<hash>` prefix could match ANY unnamed halted flow, including a host-derived action flow
/// from an agent turn, since they share the same session store and ledger machinery), so `last`
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

/// Whether *every* analyzer diagnostic is an unknown-op error (message shape `unknown operation: …`).
/// Picks an accurate header: a validation failure of another class (bad arg, arity, type/shape,
/// composability, unbound symbol, …) must not be filed under "references unknown operations" (A-62 /
/// F-010) — that header misleads both the reader and any model stage that reads diagnostics back to
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
/// (`token` already carries the `FLUX_A2A_TOKEN` fallback — clap owns that env wiring.)
async fn run_a2a(url: String, prompt_words: Vec<String>, token: Option<String>) -> Result<()> {
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
            // Read piped stdin off the runtime thread (the codebase convention — see
            // `read_stdin_line`): a fifo that never closes must not park a worker forever.
            tokio::task::spawn_blocking(|| {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).map(|_| buf)
            })
            .await
            .context("stdin reader task")??
            .trim()
            .to_string()
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

    loop {
        let prompt = FluxPrompt;
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
                "shell" => {
                    // Toggle the generic `bash` op for the session via the runtime's in-process
                    // override — mid-session `set_var`/`remove_var` would race worker-thread
                    // `getenv`s (UB on glibc). Takes effect from the next turn (the advertised
                    // catalog is recomputed per turn from `detect_signals`).
                    let currently_on = flux_runtime::shell_opt_in();
                    flux_runtime::set_shell_opt_in(!currently_on);
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
                                match agent.switch_model_for_session(&session_id, provider, model) {
                                    Ok(()) => eprintln!("switched to {}", agent.model),
                                    Err(error) => {
                                        eprintln!("cannot persist model switch: {error}")
                                    }
                                }
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
                    // Don't `?`-abort the REPL on a store error: that would also skip the
                    // loop-exit `persist_new_rules`, silently dropping every "always allow"
                    // choice granted this session. Report and keep the current session instead.
                    match agent.events.create_session(&agent.model) {
                        Ok(sid) => {
                            session_id = sid;
                            eprintln!("started new session {session_id}");
                        }
                        Err(e) => eprintln!("{} new session: {e}", style::red("error:")),
                    }
                }
                other => eprintln!("unknown command /{other} (try /help)"),
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
    flux_system::env_truthy("FLUX_VERBOSE")
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

/// A compact, readable label for an authored-loop operation shown when
/// `--show-loop` reveals the loop. Returns `None` for ordinary ops (which fall through to the normal
/// label path). These ops carry large inputs, so the label deliberately omits the payload.
fn loop_machinery_label(name: &str, input: &Value) -> Option<String> {
    let (verb, note) = match name {
        "detect_intent" => ("intent", "classify the request"),
        "explore" => ("explore", "gather / propose actions"),
        "approve_batch" => ("approve", "freeze the action batch"),
        "execute_batch" => ("execute", "run approved actions"),
        "present_results" => ("present", "render the result"),
        "ai_segment" => ("AI segment", "bounded model stage"),
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
    // Column width: wide enough for the longest built-in op name (`web.fetch` = 9).
    const GUTTER: usize = 10;
    const ARG_CAP: usize = 120;
    // The loop machinery (revealed by `--show-loop`) may carry large typed state values.
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

fn format_operation_timing(timing: flux_core::OperationTiming) -> String {
    let fmt = |micros| style::fmt_elapsed(std::time::Duration::from_micros(micros));
    match (timing.execution_us, timing.approval_wait_us) {
        (Some(execution), Some(approval)) => {
            format!("exec {} + approval {}", fmt(execution), fmt(approval))
        }
        (Some(execution), None) => format!("exec {}", fmt(execution)),
        (None, Some(approval)) => format!("approval {}", fmt(approval)),
        (None, None) => format!("dispatch {}", fmt(timing.total_us)),
    }
}

fn format_model_call(o: &flux_evidence::Observation) -> String {
    let stage = o
        .data
        .get("stage")
        .and_then(Value::as_str)
        .unwrap_or("model");
    let round = o.data.get("round").and_then(Value::as_u64).unwrap_or(0);
    let duration = o
        .data
        .get("duration_us")
        .and_then(Value::as_u64)
        .map(std::time::Duration::from_micros)
        .map(style::fmt_elapsed)
        .unwrap_or_else(|| "?".into());
    let ttft = o
        .data
        .get("ttft_us")
        .and_then(Value::as_u64)
        .map(std::time::Duration::from_micros)
        .map(style::fmt_elapsed)
        .unwrap_or_else(|| "n/a".into());
    let operations = o
        .data
        .get("operations")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let schema_bytes = o
        .data
        .get("schema_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let schema = if schema_bytes >= 1024 {
        format!("{:.1} KiB", schema_bytes as f64 / 1024.0)
    } else {
        format!("{schema_bytes} B")
    };
    format!(
        "◇ model {stage} #{round} · {duration} · ttft {ttft} · {operations} op{} · {schema} schema",
        if operations == 1 { "" } else { "s" }
    )
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
    /// Dispatcher-attributed phases for the pending op, delivered immediately before its result.
    pending_timing: Option<flux_core::OperationTiming>,
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
    /// The phase of the most recent `loop.phase` observation this turn. Current adaptive stages use
    /// `intent`/`explore`; historical sessions may still project `orient`/`gather`/`execute`.
    /// Drives the spinner label via `phase_spinner_label`.
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
            pending_timing: None,
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
        // Fill an otherwise-silent provider wait with a phase-aware spinner. The intent/exploration
        // observation replaces it once the typed model stage completes.
        if active {
            self.turn_start.get_or_insert_with(std::time::Instant::now);
            self.commit();
            let label = phase_spinner_label(self.phase.as_deref(), self.execute_rounds);
            if self.use_spinner() {
                self.start_spinner(style::dim(&label));
            } else if matches!(self.phase.as_deref(), Some("intent" | "explore")) {
                // Redirected runs have no animated line to rewrite. Preserve one stable
                // phase marker per provider consultation so logs and CI output do not reproduce
                // the otherwise-silent wait that A-72 closes for interactive terminals.
                eprintln!("{}", style::dim(&label));
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
        self.pending_timing = None;
    }
    fn tool_timing(&mut self, _name: &str, timing: &flux_core::OperationTiming) {
        self.pending_timing = Some(*timing);
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
        let elapsed = self
            .pending_timing
            .take()
            .map(format_operation_timing)
            .unwrap_or_else(|| style::fmt_elapsed(start.elapsed()));
        let elapsed = style::dim(&format!("· {elapsed}"));
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
        } else if o.kind == "model.call" && flux_flow::engine::show_loop() {
            eprintln!("{}", style::dim(&format_model_call(o)));
        } else if o.kind == "loop.phase" {
            self.record_phase(o);
        } else if o.kind == flux_evidence::KIND_TURN_INTENT
            && o.data.get("intent").and_then(|v| v.as_str()).is_some()
        {
            self.render_intent(o);
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

    /// Render a `flow.halt` observation: a red one-liner marking exactly where guarded execution
    /// halted before the execution report returns to the native stage for correction.
    fn render_halt(&self, o: &flux_evidence::Observation) {
        eprintln!("{}", style::red(&halt_line(&o.data)));
    }

    /// Track a `loop.phase` observation so the spinner names the current typed stage. Historical
    /// gather/execute values remain readable when old sessions are projected.
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
            "orient" | "intent" | "explore" => self.gather_mode = false,
            _ => {}
        }
        self.phase = Some(phase);
    }

    /// Render A-72's accepted staged intent from the already-durable `turn.intent` observation.
    /// Keyword-derived turn signals use the same observation kind but carry only `signal`, so the
    /// caller filters those out. Normal output stays compact; `-v` adds the exact selected ops.
    fn render_intent(&self, o: &flux_evidence::Observation) {
        for (index, line) in intent_lines(&o.data, self.verbose, self.width)
            .into_iter()
            .enumerate()
        {
            if index == 0 {
                eprintln!("{}", style::cyan(&line));
            } else {
                eprintln!("{}", style::dim(&line));
            }
        }
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
/// fired the moment an execution flow halts, distinct from this spinner label. A phase-less caller
/// falls back to "working…".
fn phase_spinner_label(phase: Option<&str>, execute_rounds: usize) -> String {
    match phase {
        Some("intent") => "routing intent…".to_string(),
        Some("explore") => "exploring…".to_string(),
        Some("orient") => "orienting…".to_string(),
        Some("gather") => "gathering…".to_string(),
        Some("execute") => {
            if execute_rounds > 1 {
                "revising…".to_string()
            } else {
                "planning…".to_string()
            }
        }
        _ => "working…".to_string(),
    }
}

/// Format the accepted staged intent as bounded, stable plain lines. The intent is model-authored,
/// so whitespace is collapsed before display; families and operation names are host-validated.
fn intent_lines(data: &Value, verbose: bool, width: usize) -> Vec<String> {
    let raw_intent = data
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let sanitized: String = raw_intent
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let intent = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    let intent_cap = width.saturating_sub(12).clamp(24, 160);
    let families = data
        .get("families")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let operations = data
        .get("operations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let capabilities = if families.is_empty() {
        "none".to_string()
    } else {
        families.join(", ")
    };
    let plural = if operations.len() == 1 {
        "operation"
    } else {
        "operations"
    };
    let mut lines = vec![
        format!("◆ intent: {}", truncate(&intent, intent_cap)),
        format!(
            "  capabilities: {capabilities} · {} {plural}",
            operations.len()
        ),
    ];
    if verbose && !operations.is_empty() {
        lines.push(format!("  operations: {}", operations.join(", ")));
    }
    lines
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
/// the action execution report is built — a real-time cue distinct from the per-tool ✓/✗ markers.
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

/// A built-in offline provider (`-m mock`) that speaks the same adaptive native-tool protocol as a
/// live model: declare intent, capture one literal operation, finalize its action batch, then answer
/// from the guarded execution report. This keeps the offline gate on the product-default loop rather
/// than preserving a second mock-only agent-loop path.
#[derive(Default)]
struct MockCliProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for MockCliProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(&self, req: Request) -> flux_core::Result<ChunkStream> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed);

        // Test hook: `FLUX_MOCK_HANG=1` streams one delta then never completes (only cancellation
        // can end the turn) — used to exercise Ctrl-C interruption in the REPL.
        if std::env::var("FLUX_MOCK_HANG").is_ok() {
            let s = futures::stream::once(async { Ok(Chunk::TextDelta("thinking…".into())) })
                .chain(futures::stream::pending::<flux_core::Result<Chunk>>());
            return Ok(Box::pin(s));
        }

        // Test hook for direct model-backed cognition ops (not the adaptive outer loop): return a canned
        // text completion. L-79 uses this to exercise `ai.extract` input mapping through the real
        // binary without provider credentials or a network stub.
        if let Ok(text) = std::env::var("FLUX_MOCK_RESPONSE") {
            let chunks = vec![
                Chunk::TextDelta(text),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ];
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }

        let target = std::env::var("FLUX_MOCK_TOOL")
            .ok()
            .or_else(|| std::env::var("FLUX_MOCK_BASH").ok().map(|_| "bash".into()))
            .unwrap_or_else(|| "write".into());

        // Intent routing sees only the family index. Select the one whose stable index line names
        // the target operation; this works for grouped/plugin tools too without hard-coding their
        // family names into the mock.
        if req.tools.len() == 1 && req.tools[0].name == "declare_intent" {
            let family = req
                .system_segments
                .iter()
                .flat_map(|segment| segment.text.lines())
                .filter_map(|line| {
                    let line = line.strip_prefix("- ")?;
                    let (family, details) = line.split_once(" (")?;
                    let members = details.split_once("; ")?.1.split_once("):")?.0;
                    let examples = members
                        .strip_prefix("e.g. ")
                        .or_else(|| members.strip_prefix("operations "))?;
                    let contains_target = family == target
                        || examples
                            .split(',')
                            .any(|operation| operation.trim() == target);
                    contains_target.then(|| family.to_string())
                })
                .next()
                .into_iter()
                .collect::<Vec<_>>();
            let chunks = vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "intent1".into(),
                    name: "declare_intent".into(),
                    input: serde_json::json!({
                        "intent": "complete the offline mock turn",
                        "capability_families": family
                    }),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ];
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }

        if req.tools.iter().any(|tool| tool.name == "finalize_plan") {
            // Tool-result text is deliberately not part of `Message::text()`. Serialize the mock
            // ledger so this offline provider observes the same structured result blocks a real
            // wire codec sends back to the model.
            let transcript = serde_json::to_string(&req.messages).unwrap_or_default();

            // The finalize call's matching tool result carries the actual ExecutionReport. Only now
            // may the model claim completion.
            if transcript.contains("Execution report (actual guarded results)") {
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

            // Once the operation was captured, freeze the host-built batch.
            if transcript.contains("captured as proposed action") {
                let chunks = vec![
                    Chunk::Block(ContentBlock::ToolUse {
                        id: "finalize1".into(),
                        name: "finalize_plan".into(),
                        input: serde_json::json!({
                            "instructions": "Report the actual guarded operation result."
                        }),
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::ToolUse),
                    },
                ];
                return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
            }

            let input = if target == "write" && std::env::var("FLUX_MOCK_TOOL").is_err() {
                serde_json::json!({
                    "path": "flux-mock.txt",
                    "content": "created by flux mock\n"
                })
            } else if target == "bash" {
                serde_json::json!({
                    "command": std::env::var("FLUX_MOCK_BASH").unwrap_or_default()
                })
            } else {
                std::env::var("FLUX_MOCK_TOOL_INPUT")
                    .ok()
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_else(|| serde_json::json!({}))
            };
            let native = req
                .tools
                .iter()
                .find(|tool| {
                    tool.name == target || tool.description.contains(&format!("`{target}`"))
                })
                .map(|tool| tool.name.clone());
            if let Some(native) = native {
                let chunks = vec![
                    Chunk::Block(ContentBlock::ToolUse {
                        id: "action1".into(),
                        name: native,
                        input,
                    }),
                    Chunk::Done {
                        stop_reason: Some(StopReason::ToolUse),
                    },
                ];
                return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
            }
        }

        // A target that cannot be surfaced ends honestly in prose instead of inventing an operation.
        if n > 0 {
            let chunks = vec![
                Chunk::Block(ContentBlock::Text {
                    text: format!("The mock target `{target}` is not available in this agent."),
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

        unreachable!("the first mock provider call is always intent detection")
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
fn apply_workspace_access_env(cli: &Cli, cfg: &flux_config::Config) {
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
fn apply_sandbox_env(cli: &Cli, cfg: &flux_config::Config) -> Result<()> {
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
fn private_net_cli_override() -> bool {
    flux_system::env_truthy("FLUX_ALLOW_PRIVATE_NET")
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
/// `web.fetch`, `browser.*`), widened to `*` when `--allow-private-net` is active.
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

/// Sync entry point: everything that must happen BEFORE the tokio runtime exists lives here —
/// signal disposition, the rustls provider, clap, and every process-env export. `setenv` racing a
/// concurrent `getenv` (any worker thread resolving DNS or reading config) is undefined behavior
/// on glibc — the reason Rust 2024 marks `set_var` unsafe — so the env mutation happens while this
/// is still the only thread, and only then does the runtime spin up worker threads.
fn main() -> Result<()> {
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
    let cfg = flux_config::load(&cwd).context("load .flux/config.toml")?;
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
async fn async_main(cli: Cli) -> Result<()> {
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
/// turn. The shell is a clap `ValueEnum` (bash/elvish/fish/powershell/zsh), so an unknown value is
/// rejected at parse time; defaults to fish.
fn run_completion(shell: Option<clap_complete::Shell>) -> Result<()> {
    use clap::CommandFactory;
    let shell = shell.unwrap_or(clap_complete::Shell::Fish);
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
    let cfg = flux_config::load(&cwd).context("load .flux/config.toml")?;
    // The knowledge datasource: build the program's declared datasources, and SHARE the backend so
    // integration plugins' contributed records (via the DatasourceHostCaps bridge) land in the same
    // index the `search`/`get`/`list`/`relation`/`batch_get`/`sources` ops read.
    let backend = build_datasources(&program.datasources, &program_dir, &system).await?;
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
        if let Err(e) = endpoint_registry.load() {
            eprintln!(
                "{}",
                style::dim(&format!("(endpoints store not loaded: {e})"))
            );
        }
        let invoker = Arc::new(flux_capabilities::HostProviderInvoker::new(
            plugin_registry.clone(),
        ));
        // D-116: bind config-bound named refs (from `flux endpoint add` + `[[endpoint.static]]`).
        merge_static_endpoints(&endpoint_registry, &cfg);
        let static_resolver = Arc::new(flux_capabilities::StaticResolver::new(
            system.clone(),
            endpoint_registry.config_bindings(),
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
        let (plugins, stale) = split_stale_plugins(flux_plugin::discover(&dir));
        warn_stale_plugins(&stale);
        for p in plugins {
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
            match flux_plugin::load_plugin_tools(&system, &p.name, &p.descriptor, make_caps).await {
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
    let app = std::sync::Arc::new(flux_app::App::try_with_events_and_permissions(
        program,
        provider,
        model,
        auto_approve,
        extra_tools,
        sub_agents,
        redactor,
        app_events,
        flux_app::HostPermissionRules {
            allow: cfg.permissions.allow.clone(),
            deny: cfg.permissions.deny.clone(),
        },
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
struct CliTuiModelResolver;

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

async fn run_tui(flags: AgentFlags) -> Result<()> {
    let auto_approve = flags.yes;
    let (agent, session_id, model_spec, _spawner) = build_agent(&flags).await?;
    let initial_rules = agent.executor.allow_rules();
    let mut options = flux_tui::TuiRunOptions::new(auto_approve, Some(model_spec));
    options.model_resolver = Some(Arc::new(CliTuiModelResolver));
    // Persist even when the TUI returns an error: an earlier "always allow" choice remains a user
    // decision and must not vanish because terminal restoration or a later turn failed.
    let executor = agent.executor.clone();
    let result = flux_tui::run_with_options(agent, session_id, options).await;
    persist_new_rules(&initial_rules, &executor.allow_rules());
    result
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
/// Parse repeatable `key=value` label args into a map (rejects a missing `=` or an empty key).
fn parse_labels(pairs: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
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
fn url_has_userinfo(url: &str) -> bool {
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
fn endpoint_ref_from_parts(
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
fn merge_static_endpoints(
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

fn run_endpoint(action: EndpointAction) -> Result<()> {
    // The persisted store. A standalone CLI invocation has no in-memory session registry, so every
    // subcommand operates on `~/.flux/endpoints.toml` (loaded fresh; a missing file is empty).
    let path = flux_capabilities::EndpointRegistry::default_path()
        .ok_or_else(|| anyhow::anyhow!("HOME is not set (no endpoints store path)"))?;
    run_endpoint_in(&path, action)
}

/// The path-parameterized body of [`run_endpoint`] (tests pass a temp store so they don't touch
/// `HOME`), mirroring [`run_plugin_in`].
fn run_endpoint_in(path: &std::path::Path, action: EndpointAction) -> Result<()> {
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
            // Propagate a malformed config like the agent paths do — swallowing it here would
            // silently drop the user's `[private_net]` plugin grants and refuse the call as
            // ungranted with no hint that the config failed to parse.
            let cwd = std::env::current_dir()?;
            let cfg = flux_config::load(&cwd).context("load .flux/config.toml")?;
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
                let dry = serde_json::json!({
                    "plugin": name,
                    "operation": resolved_op,
                    "valid": problems.is_empty(),
                    "problems": problems,
                    "warnings": warnings,
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

/// Describe how a declared auth purpose would resolve right now — a stored token (OAuth login or
/// `flux auth set`), or which env key (if any) is set — without ever printing the resolved secret
/// value. Mirrors the host's resolution order: stored token first, declared env keys second.
fn describe_auth_resolution(plugin: &str, m: &flux_plugin::AuthMethod) -> String {
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
fn describe_endpoint_resolution(ep: &flux_plugin::EndpointSpec) -> String {
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
/// (`--global` without `--install` is a clap-level `requires` error, not checked here.)
async fn run_skill(type_: Option<skill_cmd::SkillType>, install: bool, global: bool) -> Result<()> {
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

/// Split discovered plugins into loadable descriptors and STALE registrations — an ABSOLUTE
/// recorded `program` whose binary is POSITIVELY confirmed absent (a deleted checkout, a pruned
/// pack store). Stale ones are skipped before any spawn attempt and reported by
/// [`warn_stale_plugins`] as ONE aggregated line, so a pile of dead descriptors doesn't print a
/// warning per plugin on every command. Anything this can't confirm absent defers to the spawn
/// (whose real error still gets its own detailed line): relative paths (they'd resolve against
/// whatever the CURRENT cwd is), bare PATH-resolved names, and stat errors (permissions, a
/// transient mount) — and on Windows a program recorded without `.exe` counts as present when the
/// `.exe` sibling exists (CreateProcess appends it).
fn split_stale_plugins(
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
fn warn_stale_plugins(stale: &[String]) {
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

/// `flux auth status | login <provider>`.
/// Map a resolved `provider/model` spec to the `flux auth status` row it authenticates against, so
/// the status view can flag the active default provider. Returns `None` for specs that need no
/// listed credential (local `ollama*`, or `aws`, which isn't a listed row).
fn auth_row_for_spec(spec: &str) -> Option<&'static str> {
    // The offline `mock` provider needs no credential (bare `mock` resolves to `anthropic` in
    // `flux_providers::spec::provider_prefix` for provider construction, but there is no key to
    // flag here).
    if spec == "mock" {
        return None;
    }
    match flux_providers::spec::provider_prefix(spec)? {
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
            // A malformed config must not silently report the wrong "default model" as configured.
            let cfg = flux_config::load(&cwd).context("load .flux/config.toml")?;
            let default_spec = resolve_model_spec(&None, &cfg);
            let active = auth_row_for_spec(&default_spec);
            let rows = flux_credentials::auth_status();
            print!("{}", format_auth_status(&rows, &default_spec, active));
            Ok(())
        }
        AuthAction::Login { provider, password } => match provider.as_str() {
            // The built-in providers only speak their PKCE flows — reject `--password` instead
            // of silently ignoring it (it is the plugin-OAuth password grant, D-82).
            name @ ("claude" | "codex") if password => {
                bail!("--password only applies to an installed OAuth2 plugin — `{name}` uses its browser PKCE flow")
            }
            "claude" => login_claude().await,
            "codex" => login_codex().await,
            // Any other name is treated as an installed OAuth2 plugin (plugin-oauth, D-82).
            name => login_plugin(name, password).await,
        },
        AuthAction::Set {
            plugin,
            purpose,
            clear,
        } => auth_set(&plugin, purpose.as_deref(), clear).await,
    }
}

/// Store (or `--clear`) a plain bearer for an installed plugin's auth purpose (D-126): validate the
/// plugin + purpose against the live manifest, prompt hidden for the token (read one stdin line
/// when piped, so `printf '%s' "$TOK" | flux auth set …` scripts), and persist it under
/// `plugin:<name>:<purpose>` — the same store key the host's purpose resolution consults before
/// falling back to the declared env keys. The token value is never echoed.
async fn auth_set(name: &str, purpose: Option<&str>, clear: bool) -> Result<()> {
    let dir = plugins_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set — no plugin store"))?;
    let desc = flux_plugin::load_descriptor(&dir, name)
        .context("load plugin descriptor")?
        .ok_or_else(|| anyhow::anyhow!("no such plugin `{name}` — install it first"))?;
    let manifest = spawn_and_load_manifest(name, &desc).await?;
    let declared = || {
        manifest
            .auth
            .iter()
            .map(|a| a.purpose.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let method = match purpose {
        Some(p) => manifest
            .auth
            .iter()
            .find(|a| a.purpose == p)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "plugin `{name}` declares no auth purpose `{p}` (declared: {})",
                    declared()
                )
            })?,
        None => match manifest.auth.as_slice() {
            [] => bail!("plugin `{name}` declares no auth methods"),
            [only] => only,
            _ => bail!(
                "plugin `{name}` declares {} auth purposes — name one: {}",
                manifest.auth.len(),
                declared()
            ),
        },
    };
    let key = format!("plugin:{name}:{}", method.purpose);
    if clear {
        flux_credentials::delete_token(&key)?;
        println!(
            "\u{2713} cleared stored token for plugin `{name}` (purpose `{}`)",
            method.purpose
        );
        return Ok(());
    }
    let prompt = format!("{} for `{name}`: ", method.purpose);
    // The prompt blocks on user think-time — keep it off the runtime thread.
    let token = tokio::task::spawn_blocking(move || -> Result<String> {
        if std::io::stdin().is_terminal() {
            rpassword::prompt_password(&prompt).context("read token")
        } else {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .context("read token from stdin")?;
            Ok(line)
        }
    })
    .await
    .context("token prompt task")??;
    let token = token.trim();
    if token.is_empty() {
        bail!("empty token — nothing stored");
    }
    flux_credentials::save_token(
        &key,
        &flux_credentials::OAuthToken {
            access: token.to_string(),
            refresh: None,
            expires_at_ms: None,
            account_id: None,
        },
    )?;
    println!(
        "\u{2713} stored token for plugin `{name}` (purpose `{}`) in ~/.flux/credentials.toml",
        method.purpose
    );
    Ok(())
}

/// Interactive Anthropic (Claude subscription) PKCE login.
async fn login_claude() -> Result<()> {
    let pkce = flux_credentials::generate_pkce();
    let state = flux_credentials::generate_state();
    let url = flux_credentials::anthropic_authorize_url(&pkce, &state);
    println!(
        "Open this URL, approve access, then paste the code from the callback page:\n\n{url}\n"
    );
    // Off the runtime thread: the user can sit on this prompt indefinitely.
    let code = tokio::task::spawn_blocking(|| prompt_line("code: "))
        .await
        .context("code prompt task")??;
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
/// `/favicon.ico`) get a 404 and the wait continues. Bounded at 300s like its generic sibling
/// [`wait_for_oauth_callback`] — an abandoned browser flow must not hang the login forever.
/// Returns the callback as `code#state` — the shape `codex_exchange_and_store` binds against the
/// login's CSRF state.
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
    let accept = async {
        loop {
            let (mut sock, _) = listener.accept().await.context("accept OAuth callback")?;
            // The callback is a small GET; one read is enough for the request line we parse.
            let mut buf = vec![0u8; 8192];
            let n = match sock.read(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    // A failed read is this connection's problem, not the login's — say so and
                    // keep listening rather than silently 404-ing an empty request.
                    eprintln!("{}", style::dim(&format!("(callback read failed: {e})")));
                    continue;
                }
            };
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
    };
    match tokio::time::timeout(std::time::Duration::from_secs(300), accept).await {
        Ok(r) => r,
        Err(_) => bail!(
            "timed out waiting for the OAuth callback on localhost:{}",
            flux_credentials::CODEX_REDIRECT_PORT
        ),
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
        // Both prompts block on user think-time — keep them off the runtime thread.
        let (username, secret) = tokio::task::spawn_blocking(|| -> Result<(String, String)> {
            let username = prompt_line("username: ")?;
            let secret = rpassword::prompt_password("password: ").context("read password")?;
            Ok((username, secret))
        })
        .await
        .context("credential prompt task")??;
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
            let n = match sock.read(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("{}", style::dim(&format!("(callback read failed: {e})")));
                    continue;
                }
            };
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
        credential_location, endpoint_ref_from_parts, format_evidence, implicit_plugin_group,
        loop_machinery_label, merge_static_endpoints, new_render_suffix, parse_labels,
        plugin_binaries_in, plugin_status_one, render_endpoint_row, render_review_markdown,
        resolve_plugin_operation_name, run_endpoint_in, run_plugin_in, run_usage_with, should_fail,
        tool_preview, truncate, url_has_userinfo, usage_annotation, write_generated_skill,
        EndpointAction, EventStore, EventStoreCrossPluginAudit, EventStoreEgressAudit, Liveness,
        PluginAction, RedactorSecretSink, ReviewSeverity,
    };
    use flux_flow::AgentSink;
    use flux_provider::{ChunkStream, Provider, Request};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    struct CapturingModelProvider(Arc<Mutex<Vec<Request>>>);

    #[async_trait::async_trait]
    impl Provider for CapturingModelProvider {
        fn name(&self) -> &str {
            "capture"
        }

        async fn stream(&self, request: Request) -> flux_core::Result<ChunkStream> {
            self.0.lock().unwrap().push(request);
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[test]
    fn reasoning_controls_are_visible_in_agent_help() {
        use clap::CommandFactory;

        let help = super::AgentFlagsOnly::command()
            .render_long_help()
            .to_string();
        assert!(help.contains("--think"), "{help}");
        assert!(help.contains("--effort"), "{help}");
        assert!(help.contains("--loop"), "{help}");
        assert!(help.contains("adaptive"), "{help}");
        assert!(help.contains("low"), "{help}");
        assert!(help.contains("high"), "{help}");
        assert!(help.contains("--max-model-calls"), "{help}");
        assert!(help.contains("--max-iterations"), "{help}");
    }

    #[tokio::test]
    async fn lazy_provider_resolves_only_the_inherited_default_model() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let lazy = super::LazyProvider::new("codex/unresolved-parent".into());
        let initialized = lazy.cell.set((
            Box::new(CapturingModelProvider(requests.clone())),
            "resolved-parent".into(),
        ));
        assert!(initialized.is_ok());

        let _stage_stream = lazy
            .stream(Request::new("stage-model", "stage"))
            .await
            .unwrap();
        let _default_stream = lazy
            .stream(Request::new("unresolved-parent", "default"))
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests[0].model, "stage-model");
        assert_eq!(requests[1].model, "resolved-parent");
    }

    #[test]
    fn adaptive_config_rejects_zero_stage_limits_before_provider_setup() {
        let flags = super::AgentFlags::from_model_yes(Some("mock"), true);
        let mut config = flux_config::AgentConfig::default();
        config.adaptive.explore.max_calls = Some(0);
        let error = super::adaptive_loop_policy(&flags, &config)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("[agent.adaptive.explore] max_calls must be greater than zero"),
            "{error}"
        );
    }

    #[test]
    fn outer_loop_iterations_follow_cli_then_config_then_default_precedence() {
        use clap::Parser;

        let default_flags = super::AgentFlags::from_model_yes(Some("mock"), true);
        let mut config = flux_config::AgentConfig {
            max_iterations: Some(37),
            ..Default::default()
        };
        assert_eq!(
            super::agent_max_iterations(&default_flags, &config).unwrap(),
            37
        );

        let cli_flags = super::AgentFlagsOnly::parse_from(["flux", "--max-iterations", "41"]).agent;
        assert_eq!(
            super::agent_max_iterations(&cli_flags, &config).unwrap(),
            41
        );

        config.max_iterations = None;
        assert_eq!(
            super::agent_max_iterations(&default_flags, &config).unwrap(),
            flux_flow::DEFAULT_AGENT_LOOP_ITERATIONS
        );
        config.max_iterations = Some(0);
        assert!(super::agent_max_iterations(&default_flags, &config)
            .unwrap_err()
            .to_string()
            .contains("[agent] max_iterations must be greater than zero"));
    }

    #[test]
    fn outer_loop_iterations_reject_cli_and_config_values_above_the_practical_cap() {
        use clap::Parser;

        let default_flags = super::AgentFlags::from_model_yes(Some("mock"), true);
        let at_max = flux_config::AgentConfig {
            max_iterations: Some(flux_flow::MAX_AGENT_LOOP_ITERATIONS),
            ..Default::default()
        };
        assert_eq!(
            super::agent_max_iterations(&default_flags, &at_max).unwrap(),
            flux_flow::MAX_AGENT_LOOP_ITERATIONS
        );

        let above_max = flux_flow::MAX_AGENT_LOOP_ITERATIONS + 1;
        let config = flux_config::AgentConfig {
            max_iterations: Some(above_max),
            ..Default::default()
        };
        let config_error = super::agent_max_iterations(&default_flags, &config)
            .unwrap_err()
            .to_string();
        assert!(
            config_error.contains("[agent] max_iterations"),
            "{config_error}"
        );
        assert!(
            config_error.contains(&format!(
                "maximum of {}",
                flux_flow::MAX_AGENT_LOOP_ITERATIONS
            )),
            "{config_error}"
        );

        let cli_flags =
            super::AgentFlagsOnly::parse_from(["flux", "--max-iterations", &above_max.to_string()])
                .agent;
        let cli_error = super::agent_max_iterations(&cli_flags, &Default::default())
            .unwrap_err()
            .to_string();
        assert!(cli_error.contains("--max-iterations"), "{cli_error}");
        assert!(
            cli_error.contains(&format!(
                "maximum of {}",
                flux_flow::MAX_AGENT_LOOP_ITERATIONS
            )),
            "{cli_error}"
        );
    }

    #[test]
    fn operation_timing_names_approval_and_execution_separately() {
        let rendered = super::format_operation_timing(flux_core::OperationTiming {
            total_us: 30_005_000,
            approval_wait_us: Some(30_000_000),
            execution_us: Some(5_000),
        });
        assert_eq!(rendered, "exec 5ms + approval 30.0s");
    }

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

    #[test]
    fn tui_model_resolver_routes_mock_to_the_offline_provider() {
        let resolved = flux_tui::ModelResolver::resolve(&super::CliTuiModelResolver, "mock")
            .expect("mock resolution is credential-free");
        assert_eq!(resolved.provider.name(), "mock");
        assert_eq!(resolved.wire_model, "mock");
        assert_eq!(resolved.model_spec, "mock");
    }

    /// L-77: `flux render` is an explicit subcommand — positional `.flux` file, `--view
    /// source|tree` (default `source`), `-o <out.svg>`.
    #[test]
    fn render_subcommand_parses() {
        use super::{Cli, Commands, RenderView};
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "flux",
            "render",
            "greet.flux",
            "--view",
            "tree",
            "-o",
            "out.svg",
        ])
        .expect("`render` parses");
        match cli.command {
            Some(Commands::Render { file, view, out }) => {
                assert_eq!(file, "greet.flux");
                assert_eq!(view, RenderView::Tree);
                assert_eq!(out.as_deref(), Some("out.svg"));
            }
            other => panic!("expected Render, got {other:?}"),
        }
        // The view defaults to `source` and `-o` is optional (SVG then prints to stdout).
        let cli2 =
            Cli::try_parse_from(["flux", "render", "greet.flux"]).expect("bare render parses");
        match cli2.command {
            Some(Commands::Render { view, out, .. }) => {
                assert_eq!(view, RenderView::Source);
                assert_eq!(out, None);
            }
            other => panic!("expected Render, got {other:?}"),
        }
    }

    #[test]
    fn saved_flow_subcommands_and_input_flags_parse() {
        use super::{Cli, Commands, FlowAction};
        use clap::Parser;

        for list_word in ["list", "ls"] {
            let cli = Cli::try_parse_from(["flux", "flow", list_word]).unwrap();
            assert!(matches!(
                cli.command,
                Some(Commands::Flow {
                    action: FlowAction::List
                })
            ));
        }

        let cli = Cli::try_parse_from([
            "flux",
            "flow",
            "run",
            "deploy",
            "--inputs",
            r#"{"env":"dev"}"#,
            "--arg",
            "replicas=2",
            "--arg",
            "replicas=3",
            "--map-inputs",
            "deploy three replicas",
            "-m",
            "aws/sonnet",
            "--yes",
            "--resumable",
            "--resume",
            "last",
            "--resume-value",
            "42",
        ])
        .unwrap();
        match cli.command {
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
                assert_eq!(target, "deploy");
                assert_eq!(inputs.as_deref(), Some(r#"{"env":"dev"}"#));
                assert_eq!(args, ["replicas=2", "replicas=3"]);
                assert_eq!(map_inputs.as_deref(), Some("deploy three replicas"));
                assert_eq!(model.as_deref(), Some("aws/sonnet"));
                assert!(yes && resumable);
                assert_eq!(resume.as_deref(), Some("last"));
                assert_eq!(resume_value.as_deref(), Some("42"));
            }
            other => panic!("expected flow run, got {other:?}"),
        }
    }

    fn cli_input_ast(params: Vec<(&str, flux_flow::ast::TypeRef)>) -> flux_flow::ast::DraftAst {
        flux_flow::ast::DraftAst {
            name: Some("input-test".into()),
            params: params
                .into_iter()
                .map(|(name, ty)| flux_flow::ast::Param {
                    name: name.into(),
                    ty,
                })
                .collect(),
            body: vec![flux_flow::ast::Node::Return {
                value: Box::new(flux_flow::ast::Node::Lit {
                    value: serde_json::json!("body"),
                }),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn cli_flow_inputs_merge_and_coerce_by_declared_type() {
        use flux_flow::ast::{Node, TypeRef};
        let mut ast = cli_input_ast(vec![
            ("env", TypeRef::String),
            ("replicas", TypeRef::Number),
            ("enabled", TypeRef::Bool),
            ("tags", TypeRef::List(Box::new(TypeRef::String))),
            ("payload", TypeRef::Any),
            ("named", TypeRef::Named("DeploySpec".into())),
        ]);
        super::prepare_cli_flow_inputs(
            &mut ast,
            Some(
                r#"{"env":"json","replicas":1,"enabled":false,"tags":["old"],"payload":null,"named":{"old":true}}"#,
            ),
            &[
                "env=arg".into(),
                "replicas=not-a-number".into(),
                "replicas=3".into(),
                "enabled=true".into(),
                "tags=[\"blue\",\"green\"]".into(),
                "payload={\"mode\":\"safe\"}".into(),
                "named=plain-text".into(),
            ],
            Some("this mapper must be skipped"),
        )
        .unwrap();

        let values: std::collections::BTreeMap<String, serde_json::Value> = ast.body[..6]
            .iter()
            .map(|node| match node {
                Node::Bind { name, value, .. } => match value.as_ref() {
                    Node::Lit { value } => (name.0.clone(), value.clone()),
                    other => panic!("expected literal input bind, got {other:?}"),
                },
                other => panic!("expected input bind, got {other:?}"),
            })
            .collect();
        assert_eq!(values["env"], serde_json::json!("arg"));
        assert_eq!(values["replicas"], serde_json::json!(3));
        assert_eq!(values["enabled"], serde_json::json!(true));
        assert_eq!(values["tags"], serde_json::json!(["blue", "green"]));
        assert_eq!(values["payload"], serde_json::json!({"mode": "safe"}));
        assert_eq!(values["named"], serde_json::json!("plain-text"));
        assert!(
            !ast.body.iter().any(|node| matches!(
                node,
                Node::Bind { value, .. }
                    if matches!(value.as_ref(), Node::Call { op, .. } if op == "ai.extract")
            )),
            "a fully deterministic contract must skip --map-inputs"
        );
    }

    #[test]
    fn cli_flow_inputs_reject_bad_json_unknown_missing_and_type_mismatches() {
        use flux_flow::ast::TypeRef;
        let base = || cli_input_ast(vec![("env", TypeRef::String), ("n", TypeRef::Number)]);

        let mut ast = base();
        assert!(
            super::prepare_cli_flow_inputs(&mut ast, Some("{"), &[], None)
                .unwrap_err()
                .to_string()
                .contains("valid JSON object")
        );
        let mut ast = base();
        assert!(
            super::prepare_cli_flow_inputs(&mut ast, Some("[]"), &[], None)
                .unwrap_err()
                .to_string()
                .contains("must be a JSON object")
        );
        let mut ast = base();
        assert!(super::prepare_cli_flow_inputs(
            &mut ast,
            Some(r#"{"env":"dev","n":1,"extra":true}"#),
            &[],
            None,
        )
        .unwrap_err()
        .to_string()
        .contains("unknown flow input parameter(s): extra"));
        let mut ast = base();
        assert!(
            super::prepare_cli_flow_inputs(&mut ast, Some(r#"{"env":"dev"}"#), &[], None,)
                .unwrap_err()
                .to_string()
                .contains("missing required flow parameter(s): n (Number)")
        );
        let mut ast = base();
        assert!(super::prepare_cli_flow_inputs(
            &mut ast,
            Some(r#"{"env":"dev","n":"3"}"#),
            &[],
            None,
        )
        .unwrap_err()
        .to_string()
        .contains("input `n` expects Number, got String"));
        let mut ast = base();
        assert!(super::prepare_cli_flow_inputs(
            &mut ast,
            Some(r#"{"env":"dev","n":3}"#),
            &["broken".into()],
            None,
        )
        .unwrap_err()
        .to_string()
        .contains("--arg expects KEY=VALUE"));
    }

    #[test]
    fn mapper_ast_uses_missing_schema_strict_fields_and_collision_free_symbols() {
        use flux_flow::ast::{Node, TypeRef};
        let mut ast = cli_input_ast(vec![
            ("known", TypeRef::String),
            ("env", TypeRef::String),
            ("replicas", TypeRef::Number),
        ]);
        ast.body.splice(
            0..0,
            ["__flux_map_raw", "__flux_map_json", "__flux_map_args"]
                .into_iter()
                .map(|name| Node::Bind {
                    name: name.into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!("occupied"),
                    }),
                    ty: None,
                    effect: None,
                }),
        );
        super::prepare_cli_flow_inputs(
            &mut ast,
            Some(r#"{"known":"fixed"}"#),
            &[],
            Some("three replicas in dev"),
        )
        .unwrap();

        let Node::Bind {
            name: raw,
            value: extract,
            ..
        } = &ast.body[0]
        else {
            panic!("mapper must begin with ai.extract bind")
        };
        assert_eq!(raw.0, "__flux_map_raw_1");
        let Node::Call { op, args } = extract.as_ref() else {
            panic!("mapper first bind must be a call")
        };
        assert_eq!(op, "ai.extract");
        let Node::Obj { fields } = &args[0] else {
            panic!("ai.extract must receive named args")
        };
        let Node::Lit { value } = fields["schema"].as_ref() else {
            panic!("schema must be literal")
        };
        let schema: serde_json::Value = serde_json::from_str(value.as_str().unwrap()).unwrap();
        assert_eq!(schema["required"], serde_json::json!(["env", "replicas"]));
        assert!(schema["properties"].get("known").is_none());
        assert_eq!(schema["properties"]["env"]["type"], "string");
        assert_eq!(schema["properties"]["replicas"]["type"], "number");

        assert!(matches!(&ast.body[1], Node::Bind { name, .. } if name.0 == "__flux_map_json_1"));
        assert!(matches!(&ast.body[3], Node::Bind { name, .. } if name.0 == "__flux_map_args_1"));
        for (node, expected) in ast.body[4..6].iter().zip(["env", "replicas"]) {
            let Node::Bind { name, value, .. } = node else {
                panic!("mapped field must bind")
            };
            assert_eq!(name.0, expected);
            assert!(matches!(
                value.as_ref(),
                Node::Jq {
                    optional: false,
                    ..
                }
            ));
        }
        assert!(matches!(&ast.body[6], Node::Bind { name, value, .. }
            if name.0 == "known" && matches!(value.as_ref(), Node::Lit { .. })));
    }

    /// L-77: the render handler reads the `.flux` file from the plain filesystem (absolute and
    /// out-of-workspace paths work, like `flow run`), strips a UTF-8 BOM before parsing, writes
    /// the SVG through the workspace-confined `System` (`-o`), tree view propagates a hard parse
    /// error (non-zero exit), and source view is total — malformed input still renders. The
    /// inputs live OUTSIDE the workspace root, so re-jailing the read through
    /// `System::read_file` fails this test — the un-jailed read is a pinned decision, not an
    /// oversight.
    #[tokio::test]
    async fn run_render_writes_svg_and_propagates_tree_parse_errors() {
        use super::{run_render_in, RenderView};
        let base = std::env::temp_dir().join(format!("flux-render-cli-{}", std::process::id()));
        // `ws` is the System workspace root (`-o` writes land here); the inputs live in a SIBLING
        // dir the workspace envelope does not cover.
        let ws = base.join("ws");
        let srcdir = base.join("elsewhere");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&srcdir).unwrap();
        let greet = srcdir.join("greet.flux");
        std::fs::write(&greet, "flow greet(name: String)\n  do notify \"hi\"\n").unwrap();
        let broken = srcdir.join("broken.flux");
        std::fs::write(&broken, "flow ((((\n").unwrap();
        // A BOM'd but otherwise-valid file (PowerShell Out-File / Notepad) must render in tree
        // view — the BOM is stripped before the parser sees it.
        let bommed = srcdir.join("bommed.flux");
        std::fs::write(&bommed, "\u{feff}flow greet(name: String)\n  return 1\n").unwrap();
        let system = super::System::new(super::Workspace::new(&ws).unwrap());

        // The input is an ABSOLUTE path outside the workspace root (the read is NOT jailed —
        // parity with `flow run`); `-o` writes into the workspace.
        run_render_in(
            &system,
            greet.to_str().unwrap(),
            RenderView::Tree,
            Some("img/out.svg"),
        )
        .await
        .expect("tree render of a valid flow succeeds");
        let svg = std::fs::read_to_string(ws.join("img/out.svg")).unwrap();
        assert!(svg.starts_with("<svg"), "got: {svg}");

        run_render_in(&system, bommed.to_str().unwrap(), RenderView::Tree, None)
            .await
            .expect("a UTF-8 BOM is stripped, not fed to the parser");

        // A hard parse error in `tree` view surfaces the parser's message as an Err.
        let err = run_render_in(&system, broken.to_str().unwrap(), RenderView::Tree, None)
            .await
            .expect_err("tree view needs parseable source");
        assert!(err.to_string().contains("parse"), "got: {err:#}");

        // `source` view is total: the same malformed file still renders.
        run_render_in(
            &system,
            broken.to_str().unwrap(),
            RenderView::Source,
            Some("broken.svg"),
        )
        .await
        .expect("source view renders malformed input");
        assert!(std::fs::read_to_string(ws.join("broken.svg"))
            .unwrap()
            .starts_with("<svg"));
        std::fs::remove_dir_all(&base).ok();
    }

    /// A registered plugin whose ABSOLUTE recorded binary is confirmed gone (a deleted checkout,
    /// a pruned pack store) is a STALE registration: it is skipped up front and reported as one
    /// aggregated warning line, not spawn-failed with a dim line per plugin on every command.
    /// Everything else defers to the spawn: absolute paths that exist, bare PATH-resolved names,
    /// and RELATIVE paths (which would resolve against whatever the current cwd happens to be —
    /// a plugin registered with `install --dir` from its checkout must not be called "missing"
    /// just because flux runs elsewhere).
    #[test]
    fn split_stale_plugins_partitions_missing_binaries() {
        let dir = std::env::temp_dir().join(format!("flux-stale-plugins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("flux-plugin-live");
        std::fs::write(&live, b"#!/bin/sh\n").unwrap();
        let plugin = |name: &str, program: String| flux_plugin::DiscoveredPlugin {
            name: name.to_string(),
            descriptor: flux_plugin::PluginDescriptor {
                program,
                ..Default::default()
            },
        };
        let discovered = vec![
            plugin("live", live.to_string_lossy().into_owned()),
            plugin(
                "gone",
                dir.join("flux-plugin-gone").to_string_lossy().into_owned(),
            ),
            plugin("bare", "some-command-resolved-on-path".to_string()),
            plugin(
                "relative",
                "plugins/target/release/flux-plugin-rel".to_string(),
            ),
        ];
        let (loadable, stale) = super::split_stale_plugins(discovered);
        let names: Vec<&str> = loadable.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["live", "bare", "relative"],
            "existing, PATH-resolved, and cwd-relative programs all stay loadable"
        );
        assert_eq!(
            stale,
            ["gone"],
            "only an absolute program confirmed absent is stale"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-115: the `endpoint` group manifest and `endpoint_tools()` cannot drift — every
    /// registered endpoint op must be listed in the group. (Membership was never actually
    /// broken: `effective_group` falls back to each spec's own group tag — but the manifest is
    /// what config reassignment edits, so the explicit list must stay complete.)
    #[test]
    fn endpoint_group_manifest_matches_endpoint_tools() {
        use flux_capabilities::{
            EndpointBroker, EndpointRegistry, HostProviderInvoker, PluginRegistry,
        };
        use std::sync::Arc;
        let broker = Arc::new(EndpointBroker::new(
            Arc::new(HostProviderInvoker::new(Arc::new(PluginRegistry::new()))),
            Arc::new(PluginRegistry::new()),
            Arc::new(EndpointRegistry::new()),
        ));
        let tools = flux_capabilities::endpoint_tools(broker, Arc::new(EndpointRegistry::new()));
        let mut op_names: Vec<String> = tools.iter().map(|t| t.spec().name).collect();
        op_names.sort();
        let group = flux_tools::groups::builtin_groups()
            .into_iter()
            .find(|g| g.name == "endpoint")
            .expect("endpoint group exists");
        let mut listed = group.tools.clone();
        listed.sort();
        assert_eq!(
            listed, op_names,
            "the endpoint group manifest must gate every registered endpoint op"
        );
        // Registry-side gating agrees: every endpoint op self-declares the group.
        for t in &tools {
            assert_eq!(
                t.spec().group.as_deref(),
                Some("endpoint"),
                "{}",
                t.spec().name
            );
        }
    }

    /// D-115: a non-empty endpoints store injects the ambient `endpoint` signal — computed once
    /// from the startup-loaded registry, never a per-turn re-read of `endpoints.toml` — which
    /// surfaces the endpoint group with NO kubernetes signal. An empty/missing store injects
    /// nothing, and without a kubeconfig the group stays gated.
    #[test]
    fn endpoint_store_signal_surfaces_group_without_kubeconfig() {
        use flux_capabilities::EndpointRegistry;
        let dir = std::env::temp_dir().join(format!("flux-ep-signal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endpoints.toml");

        // Empty/missing store → no ambient signal.
        let empty = EndpointRegistry::with_path(path.clone());
        empty.load().unwrap();
        assert!(
            super::session_ambient_signals(&empty).is_empty(),
            "an empty store injects nothing"
        );

        // Persist one record, reload fresh (the CLI's startup shape), and the signal appears.
        let writer = EndpointRegistry::with_path(path.clone());
        writer.put(flux_secret::endpoint::EndpointRecord {
            endpoint: flux_secret::endpoint::EndpointRef::discovered(
                "orders-pg",
                "postgres://db.internal:5432",
                "postgres",
            ),
            owner: "config".into(),
            ttl_secs: None,
            discovered_at_secs: None,
            health: None,
        });
        writer.save().unwrap();
        let loaded = EndpointRegistry::with_path(path);
        loaded.load().unwrap();
        let signals = super::session_ambient_signals(&loaded);
        assert_eq!(signals, vec!["endpoint".to_string()]);

        // With ONLY that ambient signal (no kubernetes), the built-in endpoint group surfaces;
        // with no signals at all it stays gated. `Observation::signal` is the SAME constructor
        // the engine's ambient injection uses, so this asserts the production shape, not a copy.
        let obs: Vec<flux_evidence::Observation> = signals
            .iter()
            .map(|s| flux_evidence::Observation::signal(s))
            .collect();
        let groups = flux_tools::groups::builtin_groups();
        let active = flux_evidence::resolve_active_groups(&groups, &obs);
        assert!(active.contains("endpoint"), "surfaced by the store signal");
        let none = flux_evidence::resolve_active_groups(&groups, &[]);
        assert!(!none.contains("endpoint"), "gated with no signals");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-116: `flux endpoint add` persists a weak, credential-free config-bound ref to the store, and
    /// `list`/`show` render it. The persisted file carries the credential *location*, never a value.
    #[test]
    fn endpoint_add_persists_weak_ref_and_lists() {
        use flux_capabilities::EndpointRegistry;
        let dir = std::env::temp_dir().join(format!("flux-ep-add-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endpoints.toml");

        run_endpoint_in(
            &path,
            EndpointAction::Add {
                id: "pg-prod".into(),
                url: "postgres://db.example:5432/app".into(),
                product: Some("postgres".into()),
                protocol: Some("postgres".into()),
                credential_ref: Some("env/PGPASSWORD".into()),
                labels: vec!["region=eu".into()],
            },
        )
        .unwrap();

        // The record round-trips as a config-bound (source=Config), owner=config weak ref.
        let reg = EndpointRegistry::with_path(path.clone());
        reg.load().unwrap();
        let rec = reg.resolve("pg-prod").expect("added ref persisted");
        assert_eq!(rec.endpoint.url, "postgres://db.example:5432/app");
        assert_eq!(rec.endpoint.product, "postgres");
        assert_eq!(
            rec.endpoint.source,
            flux_secret::endpoint::SourceKind::Config
        );
        assert_eq!(rec.owner, "config");
        assert_eq!(
            rec.endpoint.credential_ref.as_ref().map(|r| r.to_string()),
            Some("env/PGPASSWORD".to_string())
        );
        assert_eq!(
            rec.endpoint.labels.get("region").map(String::as_str),
            Some("eu")
        );

        // Persisted on disk as a *location* only (the `Ref` serializes as scheme+slot, never a
        // value) — the credential slot name is present, the scheme is `env`.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("PGPASSWORD"),
            "credential slot (location) persisted"
        );
        assert!(
            on_disk.contains("env"),
            "credential scheme persisted as a location"
        );
        // The list renderer produces a row for it (reuses the same helper `flux endpoint list` uses).
        let row = render_endpoint_row(&rec);
        assert!(row.contains("pg-prod") && row.contains("postgres://db.example:5432/app"));
        // list/show/resolve all succeed against the persisted store.
        run_endpoint_in(&path, EndpointAction::List).unwrap();
        run_endpoint_in(
            &path,
            EndpointAction::Show {
                id: "pg-prod".into(),
            },
        )
        .unwrap();
        run_endpoint_in(
            &path,
            EndpointAction::Resolve {
                id: "pg-prod".into(),
            },
        )
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-116: `flux endpoint add` rejects a credential-bearing URL, an `@endpoint/` id, and an
    /// unparseable credential ref — and leaves the store untouched on rejection.
    #[test]
    fn endpoint_add_rejects_credential_bearing_url_and_bad_inputs() {
        use flux_capabilities::EndpointRegistry;
        let dir = std::env::temp_dir().join(format!("flux-ep-add-reject-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endpoints.toml");

        // Inline `user:pass@` is rejected with a pointer to `--credential-ref`.
        let err = run_endpoint_in(
            &path,
            EndpointAction::Add {
                id: "pg".into(),
                url: "postgres://user:secret@db.example:5432/app".into(),
                product: None,
                protocol: None,
                credential_ref: None,
                labels: vec![],
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("must not embed credentials"), "got: {msg}");
        assert!(msg.contains("--credential-ref"), "points at the fix: {msg}");
        // Nothing was written — the store file does not exist yet.
        assert!(!path.exists(), "a rejected add persists nothing");

        // An `@endpoint/` id (reserved for discovered) is rejected.
        assert!(run_endpoint_in(
            &path,
            EndpointAction::Add {
                id: "@endpoint/pg".into(),
                url: "postgres://db.example:5432/app".into(),
                product: None,
                protocol: None,
                credential_ref: None,
                labels: vec![],
            },
        )
        .is_err());

        // An unparseable credential ref is rejected.
        assert!(run_endpoint_in(
            &path,
            EndpointAction::Add {
                id: "pg".into(),
                url: "postgres://db.example:5432/app".into(),
                product: None,
                protocol: None,
                credential_ref: Some("not-a-ref".into()),
                labels: vec![],
            },
        )
        .is_err());

        // The store never came into existence across all three rejections.
        let reg = EndpointRegistry::with_path(path.clone());
        reg.load().unwrap();
        assert!(reg.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-116: the shared validator's low-level invariants (also exercised by `[[endpoint.static]]`).
    #[test]
    fn endpoint_ref_from_parts_validates() {
        // A valid, unauthenticated named ref.
        let r = endpoint_ref_from_parts(
            "m",
            "http://prom:9090",
            None,
            None,
            None,
            parse_labels(&[]).unwrap(),
        )
        .unwrap();
        assert_eq!(r.id, "m");
        assert_eq!(r.source, flux_secret::endpoint::SourceKind::Config);
        assert!(r.credential_ref.is_none());

        // Userinfo detection: authority `@` is a credential, a path `@` is not.
        assert!(url_has_userinfo("postgres://u:p@host:5432/db"));
        assert!(!url_has_userinfo("postgres://host:5432/db"));
        assert!(!url_has_userinfo("https://host/path@thing"));

        // Empty id / empty url are rejected.
        assert!(
            endpoint_ref_from_parts("", "http://x", None, None, None, Default::default()).is_err()
        );
        assert!(endpoint_ref_from_parts("m", "  ", None, None, None, Default::default()).is_err());
        // A malformed label is rejected at parse time.
        assert!(parse_labels(&["novalue".to_string()]).is_err());
    }

    /// D-116: `[[endpoint.static]]` bindings merge into the registry as config-bound records that
    /// then populate the StaticResolver binding table (via `config_bindings`); an invalid entry is
    /// skipped, not fatal.
    #[test]
    fn static_endpoint_config_merges_into_registry_bindings() {
        use flux_capabilities::EndpointRegistry;
        let cfg = flux_config::Config {
            endpoint: flux_config::EndpointConfig {
                static_endpoints: vec![
                    flux_config::StaticEndpoint {
                        id: "pg-prod".into(),
                        url: "postgres://db.example:5432/app".into(),
                        product: "postgres".into(),
                        credential_ref: Some("env/PGPASSWORD".into()),
                        ..Default::default()
                    },
                    // Invalid (credential-bearing URL) — must be skipped, not abort the merge.
                    flux_config::StaticEndpoint {
                        id: "bad".into(),
                        url: "postgres://u:p@host/db".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let reg = EndpointRegistry::new();
        merge_static_endpoints(&reg, &cfg);
        let bindings = reg.config_bindings();
        assert!(
            bindings.contains_key("pg-prod"),
            "valid static binding wired"
        );
        assert!(!bindings.contains_key("bad"), "invalid entry skipped");
        assert_eq!(bindings["pg-prod"].url, "postgres://db.example:5432/app");
    }

    /// D-116 e2e (gated on `TEST_POSTGRES_URL`, like the pg backend tests): an operator-added
    /// Postgres endpoint resolves end-to-end through the broker's resolver chain — the named ref
    /// (`sql.endpoint`, the sql plugin's default dial-by-reference target) binds to its bare URL and
    /// the credential ref materializes host-side. These are exactly the two things the sql plugin
    /// asks the host for when it dials by reference and runs host-terminated SCRAM (D-31); the SCRAM
    /// leg itself is that story's tested contract, so this proof stops at the resolution seam D-116
    /// closes (before D-116 the StaticResolver had an empty map and `sql.endpoint` never resolved).
    #[tokio::test]
    async fn endpoint_add_postgres_resolves_through_broker_e2e() {
        use flux_capabilities::{
            EndpointBroker, EndpointRegistry, HostProviderInvoker, PluginRegistry, StaticResolver,
        };
        use flux_plugin::ReferenceResolver; // brings `resolve_endpoint`/`resolve_credential` in scope
        use std::sync::Arc;
        let Ok(pg_url) = std::env::var("TEST_POSTGRES_URL") else {
            eprintln!(
                "skipping endpoint_add_postgres_resolves_through_broker_e2e: TEST_POSTGRES_URL unset"
            );
            return;
        };
        // The stored URL must be credential-free — strip any userinfo the test DSN carries.
        let bare = {
            match pg_url.split_once("://") {
                Some((scheme, rest)) => {
                    let slash = rest.find('/').unwrap_or(rest.len());
                    match rest[..slash].find('@') {
                        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
                        None => pg_url.clone(),
                    }
                }
                None => pg_url.clone(),
            }
        };

        let dir = std::env::temp_dir().join(format!("flux-ep-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endpoints.toml");
        // The credential is a *location*: an env var the host materializes, never part of the URL.
        let cred_key = format!("FLUX_D116_PGPASS_{}", std::process::id());
        std::env::set_var(&cred_key, "host-side-only");

        // Operator wires the service in one command → a weak, credential-free ref is persisted.
        run_endpoint_in(
            &path,
            EndpointAction::Add {
                id: "sql.endpoint".into(),
                url: bare.clone(),
                product: Some("postgres".into()),
                protocol: Some("postgres".into()),
                credential_ref: Some(format!("env/{cred_key}")),
                labels: vec![],
            },
        )
        .unwrap();

        // A fresh session loads the store and builds the resolver from its config bindings.
        let registry = Arc::new(EndpointRegistry::with_path(path.clone()));
        registry.load().unwrap();
        assert!(
            registry.resolve("sql.endpoint").is_some(),
            "endpoint.list / `flux endpoint list` would show the added ref"
        );
        let system = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let resolver = Arc::new(StaticResolver::new(system, registry.config_bindings()));
        let broker = EndpointBroker::new(
            Arc::new(HostProviderInvoker::new(Arc::new(PluginRegistry::new()))),
            Arc::new(PluginRegistry::new()),
            registry,
        )
        .with_static_resolver(resolver);

        // Dial-by-reference: the named ref binds to its bare URL through the broker chain.
        let resolved = broker.resolve_endpoint("sql.endpoint").await.unwrap();
        assert_eq!(resolved.url, bare);
        // Host-terminated auth: the credential ref materializes host-side (the value never enters a
        // plugin — this is the same host-side read `host.conn_authenticate` performs for SCRAM).
        let material = broker
            .resolve_credential(&flux_secret::Ref::env(&cred_key))
            .await
            .unwrap();
        assert_eq!(material.value, "host-side-only");
        std::fs::remove_dir_all(&dir).ok();
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
        assert_eq!(super::auth_row_for_spec("fable"), Some("anthropic"));
        assert_eq!(super::auth_row_for_spec("claude"), Some("claude"));
        assert_eq!(super::auth_row_for_spec("claude/sonnet"), Some("claude"));
        assert_eq!(
            super::auth_row_for_spec("openrouter-anthropic/x"),
            Some("openrouter")
        );
        assert_eq!(super::auth_row_for_spec("ollama/llama"), None);
    }

    /// C-49: spec parsing — bare aliases, bare-provider defaults, and the client-side empty-model
    /// rejection (a spec like `claude/` previously shipped an empty model id to the API and came
    /// back as a confusing HTTP 400). D-152 moved the parser into `flux-providers`; this asserts the
    /// CLI's view of the shared function still surfaces the exact provider-error strings.
    #[test]
    fn parse_model_spec_covers_aliases_defaults_and_rejects_empty_models() {
        let parse = flux_providers::spec::parse_model_spec;
        // Bare anthropic short-names carry the alias through as the model.
        assert_eq!(
            parse("sonnet").unwrap(),
            ("anthropic".into(), "sonnet".into())
        );
        assert_eq!(
            parse("fable").unwrap(),
            ("anthropic".into(), "fable".into())
        );
        // Bare `claude` defaults to the subscription's sonnet, like bare `codex`/`aws` defaults.
        assert_eq!(parse("claude").unwrap(), ("claude".into(), "sonnet".into()));
        assert_eq!(parse("codex").unwrap(), ("codex".into(), "".into()));
        assert_eq!(parse("aws").unwrap(), ("aws".into(), "".into()));
        // Fully-qualified specs pass through.
        assert_eq!(
            parse("claude/claude-fable-5").unwrap(),
            ("claude".into(), "claude-fable-5".into())
        );
        // Empty model after the slash: rejected client-side with an actionable hint…
        let err = parse("claude/").unwrap_err().to_string();
        assert!(err.contains("no model"), "unexpected: {err}");
        assert!(err.contains("claude/sonnet"), "unexpected: {err}");
        let err = parse("anthropic/").unwrap_err().to_string();
        assert!(err.contains("no model"), "unexpected: {err}");
        // …except for the two providers whose resolvers document an "" → default mapping.
        assert_eq!(parse("codex/").unwrap(), ("codex".into(), "".into()));
        // Unknown bare words still point at the spec shape and the alias set.
        let err = parse("gpt-5.5").unwrap_err().to_string();
        assert!(err.contains("claude/sonnet"), "unexpected: {err}");
        assert!(!err.contains("claude/gpt-5.5"), "unexpected: {err}");
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
        // misleads both the reader and a repair-reading model stage.
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
        let enabled = vec!["l02-cli-layering".to_string()];
        let skills = super::load_skills(&root, &cfg, &[], &enabled).unwrap();
        let s = skills
            .iter()
            .find(|s| s.name == "l02-cli-layering")
            .unwrap();
        assert_eq!(s.body, "from config");

        // ...and a CLI --skill-dir beats the config layer.
        let skills = super::load_skills(&root, &cfg, &[root.join("cli-skills")], &enabled).unwrap();
        let s = skills
            .iter()
            .find(|s| s.name == "l02-cli-layering")
            .unwrap();
        assert_eq!(s.body, "from cli");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skills_are_disabled_until_named_explicitly() {
        let root =
            std::env::temp_dir().join(format!("flux-cli-manual-skills-{}", std::process::id()));
        let dir = root.join(".flux/skills");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("automatic.md"),
            "---\nname: automatic\ntriggers: [hello]\n---\nlarge body",
        )
        .unwrap();
        let cfg = flux_config::Config::default();
        assert!(
            super::load_skills(&root, &cfg, &[], &[])
                .unwrap()
                .is_empty(),
            "discovery and prompt triggers must not enable a skill"
        );
        let enabled = super::load_skills(&root, &cfg, &[], &["automatic".to_string()]).unwrap();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "automatic");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unknown_explicit_skill_fails_before_agent_construction() {
        let root =
            std::env::temp_dir().join(format!("flux-cli-unknown-skill-{}", std::process::id()));
        let error = super::load_skills(
            &root,
            &flux_config::Config::default(),
            &[],
            &["missing".to_string()],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("unknown skill `missing` (discovered:"),
            "{error}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn bounded_collector_polls_plugin_loads_concurrently() {
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let maximum = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let futures = (0..4)
            .map(|value| {
                let active = active.clone();
                let maximum = maximum.clone();
                async move {
                    let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    maximum.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    // Deliberately block before this future can yield. `buffer_unordered` alone does
                    // not provide concurrency for this shape; each plugin loader performs a small
                    // synchronous verify/spawn prefix with the same property.
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    value
                }
            })
            .collect();
        let mut values = super::collect_bounded(futures, 4).await.unwrap();
        values.sort_unstable();
        assert_eq!(values, [0, 1, 2, 3]);
        assert!(
            maximum.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "plugin handshakes were polled sequentially"
        );
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
        // Canonicalize so the program dir matches the (canonicalized) workspace root on platforms where
        // the temp dir is a symlink (e.g. macOS `/tmp` → `/private/tmp`).
        let dir = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(dir.join("note.md"), "# Title\nhello from a markdown note").unwrap();
        let system = System::new(Workspace::new(&dir).unwrap());

        let ok = vec![DatasourceDecl {
            name: "docs".into(),
            kind: "markdown".into(),
            path: Some(".".into()),
            settings: serde_json::Value::Null,
        }];
        let backend = build_datasources(&ok, &dir, &system).await.unwrap();
        assert!(!backend.is_empty(), "the markdown note was ingested");

        let bad = vec![DatasourceDecl {
            name: "x".into(),
            kind: "nope".into(),
            path: None,
            settings: serde_json::Value::Null,
        }];
        assert!(
            build_datasources(&bad, &dir, &system).await.is_err(),
            "an unknown datasource kind is a clean error"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A relative datasource `path` resolves against the PROGRAM FILE's directory, not the process cwd —
    /// so `flux app run <elsewhere>/support-bot.flux` indexes the `./docs` shipped beside the program even
    /// when launched from an unrelated directory. Here the workspace root (the "cwd") and the program dir
    /// are siblings: `./docs` must pull the program dir's corpus and ignore a decoy under the cwd root.
    #[tokio::test]
    async fn build_datasources_resolves_relative_path_against_program_dir() {
        use flux_datasource::SearchInput;
        use flux_lang::program::DatasourceDecl;
        use flux_system::{System, Workspace};

        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = base.join(format!("flux-ds-cwd-{}", std::process::id())); // the launch "cwd"
        let progdir = base.join(format!("flux-ds-prog-{}", std::process::id())); // where the .flux lives
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&progdir);
        std::fs::create_dir_all(progdir.join("docs")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            progdir.join("docs/faq.md"),
            "# FAQ\nReset your password from the account settings page.",
        )
        .unwrap();
        // A decoy under the cwd root — it must NOT be indexed (proves resolution is program-relative).
        std::fs::write(
            root.join("decoy.md"),
            "# Decoy\nkielbasa should not be indexed",
        )
        .unwrap();

        // The workspace is rooted at the cwd; the program dir is registered as a read-only root, exactly
        // as `run_app` does for an out-of-cwd program.
        let mut ws = Workspace::new(&root).unwrap();
        ws.add_read_root(&progdir).unwrap();
        let system = System::new(ws);

        let decls = vec![DatasourceDecl {
            name: "docs".into(),
            kind: "markdown".into(),
            path: Some("./docs".into()),
            settings: serde_json::Value::Null,
        }];
        let backend = build_datasources(&decls, &progdir, &system).await.unwrap();

        // The program dir's corpus is searchable...
        let hits = backend
            .search(&SearchInput {
                query: "reset password settings".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(
            hits.iter().any(|h| h.record.entity == "file.document"),
            "the ./docs beside the program was indexed"
        );
        // ...and the decoy under the cwd root was not.
        let decoy = backend
            .search(&SearchInput {
                query: "kielbasa".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(
            decoy.is_empty(),
            "a file under the cwd (not the program dir) must not be indexed"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&progdir).ok();
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
        let dir = std::fs::canonicalize(&dir).unwrap();
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
        let backend = build_datasources(&decls, &dir, &system).await.unwrap();

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
        // delete `<dir>/../flux-uninstall-traversal-sentinel.toml` — the traversal name below
        // MUST point exactly at this sentinel (one `..`), or a regression would `remove_file` a
        // non-existent path, return "no such plugin", and both assertions would pass vacuously.
        let outside = dir
            .parent()
            .unwrap()
            .join("flux-uninstall-traversal-sentinel.toml");
        std::fs::write(&outside, b"keep me").unwrap();

        let err = run_plugin_in(
            &dir,
            Some(PluginAction::Uninstall {
                name: "../flux-uninstall-traversal-sentinel".into(),
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

    #[test]
    fn ungrouped_plugin_ops_get_an_implicit_turn_intent_group() {
        let manifest = flux_plugin::PluginManifest {
            name: "slack".into(),
            groups: vec![flux_evidence::ToolGroup {
                name: "slack.health".into(),
                tools: vec!["slack.test".into()],
                surface_when: Vec::new(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let specs = vec![
            flux_spec::ToolSpec::read_only("slack.message.send", "send", json!({})),
            flux_spec::ToolSpec::read_only("slack.test", "test", json!({})),
        ];

        let group = implicit_plugin_group(&manifest, &specs).expect("one ungrouped operation");
        assert_eq!(group.name, "plugin.slack");
        assert_eq!(group.tools, vec!["slack.message.send"]);
        assert_eq!(group.surface_when.len(), 1);
        assert_eq!(group.surface_when[0].kind, flux_evidence::KIND_TURN_INTENT);
        assert_eq!(group.surface_when[0].signal.as_deref(), Some("slack"));
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

    /// A `CliSink` with an attached model spec + pricing table prices a turn's usage through the
    /// cost model end-to-end (the wiring that makes C-05's `cost()` live, not dead code). The codex
    /// path resolves on `gpt-5.5` and is labelled subscription spend (C-03 model resolution + C-05).
    #[test]
    fn sink_prices_a_codex_turn_as_subscription() {
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
    #[test]
    fn unpriced_model_renders_visible_marker() {
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
    #[test]
    fn cost_suffix_prefers_reported_cost_over_unpriced_marker() {
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
    /// `loop.phase` observation updates the phase-labeled spinner. Historical phase names remain
    /// supported, and a phase-less turn uses a neutral fallback.
    #[test]
    fn loop_phase_observations_drive_the_phase_labeled_spinner() {
        use flux_evidence::{Observation, Phase};

        let mut sink = super::CliSink::new(0);
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "working…",
            "no loop.phase observed yet -> neutral fallback"
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
            serde_json::json!({ "phase": "intent" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "routing intent…"
        );

        sink.observation(&Observation::new(
            "loop.phase",
            Phase::Turn,
            serde_json::json!({ "phase": "explore" }),
        ));
        assert_eq!(
            super::phase_spinner_label(sink.phase.as_deref(), sink.execute_rounds),
            "exploring…"
        );

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

    #[test]
    fn staged_intent_summary_is_concise_and_verbose_is_explicit() {
        let data = serde_json::json!({
            "intent": "  answer   the account and incident questions\nfrom evidence  ",
            "families": ["workspace.read"],
            "operations": ["glob", "read", "grep"]
        });
        assert_eq!(
            super::intent_lines(&data, false, 80),
            vec![
                "◆ intent: answer the account and incident questions from evidence",
                "  capabilities: workspace.read · 3 operations",
            ]
        );
        assert_eq!(
            super::intent_lines(&data, true, 80),
            vec![
                "◆ intent: answer the account and incident questions from evidence",
                "  capabilities: workspace.read · 3 operations",
                "  operations: glob, read, grep",
            ]
        );

        let none = serde_json::json!({
            "intent": "chat",
            "families": [],
            "operations": []
        });
        assert_eq!(
            super::intent_lines(&none, false, 80)[1],
            "  capabilities: none · 0 operations"
        );
    }

    #[test]
    fn first_planning_consultation_starts_cli_turn_timing_without_reset() {
        let mut sink = super::CliSink::new(0);
        assert!(sink.turn_start.is_none());
        sink.planning(true);
        let started = sink.turn_start.expect("planning starts the turn clock");
        sink.planning(false);
        sink.planning(true);
        assert_eq!(sink.turn_start, Some(started));
        sink.planning(false);
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

    /// The turn flags are scoped to the agent path, not leaked onto other subcommands — checked
    /// against the DECLARED arguments (not rendered help text), like
    /// `top_level_has_only_the_color_flag`, so a subcommand description that merely *mentions*
    /// `--continue` can't false-trip this.
    #[test]
    fn agent_flags_are_scoped_off_other_subcommands() {
        use clap::CommandFactory;
        let cmd = super::Cli::command();
        let longs_of = |name: &str| -> Vec<String> {
            cmd.find_subcommand(name)
                .unwrap_or_else(|| panic!("subcommand {name}"))
                .get_arguments()
                .filter_map(|a| a.get_long().map(String::from))
                .collect()
        };
        let has = |longs: &[String], flag: &str| longs.iter().any(|l| l == flag);
        for sub in ["sessions", "loop", "completion", "auth", "plugin"] {
            let longs = longs_of(sub);
            assert!(
                !has(&longs, "max-tokens"),
                "`{sub}` declares --max-tokens: {longs:?}"
            );
            assert!(
                !has(&longs, "continue"),
                "`{sub}` declares --continue: {longs:?}"
            );
        }
        // The agent-path subcommands carry the full turn-flag set.
        for agent_cmd in ["run", "tui"] {
            let longs = longs_of(agent_cmd);
            assert!(
                has(&longs, "max-tokens")
                    && has(&longs, "max-model-calls")
                    && has(&longs, "continue"),
                "`{agent_cmd}` should carry the turn flags: {longs:?}"
            );
        }
        // `review` carries only its scoped-down ReviewFlags: the session/approval flags its
        // FlowClient path can't honor are parse errors, not accepted-and-ignored.
        let review = longs_of("review");
        assert!(has(&review, "max-tokens"));
        assert!(
            !has(&review, "continue") && !has(&review, "resume"),
            "review must not accept session flags it ignores: {review:?}"
        );
        assert!(
            !has(&review, "yes"),
            "review must not accept --yes (it always auto-approves its fixed read-only flow)"
        );
        // `eval` has its own `-m` but not the turn-flag set.
        let eval = longs_of("eval");
        assert!(has(&eval, "model"), "eval should keep its own --model");
        assert!(
            !has(&eval, "max-tokens"),
            "eval should not carry the turn flags"
        );
    }

    /// The clap-level constraints reject contradictory or path-dead flag combinations at parse
    /// time (exit 2 + usage), instead of accepting-and-ignoring or failing deep in a handler.
    #[test]
    fn contradictory_flag_combinations_are_parse_errors() {
        use clap::Parser;
        let err = |args: &[&str]| {
            super::Cli::try_parse_from(args)
                .err()
                .unwrap_or_else(|| panic!("{args:?} should be rejected at parse time"));
        };
        // completion: an unknown shell is a usage error, not a silent empty script + exit 0.
        err(&["flux", "completion", "bassh"]);
        // fork: --prompt belongs to mode B (replan) only.
        err(&[
            "flux", "fork", "s_1", "--at", "2", "--inject", "1", "--prompt", "x",
        ]);
        err(&[
            "flux", "fork", "s_1", "--at", "2", "--edit", "f.flux", "--prompt", "x",
        ]);
        // flow run: --resume-value binds a halted await — meaningless without --resume.
        err(&["flux", "flow", "run", "f.flux", "--resume-value", "42"]);
        // changelog: one selection mode at a time.
        err(&["flux", "changelog", "0.11.6", "--all"]);
        err(&["flux", "changelog", "0.11.6", "--unreleased"]);
        err(&["flux", "changelog", "--all", "--unreleased"]);
        // plugin install: local-scan and remote modes are exclusive.
        err(&["flux", "plugin", "install", "--dir=some/dir", "gitlab"]);
        err(&["flux", "plugin", "install", "--all", "gitlab"]);
        // plugin call: --dry-run validates; --no-validate skips validation.
        err(&[
            "flux",
            "plugin",
            "call",
            "p",
            "op",
            "--dry-run",
            "--no-validate",
        ]);
        // skill surfaces: --global picks the install destination; --out is a different one.
        err(&["flux", "skill", "--global"]);
        err(&["flux", "plugin", "skill", "--global"]);
        err(&["flux", "plugin", "skill", "--install", "--out", "x.md"]);
        // Zero is invalid where it would alias (1-based --turn) or instantly fail/mislead.
        err(&["flux", "replay", "--turn", "0"]);
        err(&["flux", "run", "--max-tokens", "0", "hi"]);
        err(&["flux", "run", "--max-model-calls", "0", "hi"]);
        err(&["flux", "run", "--max-iterations", "0", "hi"]);
        err(&["flux", "run", "--turn-budget", "0", "hi"]);
        err(&["flux", "eval", "not-an-adapter"]);
        err(&["flux", "eval", "synthetic", "--trials", "0"]);
        // review's scoped-down flags: the flags its FlowClient path ignores are parse errors.
        err(&["flux", "review", "--files", "x.rs", "--yes"]);
        err(&["flux", "review", "--files", "x.rs", "--continue"]);
        // D-130: --sandbox and --no-sandbox are mutually exclusive.
        err(&["flux", "--sandbox", "--no-sandbox", "run", "hi"]);
    }

    /// …and the legitimate forms of the same flags still parse.
    #[test]
    fn valid_flag_combinations_parse() {
        use clap::Parser;
        let ok = |args: &[&str]| {
            super::Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        };
        ok(&["flux", "completion", "zsh"]);
        ok(&["flux", "completion"]);
        ok(&[
            "flux", "fork", "s_1", "--at", "2", "--replan", "--prompt", "x",
        ]);
        ok(&[
            "flux",
            "flow",
            "run",
            "f.flux",
            "--resume",
            "last",
            "--resume-value",
            "42",
        ]);
        ok(&["flux", "changelog", "0.11.6"]);
        ok(&["flux", "plugin", "install", "--dir"]);
        ok(&["flux", "plugin", "install", "--dir=plugins/target/release"]);
        ok(&["flux", "plugin", "install", "gitlab", "slack@1.2.0"]);
        ok(&["flux", "plugin", "install", "--all"]);
        ok(&["flux", "skill", "--install", "--global"]);
        ok(&["flux", "replay", "--turn", "1"]);
        ok(&["flux", "eval", "terminal-bench"]);
        ok(&["flux", "eval", "multi", "--members", "synthetic,mock"]);
        // D-130: --sandbox and --no-sandbox parse fine on their own (only combined do they conflict).
        ok(&["flux", "--sandbox", "run", "hi"]);
        ok(&["flux", "--no-sandbox", "run", "hi"]);
        // --serve's optional value: the common documented shape (no program, space-separated
        // address) still parses; a program BEFORE a bare --serve avoids the ambiguity entirely.
        ok(&["flux", "app", "run", "--serve", "0.0.0.0:1234", "--yes"]);
        ok(&["flux", "app", "run", "p.flux", "--serve", "--yes"]);
        ok(&[
            "flux",
            "app",
            "run",
            "p.flux",
            "--serve=0.0.0.0:1234",
            "--yes",
        ]);
        ok(&["flux", "review", "--files", "x.rs", "-m", "mock"]);
    }

    /// `program_resolves` PATH-searches a bare name. A one-component relative path has
    /// `Path::parent() == Some("")`, which must not be mistaken for "has a directory component" —
    /// that pre-fix bug reported every bare-name plugin as `missing` in `flux plugin status`
    /// while `call` (which spawns via PATH) worked fine.
    #[test]
    fn program_resolves_finds_bare_names_on_path() {
        let dir =
            std::env::temp_dir().join(format!("flux-program-resolves-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("flux-plugin-resolve-probe");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let _guard = EnvVarGuard::new("PATH");
        let old = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{old}", dir.display()));

        assert!(
            super::program_resolves("flux-plugin-resolve-probe"),
            "bare name on PATH must resolve"
        );
        assert!(!super::program_resolves("flux-plugin-definitely-absent"));
        // A path with a separator is checked directly, never PATH-searched.
        assert!(super::program_resolves(bin.to_str().unwrap()));
        assert!(!super::program_resolves("./flux-plugin-resolve-probe"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `flux run <app.flux> extra words` errors loudly — before the fix, everything after the
    /// program path was silently discarded.
    #[tokio::test]
    async fn run_app_cmd_rejects_trailing_words() {
        let flags = super::AgentFlags::from_model_yes(Some("mock"), true);
        let err = super::run_app_cmd(
            vec!["app.flux".into(), "with".into(), "inputs".into()],
            &flags,
        )
        .await
        .expect_err("trailing words after a program path must error");
        assert!(
            err.to_string().contains("takes no further arguments"),
            "got: {err:#}"
        );
    }

    /// `--members` pairs with the `multi` adapter only — both mismatches are caught before any
    /// suite runs (previously: multi-without-members failed deep in flux-eval, members-without-
    /// multi was silently ignored).
    #[tokio::test]
    async fn eval_members_pairing_is_validated_up_front() {
        let err = super::run_eval_cmd(
            super::EvalAdapter::Multi,
            vec![],
            vec![],
            0,
            1,
            None,
            false,
            None,
        )
        .await
        .expect_err("multi without --members");
        assert!(err.to_string().contains("--members"), "got: {err:#}");
        let err = super::run_eval_cmd(
            super::EvalAdapter::Synthetic,
            vec![],
            vec!["mock".into()],
            0,
            1,
            None,
            false,
            None,
        )
        .await
        .expect_err("--members without multi");
        assert!(err.to_string().contains("--members"), "got: {err:#}");
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
        assert!(loop_machinery_label("detect_intent", &json!({}))
            .unwrap()
            .contains("classify the request"));
        assert!(loop_machinery_label("execute_batch", &json!({}))
            .unwrap()
            .contains("approved actions"));
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
    /// unnamed halted flow, including a host-derived action flow from an agent turn — same store,
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

    /// Restores (or removes) an env var on drop — panic-safe cleanup for env-mutating tests, so a
    /// failed assertion can't leak a widened grant into every later test in the process.
    struct EnvVarGuard {
        key: &'static str,
        prior: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn new(key: &'static str) -> Self {
            Self {
                key,
                prior: std::env::var_os(key),
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// D-96: the ephemeral `--allow-private-net` override widens the *operator* grant to `*` for this
    /// process and stamps a distinct `cli:--allow-private-net` audit grant-source, while its absence
    /// preserves deny-by-default (an empty config yields no private grant). The manifest-declaration
    /// intersection that still gates each plugin lives in `flux_plugin::SystemHostCaps` and is covered
    /// there; this pins the CLI-surface wiring — including the truthy-value semantics: an explicit
    /// "off" value (`0`) must never widen an SSRF-relevant grant.
    #[test]
    fn allow_private_net_override_widens_grant_and_labels_audit() {
        let cfg = flux_config::Config::default();
        let _guard = EnvVarGuard::new("FLUX_ALLOW_PRIVATE_NET");

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

        // An explicit "off" value stays OFF — presence alone must not grant (the pre-fix bug).
        for off in ["0", "false", "no", "off", ""] {
            std::env::set_var("FLUX_ALLOW_PRIVATE_NET", off);
            assert!(
                !super::private_net_cli_override(),
                "FLUX_ALLOW_PRIVATE_NET={off:?} must not widen the grant"
            );
            assert!(super::effective_plugin_private_hosts(&cfg, "gitlab").is_empty());
        }

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
    }

    /// D-130 (findings 6/7/9b): `apply_sandbox_env` resolves posture **tightest-wins** — the
    /// strictest of `Require > On > Off` across `--sandbox`, a pre-set `FLUX_SANDBOX`, and config —
    /// so a laxer source can never silently downgrade a stricter one; the sole override is the
    /// explicit kill switch (`--no-sandbox` / `FLUX_SANDBOX=off`). The startup preflight then fails
    /// closed under `require` when no backend is usable.
    ///
    /// Real backends shipped (D-131 bubblewrap, D-132 Seatbelt), so this forces BOTH discovery
    /// vars — `FLUX_BWRAP_BIN` (Linux) and `FLUX_SANDBOX_EXEC_BIN` (macOS) — at nonexistent paths so
    /// the backend resolves `Unsupported` deterministically on either platform (finding 9b: forcing
    /// only `FLUX_BWRAP_BIN` let macOS resolve a real Seatbelt backend and the `.unwrap_err()`
    /// below panicked). `FLUX_SANDBOXED` is cleared too, so an ambient nested-run marker can't make
    /// `resolve()` report `AlreadyConfined` (which would satisfy `require` and defeat the test).
    #[test]
    fn apply_sandbox_env_resolves_tightest_wins_and_fails_closed_under_require() {
        use clap::Parser;

        let _g_mode = EnvVarGuard::new("FLUX_SANDBOX");
        let _g_net = EnvVarGuard::new("FLUX_SANDBOX_NET");
        let _g_writable = EnvVarGuard::new("FLUX_SANDBOX_WRITABLE");
        let _g_bwrap = EnvVarGuard::new("FLUX_BWRAP_BIN");
        let _g_exec = EnvVarGuard::new("FLUX_SANDBOX_EXEC_BIN");
        let _g_confined = EnvVarGuard::new("FLUX_SANDBOXED");
        std::env::set_var(
            "FLUX_BWRAP_BIN",
            "/nonexistent/definitely-not-a-real-bwrap-d126",
        );
        std::env::set_var(
            "FLUX_SANDBOX_EXEC_BIN",
            "/nonexistent/definitely-not-a-real-sandbox-exec-d132",
        );
        // No ambient "already confined by a parent flux" marker — that would satisfy `require`.
        std::env::remove_var("FLUX_SANDBOXED");

        let bare = super::Cli::try_parse_from(["flux", "run", "hi"]).unwrap();
        let sandboxed = super::Cli::try_parse_from(["flux", "--sandbox", "run", "hi"]).unwrap();
        let no_sandbox = super::Cli::try_parse_from(["flux", "--no-sandbox", "run", "hi"]).unwrap();

        let mut cfg_require = flux_config::Config::default();
        cfg_require.sandbox.require = true;

        // Nothing set anywhere: off, and no startup error.
        std::env::remove_var("FLUX_SANDBOX");
        super::apply_sandbox_env(&bare, &flux_config::Config::default()).unwrap();
        assert_eq!(std::env::var("FLUX_SANDBOX").as_deref(), Ok("off"));

        // Config alone (`require`) propagates when nothing else overrides it, and — with no usable
        // backend (forced above) — fails closed at the startup preflight.
        std::env::remove_var("FLUX_SANDBOX");
        let err = super::apply_sandbox_env(&bare, &cfg_require).unwrap_err();
        assert!(err.to_string().contains("unavailable"), "{err}");
        assert_eq!(
            std::env::var("FLUX_SANDBOX").as_deref(),
            Ok("require"),
            "the var is exported even though the call then errors"
        );

        // (a) TIGHTEST-WINS: `--sandbox` (asks for `On`) alongside config `require` resolves to
        // `Require`, NOT `On` — the soft flag must not downgrade the fail-closed config posture
        // (finding 6). So it still fails closed against the unavailable backend.
        std::env::remove_var("FLUX_SANDBOX");
        let err = super::apply_sandbox_env(&sandboxed, &cfg_require).unwrap_err();
        assert!(err.to_string().contains("unavailable"), "{err}");
        assert_eq!(
            std::env::var("FLUX_SANDBOX").as_deref(),
            Ok("require"),
            "tightest-wins: --sandbox must not downgrade a configured `require` to `on`"
        );

        // (b) A pre-set `FLUX_SANDBOX` that is empty or a typo must NOT downgrade config `require` —
        // the old `_ => Off` arm silently dropped a fail-closed posture (finding 6). Both still
        // resolve to `Require` and fail closed.
        for garbage in ["", "requird"] {
            std::env::set_var("FLUX_SANDBOX", garbage);
            let err = super::apply_sandbox_env(&bare, &cfg_require).unwrap_err();
            assert!(err.to_string().contains("unavailable"), "{err}");
            assert_eq!(
                std::env::var("FLUX_SANDBOX").as_deref(),
                Ok("require"),
                "a garbage FLUX_SANDBOX={garbage:?} must not downgrade a configured `require`"
            );
        }

        // A pre-set `on` with default config is a soft request: it only warns (Ok), never fails
        // closed — `On`-mode auto-degrades against the unavailable backend.
        std::env::set_var("FLUX_SANDBOX", "on");
        super::apply_sandbox_env(&bare, &flux_config::Config::default()).unwrap();
        assert_eq!(std::env::var("FLUX_SANDBOX").as_deref(), Ok("on"));

        // (c) `--no-sandbox` is the kill switch: forces Off over a pre-set `require` env AND config.
        std::env::set_var("FLUX_SANDBOX", "require");
        super::apply_sandbox_env(&no_sandbox, &cfg_require).unwrap();
        assert_eq!(std::env::var("FLUX_SANDBOX").as_deref(), Ok("off"));

        // (c) A pre-set `FLUX_SANDBOX=off` is the other kill switch: forces Off even over config
        // `require` (mirrors `FLUX_OP_CACHE=off`).
        std::env::set_var("FLUX_SANDBOX", "off");
        super::apply_sandbox_env(&bare, &cfg_require).unwrap();
        assert_eq!(
            std::env::var("FLUX_SANDBOX").as_deref(),
            Ok("off"),
            "FLUX_SANDBOX=off is the kill switch, even over config `require`"
        );

        // `--sandbox` with no pre-set env and default config resolves to `On` (soft): warns and
        // runs unconfined against the unavailable backend, no error.
        std::env::remove_var("FLUX_SANDBOX");
        super::apply_sandbox_env(&sandboxed, &flux_config::Config::default()).unwrap();
        assert_eq!(std::env::var("FLUX_SANDBOX").as_deref(), Ok("on"));

        // Network: an explicit `false` in config narrows and is exported; the default stays open
        // and exports nothing (mirrors FLUX_ADD_DIRS' "only set what changes" style). Applies
        // regardless of mode.
        std::env::remove_var("FLUX_SANDBOX");
        std::env::remove_var("FLUX_SANDBOX_NET");
        let mut cfg_net = flux_config::Config::default();
        cfg_net.sandbox.network = Some(false);
        super::apply_sandbox_env(&bare, &cfg_net).unwrap();
        assert_eq!(std::env::var("FLUX_SANDBOX_NET").as_deref(), Ok("0"));

        std::env::remove_var("FLUX_SANDBOX_NET");
        super::apply_sandbox_env(&bare, &flux_config::Config::default()).unwrap();
        assert!(
            std::env::var("FLUX_SANDBOX_NET").is_err(),
            "the unrestricted default exports nothing"
        );

        // Writable: config entries are absolutized against the cwd and exported as a `:`-list.
        std::env::remove_var("FLUX_SANDBOX_WRITABLE");
        let mut cfg_writable = flux_config::Config::default();
        cfg_writable.sandbox.writable = vec!["relative-sandbox-dir".to_string()];
        super::apply_sandbox_env(&bare, &cfg_writable).unwrap();
        let exported = std::env::var("FLUX_SANDBOX_WRITABLE").unwrap();
        assert!(
            std::path::Path::new(&exported).is_absolute(),
            "expected an absolutized path, got {exported:?}"
        );
        assert!(exported.ends_with("relative-sandbox-dir"), "{exported:?}");

        std::env::remove_var("FLUX_SANDBOX");
        std::env::remove_var("FLUX_SANDBOX_NET");
        std::env::remove_var("FLUX_SANDBOX_WRITABLE");
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
