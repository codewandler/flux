use super::*;

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
pub(super) struct Cli {
    /// A subcommand (run `flux help` to list them). With none, `flux` opens the interactive REPL.
    #[command(subcommand)]
    pub(super) command: Option<Commands>,

    /// When to colorize output: auto (stdout AND stderr are terminals, `NO_COLOR` unset),
    /// always, or never.
    #[arg(long, value_enum, default_value_t, global = true)]
    pub(super) color: style::ColorChoice,

    /// Read and write sessions in DIR (`<DIR>/events.db` + `<DIR>/flow.db`) instead of the default
    /// `~/.flux`. This is the same layout `Storage::dir` writes and `flux record` produces, so
    /// `flux replay|fork|diff|sessions --store tests/scenarios/<name>` inspects a committed fixture
    /// with the ordinary session tools. Exported as `FLUX_STORE_DIR` so subprocess paths inherit it.
    #[arg(long = "store", value_name = "DIR", global = true)]
    pub(super) store: Option<std::path::PathBuf>,

    /// Grant READ access to an additional directory outside the workspace (repeatable). Reads, `glob`,
    /// and `grep` may reach under it; writes stay confined to the current directory. Layers over
    /// `[workspace] add_dirs` in .flux/config.toml (exported as `FLUX_ADD_DIRS` so `app run` inherits it).
    #[arg(long = "add-dir", value_name = "DIR", global = true)]
    pub(super) add_dir: Vec<std::path::PathBuf>,

    /// Lift the filesystem sandbox entirely — read AND write anywhere on disk. Dangerous; prints a
    /// warning. Prefer `--add-dir` for read-only access to specific directories.
    #[arg(long = "allow-all-paths", global = true)]
    pub(super) allow_all_paths: bool,

    /// Temporarily allow egress to private/internal network addresses for THIS invocation only —
    /// the ephemeral, audited equivalent of a `[private_net]` config grant (no config edit, nothing
    /// persisted). Plugins still only reach the private hosts their manifest declares; `web.fetch`
    /// is opened for the run (its guard has no manifest safeguard, so this re-exposes cloud-metadata
    /// and RFC-1918 ranges to any fetched URL). Prefer a scoped `[private_net.plugins]` grant for
    /// anything recurring. Exported as `FLUX_ALLOW_PRIVATE_NET` so `app run`/`plugin call` inherit it.
    #[arg(long = "allow-private-net", global = true)]
    pub(super) allow_private_net: bool,

    /// Turn on OS-level process sandboxing (bubblewrap on Linux, Seatbelt on macOS) for spawned
    /// shell/plugin processes — defense-in-depth underneath the safety envelope, orthogonal to
    /// approvals. Off by default; layers over `[sandbox]` in .flux/config.toml (the strictest of
    /// this flag, a pre-set `FLUX_SANDBOX`, and config wins). Exported as `FLUX_SANDBOX` so
    /// `app run`/`plugin call` and other subprocess/child-flux paths inherit it. If no usable
    /// backend is available (unsupported platform, or the wrapper is missing/blocked) this degrades
    /// to a one-line warning and runs unconfined — unless `[sandbox] require` (or
    /// `FLUX_SANDBOX=require`) is set, which fails closed at startup. `--no-sandbox` is the kill
    /// switch. Auto-approved noninteractive and serving surfaces instead default to fail-closed
    /// `require` with sandbox network denied; `--no-sandbox` is their explicit, prominently warned
    /// outer-container/VM escape. See docs/designs/process-sandboxing.md.
    #[arg(long = "sandbox", global = true, conflicts_with = "no_sandbox")]
    pub(super) sandbox: bool,

    /// Force OS-level sandboxing OFF for this invocation — the kill switch, overriding `--sandbox`,
    /// a pre-set `FLUX_SANDBOX`, and `[sandbox]` config. On an unattended/serving surface this is an
    /// explicit escape that prints a prominent UNCONFINED startup posture; provide equivalent
    /// filesystem and network isolation in an outer container or VM.
    #[arg(long = "no-sandbox", global = true, conflicts_with = "sandbox")]
    pub(super) no_sandbox: bool,
}

/// The flags for running an agent turn — flattened into the agent-path subcommands (`run`,
/// `tui`, `fork`, `app run`), so they live on those commands and stay off every other subcommand's
/// help. (`--color` is `global` on [`Cli`] instead; it applies to every command. `review` carries
/// its own smaller [`ReviewFlags`].) `fork` and `app run <program>` reject the session/turn flags
/// their paths can't honor at runtime (see `run_fork`/`run_app`).
#[derive(clap::Args, Debug)]
pub(super) struct AgentFlags {
    /// (Hidden) Non-interactive print mode — a bare prompt is already one-shot, so this is a no-op alias.
    #[arg(short = 'p', long = "print", hide = true)]
    pub(super) print: bool,

    /// Fully-qualified `provider/model` spec. Provider must be one of:
    ///   `anthropic` (API key), `claude` (OAuth/subscription), `openai`, `codex`, `aws` (Claude
    ///   via AWS Bedrock; credentials from the AWS chain — env, `aws sso login` + `AWS_PROFILE`,
    ///   IRSA, or EKS Pod Identity — no `aws` CLI needed), `openrouter` (every model over its
    ///   native Anthropic Messages endpoint — structured tool calls, and prompt caching on
    ///   `anthropic/…` slugs; specs read `openrouter/<vendor>/<model>`),
    ///   `ollama` (local, OpenAI Chat wire), `ollama-anthropic` (local Messages endpoint).
    ///   Short aliases `sonnet`, `opus`, `haiku`, `fable` are shorthands for `anthropic/<model>`;
    ///   bare `claude` is shorthand for `claude/sonnet` (the subscription's default model); bare
    ///   `codex` is shorthand for `codex/gpt-5.6-sol` (the ChatGPT-subscription main model; the
    ///   legacy `*-codex` ids are rejected by the backend); bare `aws` (or `aws/sonnet`,
    ///   `aws/opus`, `aws/haiku`) resolves to the region's Bedrock inference profile.
    /// Examples: `claude/claude-sonnet-4-6`, `openai/gpt-5.6`, `codex/gpt-5.6-sol`,
    ///   `aws/us.anthropic.claude-sonnet-4-6`, `openrouter/anthropic/claude-opus-4.6`.
    /// Overrides `model` in `.flux/config.toml`; falls back to `sonnet` (= `anthropic/claude-sonnet-5`).
    #[arg(short = 'm', long)]
    pub(super) model: Option<String>,

    /// Execute guarded tool effects on this remote-system HTTPS endpoint. Omit for the native local
    /// substrate. Model calls, credentials, approvals, session storage, and authored definitions
    /// remain local; the selected execution target is immutable for the session and inherited by
    /// sub-agents.
    #[arg(long, value_name = "HTTPS_URL")]
    pub(super) remote: Option<String>,

    /// Name of the environment variable containing the remote-system bearer token. The token is
    /// never accepted as a command-line value. Used only with `--remote`.
    #[arg(
        long = "remote-token-env",
        value_name = "ENV",
        default_value = "FLUX_REMOTE_SYSTEM_TOKEN",
        requires = "remote"
    )]
    pub(super) remote_token_env: String,

    /// PEM certificate for an additional private CA trusted by the remote-system client.
    #[arg(long = "remote-ca", value_name = "PEM", requires = "remote")]
    pub(super) remote_ca: Option<std::path::PathBuf>,

    /// Execute guarded tool effects on the named `[[host]]` binding. The binding must be granted
    /// to this surface class — the default posture is deny — and the selected target is immutable
    /// for the session and inherited by sub-agents. `--remote` remains as sugar for an ephemeral
    /// unnamed binding.
    #[arg(long, value_name = "NAME", conflicts_with = "remote")]
    pub(super) host: Option<String>,

    /// Ask capable providers/models to expose adaptive thinking for every call owned by this agent.
    #[arg(long)]
    pub(super) think: bool,

    /// Reasoning effort for intent, exploration, presentation, compaction, cognition, and inherited
    /// sub-agent calls.
    #[arg(long, value_enum)]
    pub(super) effort: Option<EffortArg>,

    /// Agent outer loop: `adaptive` (default) or a Flux-Lang source file. A file is selected only
    /// when named here; `.flux/agent-loop.flux` has no implicit effect.
    #[arg(long = "loop", value_name = "ADAPTIVE|FILE")]
    pub(super) agent_loop: Option<String>,

    /// Internal source-free receipt for a snapshotted `--loop`. Fleet uses this to reconstruct the
    /// exact admitted binding on message/resume/rework; ordinary callers should select `--loop`.
    #[arg(
        long = "resolved-loop-binding",
        value_name = "FILE",
        hide = true,
        requires = "agent_loop"
    )]
    pub(super) resolved_loop_binding: Option<String>,

    /// Maximum tokens per model-stage call. A truncated intent, exploration, repair, or presentation
    /// stage fails loudly rather than silently stopping. Zero would fail at the provider, so it is
    /// rejected at parse time.
    #[arg(long, default_value_t = 16384, value_parser = clap::value_parser!(u32).range(1..))]
    pub(super) max_tokens: u32,

    /// Maximum provider calls across one logical adaptive turn, including intent repairs,
    /// exploration, and every decision resume. Overrides `[agent.adaptive] max_model_calls`.
    #[arg(long, value_parser = parse_positive_usize)]
    pub(super) max_model_calls: Option<usize>,

    /// Maximum decision/batch iterations in the authored outer loop (1–1,000). This is separate
    /// from model calls: one iteration may execute a batch, ask a question, or continue from a report.
    /// Overrides `[agent] max_iterations`.
    #[arg(long, value_parser = parse_positive_usize)]
    pub(super) max_iterations: Option<usize>,

    /// Per-turn token budget (all tiers, summed across the turn's model calls): once crossed, the
    /// turn ends honestly with a budget-exceeded answer instead of consulting the model again.
    /// Overrides `FLUX_TURN_TOKEN_BUDGET` and `[limits] turn_token_budget` in .flux/config.toml.
    /// Off by default (no ceiling) — 0 would mean "instantly exceeded", not "off", so it is
    /// rejected at parse time.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub(super) turn_budget: Option<u64>,

    /// (Hidden) Print token usage — accepted for CLI compatibility; currently a no-op (usage/cost
    /// is always shown on the turn-end rule; see also `flux usage`).
    #[arg(long, hide = true)]
    pub(super) usage: bool,

    /// (Hidden, deprecated) The Flux-Lang engine is the default for a bare prompt; this is a no-op.
    #[arg(long, hide = true)]
    pub(super) agent: bool,

    /// Auto-approve every tool call (headless). Without it, unmatched calls prompt for approval.
    /// The older spelling of `--posture bounded-autonomy`, and unchanged by it.
    #[arg(long)]
    pub(super) yes: bool,

    /// The autonomy posture this run adopts — one named choice that selects who approves, how the
    /// run is confined and what its budget is, together:
    ///
    /// `supervised` (default) — you answer each guarded effect at the terminal.
    /// `bounded-autonomy` — never prompt; authorization policy, a fail-closed OS sandbox with the
    /// network closed, and resource budgets constrain instead. Same as `--yes`.
    /// `exploratory` — never prompt, and treat interruption as the harm: fail-closed sandbox,
    /// network open, wider ceilings, uncapped evidence. For research, security hardening and long
    /// exploration.
    /// `refusing` — refuse every effect that reaches the approval stage.
    ///
    /// None of these is a reduced form of another; each states what it relies on and what it does
    /// not protect against (`docs/agent/safety`).
    #[arg(long, value_name = "NAME", value_parser = parse_autonomy_posture)]
    pub(super) posture: Option<flux_runtime::AutonomyPosture>,

    /// Show tool output in full (no truncation). Action batches and tool inputs are always shown in
    /// full; this also un-caps tool *output* (e.g. large file reads). Also enabled by `FLUX_VERBOSE`.
    #[arg(short = 'v', long)]
    pub(super) verbose: bool,

    /// Reveal the agent loop: stream its typed stages (`detect_intent`/`explore`/batch approval and
    /// execution/…) that are filtered from the surface by default. Also enabled by
    /// `FLUX_SHOW_LOOP`. See `flux loop show` for the authored loop and `/evidence` for the audit trail.
    #[arg(long)]
    pub(super) show_loop: bool,

    /// Trace the outer agent loop's structure: one dim line per round (`⟳ round 3/50`) and per
    /// structural node (op calls with bind names, match/when branches taken, return) of the
    /// agent-loop program. Native leaf-operation execution is not traced. Also enabled by
    /// `FLUX_TRACE_LOOP`.
    #[arg(long)]
    pub(super) trace_loop: bool,

    /// Extra skill directory, layered above `[skills] dirs` from .flux/config.toml and the
    /// well-known set (`.flux/skills`, `.claude/skills`, `~/.flux/skills`, …). Repeatable; earlier
    /// dirs win a skill-name clash.
    #[arg(long = "skill-dir", value_name = "DIR")]
    pub(super) skill_dirs: Vec<std::path::PathBuf>,

    /// Explicitly enable a discovered skill by name. Repeatable. Skills are never activated from
    /// prompt keywords automatically; `--skill-dir` and config directories only affect discovery.
    #[arg(long = "skill", value_name = "NAME")]
    pub(super) skills: Vec<String>,

    /// Opt into Claude-style progressive skill disclosure (D-188): surface every discovered,
    /// non-`disable-model-invocation` skill's name+description to the model, and let it pull a
    /// skill's full body into context on demand via `skill.load`. Off by default — manual
    /// `--skill` activation stays the measured-cheaper default path
    /// (`docs/designs/manual-skill-activation.md`); this is additive, not a replacement. Also
    /// settable via `[skills] model_invoked` in `.flux/config.toml`.
    #[arg(long = "skills-model-invoked")]
    pub(super) skills_model_invoked: bool,

    /// Continue the most recent session instead of starting a new one.
    #[arg(short = 'c', long)]
    pub(super) continue_: bool,

    /// Resume the most recent session (equivalent to --continue; used by hot-reload).
    #[arg(long)]
    pub(super) resume: bool,

    /// Host-owned exact session continuation. Fleet uses this instead of "latest" because the
    /// coordinator's correlated research children intentionally share its audit store and may be
    /// newer sessions than the coordinator itself.
    #[arg(
        long = "resume-session",
        value_name = "SESSION",
        hide = true,
        conflicts_with_all = ["continue_", "resume"],
        requires = "operation_ceiling"
    )]
    pub(super) resume_session: Option<String>,

    /// Host-owned marker for an exact operation ceiling. Fleet supplies this only to admitted
    /// worker subprocesses; ordinary CLI users should declare capabilities in authored programs.
    #[arg(long = "operation-ceiling", hide = true)]
    pub(super) operation_ceiling: bool,

    /// One operation in the host-owned ceiling. Repeated and meaningful only with
    /// `--operation-ceiling`; the empty set deliberately exposes no model-facing operations.
    #[arg(
        long = "operation",
        value_name = "NAME",
        hide = true,
        requires = "operation_ceiling"
    )]
    pub(super) admitted_operations: Vec<String>,

    /// Host-owned marker for the durable native Fleet main coordinator subprocess. This selects a
    /// closed native Board/Fleet catalog; it is not a general user capability flag.
    #[arg(long = "native-fleet-main", hide = true)]
    pub(super) native_fleet_main: bool,

    /// Host-owned marker for the dedicated wave-integrator subprocess. This selects a closed
    /// two-operation catalog (assemble a wave, read Fleet status) that is disjoint from the
    /// coordinator's; it is not a general user capability flag.
    #[arg(long = "native-fleet-integrator", hide = true)]
    pub(super) native_fleet_integrator: bool,

    /// Dev mode: enables hot-reload (`flux_reload` tool) and other developer tools.
    #[arg(long)]
    pub(super) dev: bool,
}

/// The flags `flux review` actually consumes — deliberately NOT the full [`AgentFlags`] set.
/// Review runs the embedded strict-review flow through `flux_sdk::FlowClient`, so the turn flags
/// (`--continue`/`--resume`, `--turn-budget`, `--skill-dir`, `--dev`, `-v`, `--yes`, …) have no
/// effect on that path; offering them would accept-and-ignore, so they are rejected at parse time
/// instead. (Review always auto-approves its own fixed, read-only flow under the unattended sandbox
/// profile — see `run_review`.)
#[derive(clap::Args, Debug)]
pub(super) struct ReviewFlags {
    /// Fully-qualified `provider/model` spec the reviewer sub-agents run (same forms as `flux run -m`).
    #[arg(short = 'm', long)]
    pub(super) model: Option<String>,

    /// Maximum tokens per reviewer model call.
    #[arg(long, default_value_t = 16384, value_parser = clap::value_parser!(u32).range(1..))]
    pub(super) max_tokens: u32,
}

/// A standalone parser wrapper used only to materialize a default-populated [`AgentFlags`] from
/// synthesized args (see [`AgentFlags::from_model_yes`]). Going through clap preserves field defaults
/// like `max_tokens` that a hand-built `Default` would zero out.
#[derive(Parser, Debug)]
pub(super) struct AgentFlagsOnly {
    #[command(flatten)]
    pub(super) agent: AgentFlags,
}

impl AgentFlags {
    /// Build agent flags from just a model spec + `--yes` — the entry points (`flux flow run`,
    /// `flux preset --run`, and the bare `flux` REPL) that run an agent without the full turn-flag CLI.
    /// Preserves clap's field defaults (e.g. `max_tokens = 16384`). The args are synthesized here, so
    /// the parse never fails.
    pub(super) fn from_model_yes(model: Option<&str>, yes: bool) -> Self {
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

    /// The [`AutonomyPosture`](flux_runtime::AutonomyPosture) this invocation runs under — the
    /// single value that decides its approver, its OS-sandbox floor and its resource budget (C-463).
    ///
    /// ⚠ **No flag day.** `--yes` is not a fourth setting sitting beside `--posture`; it is the
    /// older spelling of `bounded-autonomy` and keeps meaning exactly what it always meant — never
    /// prompt, and let authorization policy, the fail-closed sandbox floor (C-262 / C-410) and the
    /// resource budgets constrain instead. Naming the posture is that same choice said out loud.
    ///
    /// A contradiction is refused rather than resolved. `--yes --posture supervised` is two
    /// opposite instructions, and quietly picking a winner would leave the operator's command line
    /// and the run's behaviour disagreeing about whether anyone is being asked.
    pub(super) fn autonomy_posture(&self) -> anyhow::Result<flux_runtime::AutonomyPosture> {
        use flux_runtime::{ApprovalStance, AutonomyPosture};
        match (self.posture, self.yes) {
            (Some(posture), true) if posture.approval() != ApprovalStance::None => {
                anyhow::bail!(
                    "`--yes` and `--posture {posture}` are opposite approval postures: `--yes` \
                     never asks, while `{posture}` {}. Pick one — `--yes` on its own is \
                     `--posture bounded-autonomy`",
                    match posture.approval() {
                        ApprovalStance::PerEffect => "asks a human before each guarded effect",
                        _ => "refuses every effect that reaches the approval stage",
                    }
                )
            }
            (Some(posture), _) => Ok(posture),
            (None, true) => Ok(AutonomyPosture::for_auto_approval()),
            // The CLI's default really is a human at a terminal; a surface with no terminal
            // (`app run --serve`) chooses its own and never reaches this.
            (None, false) => Ok(AutonomyPosture::Supervised),
        }
    }

    /// The posture for a CLI surface with **no terminal to prompt at** — `flux record`, and
    /// `flux app run <program>`, whose channels (cron, webhook, Slack) fire with no operator
    /// attached.
    ///
    /// Same resolution as [`autonomy_posture`](Self::autonomy_posture), except that `supervised` is
    /// not on the menu because there is nobody to ask. Unstated resolves to `refusing` — which is
    /// what these surfaces have always installed — and *explicitly* naming `supervised` is refused
    /// rather than quietly downgraded, because a stated posture the surface silently replaces is
    /// the accident the named postures exist to prevent.
    pub(super) fn headless_posture(
        &self,
        surface: &str,
    ) -> anyhow::Result<flux_runtime::AutonomyPosture> {
        use flux_runtime::AutonomyPosture;
        let posture = self.autonomy_posture()?;
        if posture == AutonomyPosture::Supervised {
            if self.posture.is_some() {
                anyhow::bail!(
                    "`--posture supervised` asks a human before each guarded effect, and {surface} \
                     has no terminal to ask at. Choose `bounded-autonomy` (never prompt; \
                     authorization policy, a fail-closed sandbox and resource budgets constrain \
                     instead), `exploratory`, or `refusing`"
                );
            }
            return Ok(AutonomyPosture::Refusing);
        }
        Ok(posture)
    }

    /// The **explicitly named** posture, if any — the only form that contributes a sandbox floor of
    /// its own in `apply_sandbox_env`.
    ///
    /// `--yes` is deliberately not read here even though it maps onto `bounded-autonomy`. That
    /// posture's floor is exactly what `unattended_sandbox_surface` already contributes for the
    /// flag, and the exemptions that classifier carries (the TUI, where an operator is watching the
    /// whole run) are decisions about *surfaces*, not about postures. Inferring a floor from the
    /// older spelling would silently confine those surfaces — a flag day this story rules out.
    pub(super) fn named_posture(&self) -> Option<flux_runtime::AutonomyPosture> {
        self.posture
    }
}

/// Parse a `--posture` value, listing the four names on a typo rather than guessing one.
fn parse_autonomy_posture(value: &str) -> Result<flux_runtime::AutonomyPosture, String> {
    value.parse().map_err(|e: flux_core::Error| e.to_string())
}

/// The flux subcommands. Each renders its own `flux <cmd> --help`. With no subcommand, `flux` opens
/// the interactive REPL; any unrecognized first token is a clap "unrecognized subcommand" error (so a
/// stray word never launches an autonomous turn — use `flux run <prompt>`).
#[derive(clap::Subcommand, Debug)]
pub(super) enum Commands {
    // NOTE: `agent_flags` (below the enum) must cover every variant that flattens [`AgentFlags`] —
    // it feeds the pre-runtime `apply_agent_env` export in `main`.
    /// Run the agent on a prompt, or a multi-agent program: `flux run <prompt…>` / `flux run <app.flux>`.
    Run {
        #[command(flatten)]
        agent: AgentFlags,
        /// (C-160, preview — unstable, see docs/designs/ndjson-agent-protocol.md) Emit one JSON
        /// object per line to stdout instead of human-rendered output — turn start/end, plan,
        /// per-op dispatch + result, approval request/decision, usage/cost, and error, each line
        /// carrying a `type` + schema version `v`. Diagnostics still go to stderr, so the stream is
        /// `jq`-parseable with no filtering. Not supported for `flux run <app.flux>`.
        #[arg(long = "stream-json")]
        stream_json: bool,
        /// (C-160, preview) Also read the same NDJSON framing on stdin, for a multi-message
        /// conversation in one process: a plain `{"text": "..."}` line queues the next turn, a
        /// `{"text": "...", "steer": true}` line injects into the CURRENTLY RUNNING turn instead
        /// (A-94). Requires `--yes` — v1 has no interactive-approval framing over the input stream,
        /// since this reader and the interactive approval prompt would otherwise both read stdin.
        #[arg(long = "stream-json-input")]
        stream_json_input: bool,
        /// Select and run one named top-level flow from a `.flux` program. This is the authored,
        /// deterministic flow path (the model is used only by explicit AI operations in the flow),
        /// while `flux run <app.flux>` without this flag retains the multi-agent app behavior.
        #[arg(
            long,
            value_name = "FLOW",
            conflicts_with_all = ["stream_json", "stream_json_input"]
        )]
        entry: Option<String>,
        /// JSON object supplying the selected flow's declared parameters.
        #[arg(long, value_name = "JSON", requires = "entry")]
        inputs: Option<String>,
        /// Supply one selected-flow parameter as `NAME=VALUE` (repeatable; last value wins).
        #[arg(long = "arg", value_name = "NAME=VALUE", requires = "entry")]
        args: Vec<String>,
        /// The prompt words, or a path to an `<app.flux>` multi-agent program. Agent flags
        /// (`-m`, `--yes`, …) may appear before or after. With `--stream-json-input`, the prompt is
        /// optional — the first turn's input can come from stdin's first line instead.
        prompt: Vec<String>,
    },
    /// Launch the ratatui chat TUI (requires a real terminal). Tool calls raise a y/a/N modal; pass
    /// `--yes` to auto-approve all calls without a modal.
    Tui {
        #[command(flatten)]
        agent: AgentFlags,
        /// Attach the chat to the durable Fleet main coordinator. With no value, use the current
        /// directory; use `--fleet=ROOT` to select another Fleet explicitly. Ordinary `flux tui`
        /// remains a standalone chat and never changes stores based on directory detection.
        #[arg(
            long,
            value_name = "ROOT",
            num_args = 0..=1,
            default_missing_value = ".",
            require_equals = true
        )]
        fleet: Option<std::path::PathBuf>,

        /// Attach to an agent that lives on a served host: the WHOLE agent runs there — planning,
        /// model calls, tools, session and approvals — and this terminal is a window onto it.
        ///
        /// Takes a served agent's URL (`https://agent.internal:8787`) or the id of an
        /// `[[endpoint.static]]` binding declared with `protocol = "a2a"`, whose `credential_ref`
        /// supplies the bearer credential.
        ///
        /// ⚠ This is NOT `--remote`/`--host`. Those keep the agent here and land its *effects*
        /// elsewhere, so you still approve on this machine. With `--attach` the approval stage
        /// moved too, and the conversation lives in the remote's session store — it will not appear
        /// in `flux sessions` or `flux replay` here. They are refused together for that reason.
        #[arg(
            long,
            value_name = "URL|NAME",
            conflicts_with_all = ["remote", "host", "fleet"]
        )]
        attach: Option<String>,

        /// Name of the environment variable holding the attached agent's bearer token. The token is
        /// never accepted as a command-line value. Ignored when `--attach` names a binding that
        /// carries its own `credential_ref`.
        #[arg(
            long = "attach-token-env",
            value_name = "ENV",
            default_value = "FLUX_A2A_TOKEN",
            requires = "attach"
        )]
        attach_token_env: String,

        /// Continue an existing conversation on the attached agent instead of starting a new one.
        /// A served flux agent maps one context id to one session, so the same value reaches the
        /// same remote session — including from a different machine.
        #[arg(long = "attach-context", value_name = "ID", requires = "attach")]
        attach_context: Option<String>,
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
        after_help = "ADAPTERS:\n  synthetic       real-model coding riddles (fast, no Docker)\n  mock            offline CI fixture (drives -m mock)\n  terminal-bench  the real Docker benchmark\n  multi           several behind one combined score (with --members)\n\nEXAMPLES:\n  flux eval synthetic -m openrouter/anthropic/claude-sonnet-4.6 --watch --report r.md\n  flux eval multi --members synthetic,terminal-bench\n\nBENCHMARKING THE HARNESS:\n  The supported harness benchmark is flux-bench — https://github.com/codewandler/flux-bench\n  It runs the SHIPPED flux binary against a curated corpus with the model held fixed and\n  verified fixed, measures its own noise floor, and grades what an agent declines to do.\n  `flux eval` is unchanged and stays supported: it is the in-repo scoring engine the\n  self-improvement loop drives, and an offline CI fixture."
    )]
    Eval {
        /// Which suite to run.
        #[arg(value_enum)]
        adapter: EvalAdapter,
        /// Model the suite's agent runs (e.g. `-m mock`, `-m openrouter/anthropic/claude-sonnet-4.6`).
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
    /// Manage scoped planning/execution boards. JSON is the stable agent API.
    Board(BoardCommand),
    /// Coordinate durable local Flux sub-agents without implicitly publishing changes.
    Fleet(FleetCommand),
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
        /// Write to this path (workspace-confined, parents created) instead of stdout. A `.png`
        /// extension rasterizes to PNG with the embedded font; anything else writes the SVG
        /// text. Stdout is always SVG.
        #[arg(short = 'o', long, value_name = "OUT")]
        out: Option<String>,
    },
    /// Run the strict-review protocol over `--files` and print a `ReviewReport` (flux L-13; design
    /// `docs/designs/strict-review-flows.md`). Self-contained: the reviewer roles and the
    /// `strict_review` flow are embedded immutably in the binary, so this works in any repo without
    /// trusting project role definitions. Live progress is derived from the shared flow-run event
    /// sink and written to stderr; the final report alone is written to stdout.
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
        /// Live stderr progress: `auto` selects a transient tree on a terminal and plain summaries
        /// otherwise; `tree` and `plain` force those renderers; `off` stays silent until the report.
        #[arg(long, value_enum, default_value_t)]
        progress: ReviewProgress,
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
    /// List recent sessions (newest first). With `--query`/`--file`/`--since`/`--until`, narrows
    /// the listing to sessions matching ALL given filters instead of every session (C-164) — bare
    /// `flux sessions` is unchanged.
    Sessions {
        /// Delete all zero-message (abandoned) sessions.
        #[arg(long, conflicts_with_all = ["query", "file", "since", "until"])]
        prune: bool,
        /// Only sessions whose conversation contains this text (case-insensitive).
        #[arg(long)]
        query: Option<String>,
        /// Only sessions that touched this file path (matched at a path boundary, e.g. `main.rs`
        /// matches a recorded `/repo/src/main.rs`).
        #[arg(long)]
        file: Option<String>,
        /// Only sessions active at or after this bound: YYYY-MM-DD, RFC3339, or a duration like
        /// 24h/7d/2w.
        #[arg(long)]
        since: Option<String>,
        /// Only sessions active at or before this bound: YYYY-MM-DD or RFC3339. Date-only values
        /// mean next midnight.
        #[arg(long)]
        until: Option<String>,
    },
    /// Inspect or cancel agent-scheduled wake-ups (A-98): a turn that called `schedule_wakeup` to
    /// resume itself later. Defaults to `list` when no action is given.
    Wakeups {
        #[command(subcommand)]
        action: Option<WakeupAction>,
    },
    /// Per-model token usage + cost across flux and detected local agent harnesses.
    Usage(usage::UsageArgs),
    /// Derive today's work facts from the durable Flux session log and narrate them with one
    /// tool-free model call. Empty days make no provider call.
    Insights {
        /// Model used for the one summary call; defaults through normal Flux configuration.
        #[arg(short = 'm', long, value_name = "PROVIDER/MODEL")]
        model: Option<String>,
    },
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
    /// Record one live turn as a committed-safe scenario fixture (D-174): the run's events, flow
    /// state, redacted model cassette, and canonical plan snapshot land in
    /// `tests/scenarios/<name>/`. Replay it offline — for $0, with no key and no network — with
    /// `flux test`.
    Record {
        /// Fixture name; becomes the directory `<--dir>/<name>/`.
        name: String,
        /// The prompt to record, as words.
        #[arg(required = true)]
        prompt: Vec<String>,
        /// Where fixtures live (default `tests/scenarios`).
        #[arg(long, value_name = "DIR", default_value = "tests/scenarios")]
        dir: std::path::PathBuf,
        #[command(flatten)]
        agent: AgentFlags,
    },
    /// Replay recorded scenario fixtures offline as a test gate (D-174): the REAL agent re-runs
    /// against the recorded world under a deny-all approver and a never-called provider — $0, no
    /// key, no network. Exit code 1 if any fixture diverges from its recording, so it can be a CI
    /// gate; a regression prints the plan source and the world diff.
    Test {
        /// One fixture name. Omit to run every fixture under `--dir`.
        name: Option<String>,
        /// Where fixtures live (default `tests/scenarios`).
        #[arg(long, value_name = "DIR", default_value = "tests/scenarios")]
        dir: std::path::PathBuf,
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
    /// Render a recorded run as a single self-contained static HTML file (C-132) — a shareable
    /// artifact for bug reports, PR links, and demos: the plan tree (via the `flow_render`
    /// substrate), per-op results and diffs, cost, and a timeline, with sub-agent children (A-59)
    /// nested. Every piece of rendered text is redacted (C-22) before it reaches the page. The
    /// read-only sibling of `replay`/`fork`/`diff`: a pure read, no event-store write, no provider
    /// construction. Inline CSS, no JS, no network references — open the file directly in a browser.
    Export {
        /// Session id (`s_42`), or `last` for the most recent session.
        #[arg(default_value = "last")]
        run: String,
        /// Write the HTML to this path (workspace-confined, parents created). Without it, the HTML
        /// goes to stdout (the same convention as `flux render`).
        #[arg(short = 'o', long, value_name = "OUT")]
        out: Option<String>,
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
    /// Inspect and operate the named execution-substrate bindings (`[[host]]`). Operator-only,
    /// weak refs only — never prints a secret value.
    Host {
        #[command(subcommand)]
        action: HostAction,
    },
    /// Install and operate the separately released local Exchange authority.
    Exchange {
        #[command(subcommand)]
        action: ExchangeAction,
    },
    /// Create, grant and inspect labelled Exchange connections without receiving vendor secrets.
    Integration {
        #[command(subcommand)]
        action: IntegrationAction,
    },
    /// Work with the authorization policy — currently `simulate`, which replays a proposed policy
    /// against the recorded op history before you adopt it.
    Policy {
        #[command(subcommand)]
        action: PolicyAction,
    },
    /// Export versioned, machine-readable specifications for Flux's built-in catalogue.
    Catalog {
        #[command(subcommand)]
        action: CatalogAction,
    },
    /// Browse the operation catalog. `--explore` opens a full-screen search-first explorer over
    /// every registered operation — description, parameters, risk and documentation links.
    Ops {
        /// Open the full-screen explorer. Requires a real terminal on stdin and stdout.
        #[arg(long)]
        explore: bool,
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
    /// Serve this binary's embedded documentation and Flux-Lang workbench. Loopback listeners add
    /// guarded scratch execution and editor support; public listeners remain effect-free.
    Docs {
        /// Listener address. Non-loopback binds do not construct or mount execution/LSP services.
        #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:8788")]
        bind: std::net::SocketAddr,
        /// Model used lazily by model-backed runnable examples. Uses normal Flux configuration
        /// and defaults when omitted.
        #[arg(short = 'm', long, value_name = "PROVIDER/MODEL")]
        model: Option<String>,
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
    /// Diagnose a flux install end-to-end: provider credentials (incl. OAuth expiry), plugin-pack
    /// signature/hash drift, the OS sandbox backend, `events.db` health, private-network egress
    /// config sanity, `[tools] disable` resolution, and version skew vs the latest release — each
    /// with a one-line fix-it hint on any non-pass (C-128). Exit code is non-zero iff any check
    /// fails (a warning never fails the run).
    Doctor {
        /// Machine-readable JSON output for scripting.
        #[arg(long)]
        json: bool,
    },
    /// Inspect the layered prompt context Flux would assemble in this workspace.
    Context {
        #[command(subcommand)]
        action: ContextAction,
    },
    /// Serve or inspect the guarded execution-system transport.
    System {
        #[command(subcommand)]
        action: SystemAction,
    },
}

/// `flux exchange …` — host-owned entry points for the separately released Exchange process.
#[derive(clap::Subcommand, Debug)]
pub(super) enum ExchangeAction {
    /// Operate the verified local Exchange installation and its owned process.
    Local {
        #[command(subcommand)]
        action: ExchangeLocalAction,
    },
}

/// `flux exchange local …` — the closed lifecycle grammar owned by C-510.
///
/// These arguments deliberately contain no executable, URL, token, credential, port or process-id
/// override. The lifecycle implementation consumes only C-510's verified channel and authenticated
/// supervisor boundary once that provider contract is available.
#[derive(clap::Subcommand, Debug, PartialEq, Eq)]
pub(super) enum ExchangeLocalAction {
    /// Start the verified compatible local Exchange, or report its already-running state.
    Start {
        /// Emit one stable JSON result and never prompt.
        #[arg(long)]
        json: bool,
        /// Refuse any ceremony that would require an interactive prompt.
        #[arg(long)]
        no_prompt: bool,
    },
    /// Report the verified install and owned-process state without changing it.
    Status {
        /// Emit one stable JSON result and never prompt.
        #[arg(long)]
        json: bool,
    },
    /// Stop only the local Exchange instance owned by Flux's authenticated supervisor.
    Stop {
        /// Emit one stable JSON result and never prompt.
        #[arg(long)]
        json: bool,
        /// Refuse any ceremony that would require an interactive prompt.
        #[arg(long)]
        no_prompt: bool,
    },
}

/// One syntactically valid metadata-selector `KEY=VALUE` assignment.
///
/// Only Exchange may interpret this as a grant selector. Connection fields deliberately do not use
/// this parser until the provider-owned plan can prove that an identity or alias is non-secret.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct SelectorAssignment {
    pub(super) key: String,
    pub(super) value: String,
    valid: bool,
}

impl std::fmt::Debug for SelectorAssignment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SelectorAssignment")
            .field("key", &self.key)
            .field("value", &"[redacted]")
            .finish()
    }
}

impl std::str::FromStr for SelectorAssignment {
    type Err = String;

    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        let (key, value, valid) = match input.split_once('=') {
            Some((key, value)) if !key.is_empty() => (key, value, true),
            _ => ("", input, false),
        };
        Ok(Self {
            key: key.into(),
            value: value.into(),
            valid,
        })
    }
}

impl SelectorAssignment {
    pub(super) fn is_valid(&self) -> bool {
        self.valid
    }
}

/// `flux integration …` — labelled connection management over Exchange's published contracts.
#[derive(clap::Subcommand, Debug)]
pub(super) enum IntegrationAction {
    /// Connect one labelled connector from its provider-published connection plan.
    Connect {
        /// Connector identity understood by Exchange.
        connector: String,
        /// Tenant-scoped connection label (for example, `company` or `sandbox`).
        #[arg(long)]
        name: String,
        /// Emit one stable JSON result and never prompt.
        #[arg(long)]
        json: bool,
        /// Refuse if any required non-secret setting needs an interactive prompt.
        #[arg(long)]
        no_prompt: bool,
    },
    /// Preview or apply a metadata-selector grant for one labelled connection.
    Grant {
        /// Connector identity understood by Exchange.
        connector: String,
        /// Tenant-scoped connection label.
        #[arg(long)]
        name: String,
        /// Opaque metadata selector `KEY=VALUE` interpreted by Exchange (repeatable).
        #[arg(long = "selector", value_name = "KEY=VALUE", required = true)]
        selectors: Vec<SelectorAssignment>,
        /// Apply the previewed grant. Without this flag the command is preview-only.
        #[arg(long)]
        apply: bool,
        /// Emit one stable JSON result and never prompt.
        #[arg(long)]
        json: bool,
        /// Refuse if applying the grant would require an interactive prompt.
        #[arg(long)]
        no_prompt: bool,
    },
    /// List labelled connections and their effective-operation state.
    List {
        /// Emit one stable JSON result and never prompt.
        #[arg(long)]
        json: bool,
    },
    /// Diagnose local process, bootstrap, authentication, connection and grant state.
    Doctor {
        /// Emit one stable JSON result and never prompt.
        #[arg(long)]
        json: bool,
    },
}

/// `flux context …` — inspect prompt provenance without starting a model turn.
#[derive(clap::Subcommand, Debug)]
pub(super) enum ContextAction {
    /// Show the ordered context manifest. Bodies are omitted unless explicitly requested.
    Show {
        /// Show one layer instead of all of them: its manifest row and its body, without needing
        /// `--body`. Accepts the layer id (`git`) or an unambiguous prefix of one. `--json` carries
        /// every manifest field.
        #[arg(value_name = "LAYER")]
        layer: Option<String>,
        /// Agent behavior profile to include after the universal harness protocol.
        #[arg(long, value_enum, default_value_t)]
        profile: ContextProfile,
        /// Include conditional guidance for this visible operation (repeatable).
        #[arg(long = "tool", value_name = "OP")]
        tools: Vec<String>,
        /// Include prompt bodies. The default manifest contains only metadata and hashes.
        #[arg(long)]
        body: bool,
        /// Emit JSON instead of the compact table.
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ContextProfile {
    General,
    #[default]
    Coding,
}

impl From<ContextProfile> for flux_agent::AgentProfile {
    fn from(value: ContextProfile) -> Self {
        match value {
            ContextProfile::General => Self::General,
            ContextProfile::Coding => Self::Coding,
        }
    }
}

/// `flux system …` — explicit operator control of the remote execution substrate.
#[derive(clap::Subcommand, Debug)]
pub(super) enum SystemAction {
    /// Serve one canonical workspace over authenticated TLS.
    Serve {
        /// Listener address. Non-loopback binds are allowed only because every route is authenticated.
        #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:8790")]
        bind: std::net::SocketAddr,
        /// Canonical workspace made visible to remote sessions.
        #[arg(long, value_name = "DIR", default_value = ".")]
        workspace: std::path::PathBuf,
        /// PEM certificate chain served over TLS.
        #[arg(long, value_name = "PEM")]
        cert: std::path::PathBuf,
        /// PEM private key for `--cert`.
        #[arg(long, value_name = "PEM")]
        key: std::path::PathBuf,
        /// Environment variable containing the required bearer token.
        #[arg(
            long = "token-env",
            value_name = "ENV",
            default_value = "FLUX_REMOTE_SYSTEM_TOKEN"
        )]
        token_env: String,
    },
}

/// `flux catalog …`
#[derive(clap::Subcommand, Debug)]
pub(super) enum CatalogAction {
    /// Print the curated foundational operation, language-node, and capability catalogue.
    Core {
        /// Output encoding. JSON is the stable versioned interchange format.
        #[arg(long, value_enum, default_value_t = CatalogFormat::Json)]
        format: CatalogFormat,
    },
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, Default)]
pub(super) enum CatalogFormat {
    #[default]
    Json,
}

impl Commands {
    /// The flattened [`AgentFlags`] of an agent-path subcommand (`run`/`tui`/`fork`/
    /// `app run`), if this is one. `main` uses this to export the flags' env signals BEFORE the
    /// tokio runtime exists — `set_var` must not race worker-thread `getenv`s.
    pub(super) fn agent_flags(&self) -> Option<&AgentFlags> {
        match self {
            Self::Run { agent, .. }
            | Self::Tui { agent, .. }
            | Self::Fork { agent, .. }
            | Self::Record { agent, .. }
            | Self::App {
                action: AppAction::Run { agent, .. },
            } => Some(agent),
            _ => None,
        }
    }
}

/// `flux app …`
#[derive(clap::Subcommand, Debug)]
pub(super) enum AppAction {
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
        /// form needs an approval posture chosen: `--yes` or `--remote-approval`. Give `<program>`
        /// BEFORE a bare `--serve` (`flux app run prog.flux --serve`) or attach a custom address with
        /// `=` (`--serve=0.0.0.0:8787`) — `--serve <addr>` followed by a program path swallows the path
        /// as the address instead (clap's usual optional-value-flag ambiguity).
        #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "127.0.0.1:8787")]
        serve: Option<String>,
        /// Ask a human, over the network, before each guarded effect (C-453). Parks every call that
        /// needs approval and serves it at `GET /approvals`; answer with `POST /approvals/{id}`.
        /// An effect nobody answers within `FLUX_APPROVAL_TIMEOUT_SECS` (default 120) is DENIED.
        ///
        /// This is the posture with a human in it. The alternative is `--yes` — do not ask, and let
        /// authorization policy, the sandbox floor and resource budgets do the constraining, which
        /// is the right design for high-autonomy work. Pick one; they contradict each other. Remote
        /// approval supports the shared operator token (or open loopback) server modes; principal
        /// auth is refused until approvals have a distinct supervisor authorization model.
        #[arg(long, requires = "serve", conflicts_with = "program")]
        remote_approval: bool,
    },
}

/// `flux flow …`
#[derive(clap::Subcommand, Debug)]
pub(super) enum FlowAction {
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
        /// Silence the stderr progress surface: the session line, per-op dispatch/result lines,
        /// spinners, and the turn-end rule. Errors, warnings, approval prompts, sandbox
        /// disclosures, halt reports, and the flow result on stdout are unaffected. Also enabled
        /// by `FLUX_QUIET`. Complements `flux review --progress off`.
        #[arg(short = 'q', long)]
        quiet: bool,
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
pub(super) enum LoopAction {
    /// Print the built-in adaptive agent loop (the default).
    Show,
    /// Write the built-in loop to `.flux/agent-loop.flux` so it can be edited and selected explicitly.
    Eject {
        /// Overwrite an existing ejected copy.
        #[arg(short, long)]
        force: bool,
    },
}

/// `flux wakeups …` (A-98)
#[derive(clap::Subcommand, Debug)]
pub(super) enum WakeupAction {
    /// List pending wake-ups for a session (the default action).
    List {
        /// Session id (`s_42`), or `last` for the most recent session.
        #[arg(default_value = "last")]
        session: String,
    },
    /// Cancel a pending wake-up before it fires.
    Cancel {
        /// Session id (`s_42`), or `last` for the most recent session.
        session: String,
        /// The wake-up id (from `flux wakeups list`).
        wakeup_id: String,
    },
}

/// `flux policy …`
#[derive(clap::Subcommand, Debug)]
pub(super) enum PolicyAction {
    /// Replay a proposed authorization policy against the recorded op history (C-131): a diff-style
    /// report of which historical ops it would have newly blocked and newly allowed, relative to the
    /// policy in force right now. A pure read — nothing is written to the event store, no provider is
    /// constructed, and the proposal is never adopted.
    ///
    /// A recorded op the log cannot re-evaluate is reported as `indeterminate` with a reason, never
    /// folded into blocked or allowed.
    Simulate {
        /// The proposed policy: a flux configuration document (the shape of `.flux/config.toml`)
        /// whose `[policy]` grants are layered onto the built-in local floor, exactly as adopting
        /// the file would compose them.
        #[arg(value_name = "PROPOSED.TOML")]
        proposed: String,
        /// Replay only the N most recent sessions (0 = every recorded session).
        #[arg(long, default_value_t = 0)]
        sessions: usize,
        /// Emit the report as one JSON object for tooling instead of the human rendering.
        #[arg(long)]
        json: bool,
    },
}

/// `flux auth …`
#[derive(clap::Subcommand, Debug)]
pub(super) enum AuthAction {
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
pub(super) enum PluginAction {
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
    /// `install --dir [path]` is the second mode — the pre-D-47 local scan, registering every
    /// `flux-plugin-*` binary already built in a directory (default `plugins/target/release`) with
    /// no version/hash recorded.
    ///
    /// `install --git <url> [--tag <t> | --rev <r> | --branch <b>] [--bin <name>] [--force]` is the
    /// third source (D-87): clone the repo at the given ref into a cache
    /// (`~/.flux/plugins/src/<repo>/`), detect a `flux-plugin-*` crate, `cargo build --release
    /// --locked`, and register the built binary — the source-transparent (`cargo install --git`)
    /// alternative for a GitLab-hosted or third-party plugin the signed pack channel can't serve.
    /// Building unverified source is code execution, so the resolved commit is shown and an explicit
    /// confirm is required before the build (non-interactively: `FLUX_ALLOW_SOURCE_BUILD=1`); the
    /// descriptor is labelled `from-source (unverified)`. Re-installing the same resolved commit is
    /// an idempotent no-op; `--force` rebuilds.
    ///
    /// The three sources are exclusive; bare `install` with none of names/`--all`, `--dir`, or
    /// `--git` is an error naming all three.
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
        /// Build + install from this git URL (source mode, D-87): clone → detect a flux-plugin
        /// crate → `cargo build --release --locked` → register `from-source (unverified)`.
        #[arg(long, value_name = "URL", conflicts_with_all = ["names", "all", "dir"])]
        git: Option<String>,
        /// With `--git`: check out this tag before building (mutually exclusive with `--rev`/`--branch`).
        #[arg(long, value_name = "TAG", requires = "git", conflicts_with_all = ["rev", "branch"])]
        tag: Option<String>,
        /// With `--git`: check out this exact commit/rev before building.
        #[arg(long, value_name = "REV", requires = "git", conflicts_with_all = ["tag", "branch"])]
        rev: Option<String>,
        /// With `--git`: check out this branch head before building.
        #[arg(long, value_name = "BRANCH", requires = "git", conflicts_with_all = ["tag", "rev"])]
        branch: Option<String>,
        /// With `--git`: the `flux-plugin-*` bin target to build when the repo has several.
        #[arg(long, value_name = "BIN", requires = "git")]
        bin: Option<String>,
        /// With `--git`: rebuild even if the same resolved commit is already installed.
        #[arg(long, requires = "git")]
        force: bool,
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
    /// Re-fetch a plugin's manifest and re-project its operations: `refresh <name>` (C-310).
    ///
    /// A plugin answers `manifest` over its live connection, so a plugin fronting a remote
    /// deployment can advertise a different op set once the operator authenticates a provider
    /// there (`flux auth login <name>`). This runs that second fetch against the same open
    /// subprocess the load used and reports the catalog delta — which operations appeared, which
    /// were withdrawn — so the operator can see the new surface before opening a session on it.
    ///
    /// A refresh changes the **operation set, never the grant**: the refreshed manifest may add and
    /// remove operations freely, but the capabilities the operator granted at load stay in force in
    /// both directions — a manifest asking for more is refused, and one asking for less is not
    /// adopted (surrendering a capability in the declaration must not strip the authority an
    /// operation is gated by while the capability is still granted). An operation that keeps its
    /// name may not shed the scope it was gated under either. A refusal leaves the catalog exactly
    /// as it was — so this doubles as the drift check on a plugin whose manifest is not stable
    /// across two fetches.
    Refresh {
        /// Installed plugin name.
        name: String,
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
pub(super) enum EndpointAction {
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
        /// Wire-protocol hint (`postgres`, `http`, `mysql`, …).
        #[arg(long)]
        protocol: Option<String>,
        /// Credential *location*: `env/KEY`, `kubernetes/<ns>/<name>/<key>`, or
        /// `plugin/<p>/<i>/<slot>`. Omit for an unauthenticated endpoint. Never a value.
        #[arg(long, value_name = "REF")]
        credential_ref: Option<String>,
        /// Repeatable non-secret label `key=value` (region, tags) for display/filtering.
        #[arg(long = "label", value_name = "K=V")]
        labels: Vec<String>,
        /// The `[[host]]` binding this endpoint is reachable through (e.g. `k8s-dev`). Omit for an
        /// endpoint reachable from wherever the caller is. A binding that is not declared is
        /// refused here, not at dial time.
        #[arg(long, value_name = "NAME")]
        host: Option<String>,
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

/// `flux host …` — the operator mirror of the agent's `host.*` ops over the session's named
/// execution-substrate bindings (Decision 0018 / C-649). Every path is reference-only: it shows
/// the credential *location*, never a value.
#[derive(clap::Subcommand, Debug)]
pub(super) enum HostAction {
    /// List the session's host bindings: id, backend kind, address and availability.
    #[command(alias = "list")]
    Ls {
        /// Output encoding. JSON, not human prose, is the automation API.
        #[arg(long, value_enum, default_value_t)]
        output: AgentOutput,
    },
    /// Show one binding in full by name (still reference-only).
    Show {
        /// Binding name (e.g. `build-farm`).
        id: String,
        /// Output encoding. JSON, not human prose, is the automation API.
        #[arg(long, value_enum, default_value_t)]
        output: AgentOutput,
    },
    /// Declare a named binding: upsert one `[[host]]` entry in `~/.flux/config.toml`. The
    /// credential is a *location* (`--credential-ref`), never a value; the URL must be
    /// credential-free. The declarative alternative is a `[[host]]` block in either config layer.
    Add {
        /// The binding name (a bare name, e.g. `build-farm`).
        id: String,
        /// Backend kind: `local`, `sandboxed`, `container`, `kubernetes`, `microvm`, `ssh` or
        /// `remote`. An `ssh` binding's far-side contract (`[[host]].ssh`) is declarative only —
        /// declare the binding here, then add the sub-table if it needs more than the defaults.
        #[arg(long)]
        backend: String,
        /// Bare `scheme://host[:port]` for backends with an address — no embedded credentials.
        #[arg(long)]
        url: Option<String>,
        /// Credential *location*: `env/KEY`, `kubernetes/<ns>/<name>/<key>`, or
        /// `plugin/<p>/<i>/<slot>`. Omit for an unauthenticated binding. Never a value.
        #[arg(long, value_name = "REF")]
        credential_ref: Option<String>,
        /// Path to the PEM certificate of the private CA this binding's endpoint chains to — the
        /// binding-scoped equivalent of `--remote-ca`. Omit for ordinary public trust. An
        /// unreadable or malformed certificate refuses the binding; it never downgrades to the
        /// default trust store.
        #[arg(long = "ca-cert", value_name = "PEM")]
        ca_cert: Option<String>,
        /// Repeatable surface class granted to *select* this binding: `operator` (attended
        /// sessions, the default — your explicit add is the grant) and/or `unattended` (serving
        /// and `--yes` surfaces; grant deliberately, widening is an escalation). A `[[host]]`
        /// entry written by hand defaults to deny instead.
        #[arg(long = "grant", value_name = "CLASS", default_values_t = [String::from("operator")])]
        grant: Vec<String>,
        /// Repeatable non-secret label `key=value` (region, cluster, tags) for display/filtering.
        #[arg(long = "label", value_name = "K=V")]
        labels: Vec<String>,
    },
    /// Remove a binding declared in `~/.flux/config.toml` (a project-declared binding is reported,
    /// not removed — edit the project config where it lives).
    Rm {
        /// Binding name to remove.
        id: String,
    },
    /// Verify one binding by its backend's side-effect-free identity check: substrate identity
    /// (kind, workspace, confinement, remotely_reported) and, for a remote-shaped backend
    /// (`remote`, `microvm`), the negotiated protocol version. Executes nothing on the substrate.
    Probe {
        /// Binding name to probe.
        id: String,
        /// Output encoding. JSON, not human prose, is the automation API.
        #[arg(long, value_enum, default_value_t)]
        output: AgentOutput,
    },
    /// Read one binding's own condition: CPU, load, memory, swap, disk, uptime, temperature and
    /// fans, measured by that substrate about itself. A metric it cannot measure is reported as
    /// explicitly unavailable with a reason — never as zero — and a remote binding's readings are
    /// marked as remotely reported.
    Metrics {
        /// Binding name to measure.
        id: String,
        /// Output encoding. JSON, not human prose, is the automation API.
        #[arg(long, value_enum, default_value_t)]
        output: AgentOutput,
    },
}

/// `flux eval <adapter>` — the benchmark suites flux-eval can drive. A typo'd adapter is a parse
/// error listing these, instead of a deep `build_adapter` failure after startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(super) enum EvalAdapter {
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
    pub(super) fn as_str(self) -> &'static str {
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
pub(super) enum EffortArg {
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

pub(super) fn parse_effort(value: &str) -> Result<Effort> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Ok(Effort::Low),
        "medium" => Ok(Effort::Medium),
        "high" => Ok(Effort::High),
        "xhigh" => Ok(Effort::Xhigh),
        "max" => Ok(Effort::Max),
        _ => anyhow::bail!("expected low, medium, high, xhigh, or max; got {value:?}"),
    }
}

pub(super) fn parse_positive_usize(value: &str) -> std::result::Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("expected a positive integer: {error}"))?;
    if parsed == 0 {
        return Err("expected a positive integer greater than zero".into());
    }
    Ok(parsed)
}

pub(super) fn adaptive_stage_policy(
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

pub(super) fn adaptive_loop_policy(
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
        // Only an authored `ai_segment` may raise the retained-history ceiling; an ordinary adaptive
        // turn keeps the fixed default.
        max_history_bytes: None,
    })
}

pub(super) fn agent_max_iterations(
    flags: &AgentFlags,
    config: &flux_config::AgentConfig,
) -> Result<usize> {
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
pub(super) enum RenderView {
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
pub(super) enum ReviewFormat {
    /// A readable markdown findings summary (the default).
    #[default]
    Md,
    /// The raw `ReviewReport` JSON.
    Json,
}

/// `flux review --progress` rendering policy. Progress always uses stderr.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub(super) enum ReviewProgress {
    /// Tree on an interactive stderr, append-only summaries otherwise.
    #[default]
    Auto,
    /// Force the transient tree renderer.
    Tree,
    /// Force append-only status summaries (no cursor controls).
    Plain,
    /// Disable live progress.
    Off,
}

/// `flux review --fail-on` severity threshold, ordered low → high so `>=` comparisons are meaningful.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub(super) enum ReviewSeverity {
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
    pub(super) fn from_finding_str(s: &str) -> Self {
        match s {
            "info" => Self::Info,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            _ => Self::Critical,
        }
    }
}

#[cfg(test)]
mod c509_cli_grammar_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn local_exchange_lifecycle_has_a_closed_json_capable_grammar() {
        for (verb, expected) in [
            (
                "start",
                ExchangeLocalAction::Start {
                    json: true,
                    no_prompt: false,
                },
            ),
            ("status", ExchangeLocalAction::Status { json: true }),
            (
                "stop",
                ExchangeLocalAction::Stop {
                    json: true,
                    no_prompt: false,
                },
            ),
        ] {
            let cli = Cli::try_parse_from(["flux", "exchange", "local", verb, "--json"])
                .unwrap_or_else(|error| panic!("{verb} must parse: {error}"));
            let Some(Commands::Exchange {
                action: ExchangeAction::Local { action },
            }) = cli.command
            else {
                panic!("expected exchange local {verb}");
            };
            assert_eq!(action, expected);
        }

        assert!(Cli::try_parse_from(["flux", "exchange", "local", "run"]).is_err());
        assert!(Cli::try_parse_from([
            "flux",
            "exchange",
            "local",
            "start",
            "--executable",
            "/tmp/flux-exchange",
        ])
        .is_err());
    }

    #[test]
    fn integration_connect_withholds_fields_until_a_plan_can_classify_them() {
        let cli = Cli::try_parse_from([
            "flux",
            "integration",
            "connect",
            "custom-connector",
            "--name",
            "company",
            "--json",
            "--no-prompt",
        ])
        .expect("generic labelled connection grammar must parse");

        let Some(Commands::Integration {
            action:
                IntegrationAction::Connect {
                    connector,
                    name,
                    json,
                    no_prompt,
                },
        }) = cli.command
        else {
            panic!("expected integration connect");
        };
        assert_eq!(connector, "custom-connector");
        assert_eq!(name, "company");
        assert!(json);
        assert!(no_prompt);

        for credential_flag in ["--token", "--password", "--secret", "--credential"] {
            assert!(
                Cli::try_parse_from([
                    "flux",
                    "integration",
                    "connect",
                    "custom-connector",
                    "--name",
                    "company",
                    credential_flag,
                    "not-accepted",
                ])
                .is_err(),
                "{credential_flag} must never enter Flux's CLI grammar"
            );
        }
        assert!(Cli::try_parse_from([
            "flux",
            "integration",
            "connect",
            "custom-connector",
            "--name",
            "company",
            "--endpoint",
            "https://code.example.test",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "flux",
            "integration",
            "connect",
            "custom-connector",
            "--name",
            "company",
            "--field",
            "origin=https://code.example.test",
        ])
        .is_err());
    }

    #[test]
    fn integration_management_grammar_is_metadata_selector_based() {
        let grant = Cli::try_parse_from([
            "flux",
            "integration",
            "grant",
            "custom-connector",
            "--name",
            "company",
            "--selector",
            "custom-key=custom-value",
            "--selector",
            "another-key=another-value",
            "--apply",
            "--json",
            "--no-prompt",
        ])
        .expect("metadata grant grammar must parse");
        let Some(Commands::Integration {
            action:
                IntegrationAction::Grant {
                    connector,
                    name,
                    selectors,
                    apply,
                    json,
                    no_prompt,
                },
        }) = grant.command
        else {
            panic!("expected integration grant");
        };
        assert_eq!(connector, "custom-connector");
        assert_eq!(name, "company");
        assert_eq!(selectors.len(), 2);
        let debug = format!("{selectors:?}");
        assert!(!debug.contains("custom-value"));
        assert!(!debug.contains("another-value"));
        assert!(apply);
        assert!(json);
        assert!(no_prompt);

        for verb in ["list", "doctor"] {
            let cli = Cli::try_parse_from(["flux", "integration", verb, "--json"])
                .unwrap_or_else(|error| panic!("{verb} must parse: {error}"));
            assert!(matches!(cli.command, Some(Commands::Integration { .. })));
        }

        assert!(Cli::try_parse_from([
            "flux",
            "integration",
            "grant",
            "custom-connector",
            "--name",
            "company",
            "--operation",
            "tickets.delete",
        ])
        .is_err());
    }
}

#[cfg(test)]
mod fleet_tui_cli_tests {
    use super::*;
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn fleet_attachment_is_explicit_and_defaults_to_the_current_root() {
        let attached = Cli::try_parse_from(["flux", "tui", "--fleet"])
            .expect("an explicit current-root Fleet attachment must parse");
        let Some(Commands::Tui { fleet, .. }) = attached.command else {
            panic!("tui command expected")
        };
        assert_eq!(fleet, Some(PathBuf::from(".")));

        let selected = Cli::try_parse_from(["flux", "tui", "--fleet=/workspace/program"])
            .expect("an explicit Fleet root must parse");
        let Some(Commands::Tui { fleet, .. }) = selected.command else {
            panic!("tui command expected")
        };
        assert_eq!(fleet, Some(PathBuf::from("/workspace/program")));

        let standalone = Cli::try_parse_from(["flux", "tui"])
            .expect("ordinary standalone TUI must keep parsing");
        let Some(Commands::Tui { fleet, .. }) = standalone.command else {
            panic!("tui command expected")
        };
        assert_eq!(fleet, None);
    }
}

/// C-463 — the autonomy posture is one named choice on the CLI too, and `--yes` is a spelling of
/// one of them rather than a fourth setting beside them.
#[cfg(test)]
mod c463_autonomy_posture_tests {
    use super::*;
    use clap::Parser;
    use flux_runtime::{ApprovalStance, AutonomyPosture};
    use flux_system::sandbox::SandboxMode;

    fn flags(argv: &[&str]) -> AgentFlags {
        let mut args = vec!["flux"];
        args.extend_from_slice(argv);
        AgentFlagsOnly::parse_from(&args).agent
    }

    /// ⚠ **No flag day.** `--yes` still works and still means what it meant; it is now the older
    /// spelling of a posture whose name states the rest of what it always implied.
    #[test]
    fn yes_is_the_older_spelling_of_bounded_autonomy() {
        let posture = flags(&["--yes"]).autonomy_posture().unwrap();
        assert_eq!(posture, AutonomyPosture::BoundedAutonomy);
        assert_eq!(
            posture,
            flags(&["--posture", "bounded-autonomy"])
                .autonomy_posture()
                .unwrap(),
            "the two spellings must resolve to the same posture, or `--yes` has quietly become a \
             fourth setting beside `--posture`"
        );
    }

    /// The acceptance claim, at the surface an operator actually types: **one** value decides the
    /// approver, the confinement floor and the budget. Nothing here can select the first without
    /// also selecting the other two, which is the whole bug C-444 found from the SDK side.
    #[test]
    fn one_named_posture_selects_approver_confinement_and_budget_together() {
        for (argv, expected) in [
            (
                &["--posture", "supervised"][..],
                AutonomyPosture::Supervised,
            ),
            (&["--yes"][..], AutonomyPosture::BoundedAutonomy),
            (
                &["--posture", "exploratory"][..],
                AutonomyPosture::Exploratory,
            ),
            (&["--posture", "refusing"][..], AutonomyPosture::Refusing),
        ] {
            let posture = flags(argv).autonomy_posture().unwrap();
            assert_eq!(posture, expected, "{argv:?}");
            if posture.approval() == ApprovalStance::None {
                assert_eq!(
                    posture.sandbox_floor().mode,
                    SandboxMode::Require,
                    "{argv:?}: chose not to prompt without choosing confinement"
                );
                assert!(
                    !posture.budget().is_unbounded(),
                    "{argv:?}: chose not to prompt without choosing a ceiling"
                );
            }
        }
    }

    /// The CLI default is a human at a terminal — stated, rather than being whatever is left when
    /// no flag is passed.
    #[test]
    fn the_default_posture_is_supervised() {
        assert_eq!(
            flags(&[]).autonomy_posture().unwrap(),
            AutonomyPosture::Supervised
        );
    }

    /// Two opposite instructions are refused, not silently resolved: a run whose command line and
    /// whose behaviour disagree about whether anyone is being asked is the accident this prevents.
    #[test]
    fn contradictory_flags_are_refused_rather_than_resolved() {
        for argv in [
            &["--yes", "--posture", "supervised"][..],
            &["--yes", "--posture", "refusing"][..],
        ] {
            let err = flags(argv)
                .autonomy_posture()
                .expect_err("{argv:?} must be refused")
                .to_string();
            assert!(
                err.contains("opposite approval postures"),
                "{argv:?}: {err}"
            );
        }
        // Agreeing spellings are not a contradiction: both say "do not ask".
        assert!(flags(&["--yes", "--posture", "exploratory"])
            .autonomy_posture()
            .is_ok());
    }

    /// A surface with no terminal cannot honour `supervised`. It refuses rather than downgrading —
    /// a stated posture the surface silently replaces is exactly the class of accident the named
    /// postures remove — and its unstated default stays `refusing`, which is what these surfaces
    /// have always installed.
    #[test]
    fn a_headless_surface_refuses_supervised_and_defaults_to_refusing() {
        assert_eq!(
            flags(&[]).headless_posture("`flux record`").unwrap(),
            AutonomyPosture::Refusing
        );
        assert_eq!(
            flags(&["--yes"]).headless_posture("`flux record`").unwrap(),
            AutonomyPosture::BoundedAutonomy
        );
        let err = flags(&["--posture", "supervised"])
            .headless_posture("`flux record`")
            .expect_err("an explicit supervised posture must be refused, not downgraded")
            .to_string();
        assert!(err.contains("no terminal to ask at"), "{err}");
    }

    /// Only an explicitly named posture contributes a sandbox floor of its own. `--yes` keeps
    /// contributing exactly what it contributed before, through the surface classifier — otherwise
    /// the surfaces that classifier deliberately exempts (the TUI, where an operator is watching
    /// the whole run) would silently start being confined.
    #[test]
    fn only_an_explicit_posture_contributes_a_sandbox_floor() {
        assert_eq!(flags(&["--yes"]).named_posture(), None);
        assert_eq!(
            flags(&["--posture", "exploratory"]).named_posture(),
            Some(AutonomyPosture::Exploratory)
        );
    }

    /// A typo is refused with the four names, never resolved to the nearest one.
    #[test]
    fn an_unknown_posture_name_is_refused_at_parse_time() {
        let err = Cli::try_parse_from(["flux", "run", "--posture", "yolo", "hi"])
            .expect_err("an unknown posture must not parse")
            .to_string();
        assert!(err.contains("bounded-autonomy"), "{err}");
    }
}
