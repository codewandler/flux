use super::*;

/// A coarse "… ago" string from a millisecond epoch timestamp (for session listings).
pub(super) fn fmt_age(created_at_ms: i64) -> String {
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
/// `flux sessions --query/--file/--since/--until` (C-164) — narrow the listing to sessions
/// matching every given filter, via [`flux_events::EventStore::search`] (a projection over the
/// existing event log; see that method's docs for the TUI-picker seam). No filter given behaves
/// exactly as before.
pub(super) fn run_sessions(
    prune: bool,
    query: Option<String>,
    file: Option<String>,
    since: Option<String>,
    until: Option<String>,
) -> Result<()> {
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

    let mut filter = flux_events::SessionFilter::new();
    if let Some(q) = query {
        filter = filter.with_query(q);
    }
    if let Some(f) = file {
        filter = filter.with_file(f);
    }
    if let Some(s) = since {
        filter = filter.with_since_ms(usage::parse_since_ms(&s, usage::now_ms())?);
    }
    if let Some(u) = until {
        filter = filter.with_until_ms(usage::parse_until_ms(&u)?);
    }
    if let (Some(since), Some(until)) = (filter.since_ms, filter.until_ms) {
        if since >= until {
            bail!("--since must be before --until");
        }
    }
    let filtered = !filter.is_empty();

    let sessions = store.search(&filter, 30)?;
    if sessions.is_empty() {
        if filtered {
            eprintln!("no sessions match the given filter(s)");
        } else {
            eprintln!("no sessions yet — start one with `flux` or `flux run`");
        }
        return Ok(());
    }
    let mut interrupted = 0usize;
    for s in &sessions {
        let active_ts = if s.updated_at_ms > s.created_at_ms {
            format!("active {}", fmt_age(s.updated_at_ms))
        } else {
            fmt_age(s.created_at_ms)
        };
        // D-179: flag a session a crash killed mid-turn. Listing REPORTS; it never resurrects —
        // finishing a turn (which runs the live tail through the approval envelope) must not be a
        // side effect of asking what sessions exist. The next turn on that session does it.
        let crashed = matches!(
            flux_flow::resurrect::interrupted(&store, &s.id),
            Ok(Some(_))
        );
        if crashed {
            interrupted += 1;
        }
        println!(
            "{}  {:>3} msg  {:<22} {}{}",
            s.id,
            s.messages,
            s.model,
            active_ts,
            if crashed {
                style::red("  ⚠ interrupted")
            } else {
                String::new()
            }
        );
    }
    if interrupted > 0 {
        eprintln!(
            "{}",
            style::dim(&format!(
                "{interrupted} interrupted session(s) — the next turn entered on one (`flux run`, \
                 the REPL, or `flux tui`) finishes its killed turn from the crash point (no model \
                 call). `FLUX_AUTO_RESURRECT=0` disables that."
            ))
        );
    }
    Ok(())
}

/// `flux usage` — per-model tokens + cost for the current/last session, and an all-sessions total.
/// Reads the unified event store's `cost_summary` projection (C-06); pricing is the builtin table
/// overlaid by `~/.flux/pricing.toml` (same loader the live turn-end annotation uses).
pub(super) fn run_usage(args: usage::UsageArgs) -> Result<()> {
    let pricing = flux_credentials::load_pricing_table();
    usage::run_usage(args, &pricing)
}

/// The store-parameterized body of [`run_usage`] (tests pass an in-memory store so they don't touch
/// `HOME`'s real `~/.flux/events.db`).
#[cfg(test)]
pub(super) fn run_usage_with(store: &EventStore, pricing: &flux_core::PricingTable) -> Result<()> {
    usage::run_usage_with(store, pricing)
}

/// A-45: `flux replay <SESSION|last>` — hermetic offline re-execution of a recorded session.
/// Plans re-parse from the durable `plan_source`, op outputs are served from the C-43 cassette;
/// the lazy provider is never constructed (no model op is ever reached), and no live IO or side
/// effect can fire (a served dispatch never touches the executor). Non-zero exit on divergence,
/// so a recording can be pinned in CI.
pub(super) async fn run_replay(
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
pub(super) async fn run_fork(
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

    // Establish that the parent is forkable BEFORE minting anything (C-211). The tail runs live, so
    // a parent history that no provider would accept — one ending mid-tool-pair, say — is refused
    // here rather than 400ing on the fork's first turn; checking before the child exists is what
    // keeps a refused fork from leaving an empty orphan session behind.
    let history = flux_events::ValidHistory::new(
        events
            .conversation(&sid)
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    )
    .with_context(|| format!("session `{sid}` cannot be forked"))?;
    // Mint the fork session, correlated to its source (the A-08 linkage `flux replay
    // --sub-agents` and cost rollups already understand), and seed its conversation with the
    // parent's messages so an adaptive tail has the recorded context — one checked rewrite (A-102).
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
    flux_events::SessionLog::open(&events, &fork_sid)
        .and_then(|mut log| log.rewrite(history))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Session boards are app-defined facts in the same stream. A fork inherits their recorded
    // prefix just like conversation state, then child mutations append only to the child stream.
    for event in events
        .load_by_kind(&sid, "custom")
        .map_err(|e| anyhow::anyhow!("{e}"))?
    {
        if matches!(
            &event.kind,
            flux_events::EventKind::Custom { name, .. } if name.starts_with("board.session.")
        ) {
            events
                .append(&fork_sid, flux_events::NewEvent::new(event.kind))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
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
        let ast: flux_flow::ast::DraftAst = match flux_lang::program::Module::parse_str(&src)
            .map_err(|e| anyhow::anyhow!("parse {file} as Flux-Lang text: {e}"))?
        {
            flux_lang::program::Module::Flow(ast) => ast,
            flux_lang::program::Module::Program(_) => {
                bail!("--edit needs a bare flow, not a multi-agent program")
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
pub(super) fn run_diff_cmd(a_arg: &str, b_arg: &str, json: bool) -> Result<()> {
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
pub(super) async fn run_loop_cmd(action: Option<LoopAction>) -> Result<()> {
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

/// A minimal `reedline` prompt: a single `› ` indicator (no left/right segments).
pub(super) struct FluxPrompt;

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
pub(super) fn repl_history_path() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("HOME")?).join(".flux");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("history.txt"))
}

/// Interactive agentic REPL (tools enabled), with slash commands.
/// Per-turn cost wiring for every REPL/CLI sink (C-30): one pricing table loaded per command; the
/// model spec is derived from the LIVE engine at each sink construction — the same derivation
/// `loop_host` uses to key stored usage (C-15) — so what the turn line prices and what
/// `flux usage` attributes can never diverge, and a `/model` switch is picked up with zero extra
/// plumbing (the switch arm updates `agent.provider`/`agent.model`, which is all we read).
pub(super) struct TurnCost {
    pub(super) pricing: flux_core::PricingTable,
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

/// Built-in REPL slash commands (D-186): a file command sharing one of these names is dropped at
/// load (with a warning) rather than shadowing it — see [`load_command_files`].
const REPL_BUILTIN_COMMANDS: &[&str] = &[
    "exit",
    "quit",
    "help",
    "shell",
    "plugin-refresh",
    "model",
    "effort",
    "pd",
    "goal",
    "loop",
    "tools",
    "evidence",
    "session",
    "sessions",
    "resume",
    "compact",
    "insights",
    "clear",
];

/// The terminal line `/compact` prints after the engine has made an outcome observable. Only the
/// variant that carries real before/after counts is allowed to claim that compaction happened.
pub(super) fn compact_repl_message(outcome: flux_flow::engine::CompactionOutcome) -> String {
    use flux_flow::engine::CompactionOutcome;

    match outcome {
        CompactionOutcome::Disabled => "context compaction is disabled".into(),
        CompactionOutcome::Unchanged => "context unchanged".into(),
        CompactionOutcome::Cancelled => "compaction cancelled".into(),
        CompactionOutcome::Compacted {
            from_messages,
            to_messages,
        } => format!("context compacted ({from_messages} → {to_messages} messages)"),
    }
}

pub(super) async fn run_repl(flags: AgentFlags) -> Result<()> {
    // Decorative boot splash, before any other output. Blocks the runtime thread for a
    // few seconds at most — nothing else is in flight this early in the REPL.
    crate::splash::maybe_splash();
    let (mut agent, mut session_id, _spec, spawner, live_plugins) =
        build_agent_interactive(&flags).await?;
    let cost = TurnCost::load();
    let initial_rules = agent.executor.allow_rules();
    // Command files (D-186): discovered once at REPL start, not gated behind a flag like skills —
    // `/help` and dispatch below need the full set up front.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let command_files = load_command_files(&cwd, REPL_BUILTIN_COMMANDS);
    eprintln!(
        "{}",
        style::dim(&format!(
            "flux · {} · session {session_id} — /help, Ctrl-C interrupts a turn, Ctrl-D exits",
            agent.model
        ))
    );
    // D-183: the REPL is a turn-entry point too — finish an interrupted turn from a prior crash
    // BEFORE the first input runs, same step and same loud reporting as one-shot `flux run`.
    {
        let mut sink = cost.sink(&agent, 0);
        resurrect_on_open(&agent, &session_id, &mut sink).await;
    }

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
                        (
                            "/plugin-refresh <name>",
                            "refresh a loaded plugin for the next turn",
                        ),
                        ("/tools", "list available tools"),
                        (
                            "/evidence",
                            "show the audit trail this session has recorded",
                        ),
                        (
                            "/model <spec>",
                            "switch model (e.g. opus, sonnet, openai/gpt-4o)",
                        ),
                        (
                            "/effort [level]",
                            "show or set reasoning effort (low/medium/high/xhigh/max, or off)",
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
                        (
                            "/insights [direction]",
                            "derive current-session facts and narrate them once",
                        ),
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
                    if !command_files.is_empty() {
                        eprintln!("command files (.flux/commands, .claude/commands):");
                        for c in &command_files {
                            let hint = if c.argument_hint.is_empty() {
                                String::new()
                            } else {
                                format!(" {}", c.argument_hint)
                            };
                            eprintln!("  {:<24} {}", format!("/{}{hint}", c.name), c.description);
                        }
                    }
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
                "plugin-refresh" => {
                    let name = rest.strip_prefix("plugin-refresh").unwrap_or("").trim();
                    if name.is_empty() {
                        eprintln!("usage: /plugin-refresh <name>");
                    } else {
                        let catalog = agent.executor.live_catalog();
                        match live_plugins.refresh(name, &catalog).await {
                            Ok(refresh) => eprintln!(
                                "{}",
                                style::dim(&format!(
                                    "plugin `{name}` refreshed for the next turn · added [{}] · removed [{}] · retained {}",
                                    refresh.added.join(", "),
                                    refresh.removed.join(", "),
                                    refresh.retained.len(),
                                ))
                            ),
                            Err(error) => eprintln!("{} {error:#}", style::red("error:")),
                        }
                    }
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
                "effort" => {
                    let level = rest.strip_prefix("effort").unwrap_or("").trim();
                    if level.is_empty() {
                        let current = agent
                            .effort
                            .map(|e| e.as_str())
                            .unwrap_or("(provider default)");
                        eprintln!(
                            "effort: {current} · usage: /effort <low|medium|high|xhigh|max|off>"
                        );
                    } else if matches!(
                        level.to_ascii_lowercase().as_str(),
                        "off" | "none" | "default"
                    ) {
                        agent.set_effort(None);
                        eprintln!("effort: (provider default) — takes effect from the next turn");
                    } else {
                        match parse_effort(level) {
                            Ok(effort) => {
                                agent.set_effort(Some(effort));
                                eprintln!(
                                    "effort: {} — takes effect from the next turn (ignored by models without effort control)",
                                    effort.as_str()
                                );
                            }
                            Err(error) => eprintln!("cannot set effort: {error}"),
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
                    let registry = agent.executor.active_registry_snapshot();
                    let mut names = registry.names();
                    names.sort();
                    // C-162: `[tools] disable` ops stay registered (dispatch still refuses them),
                    // so mark them here rather than hiding them — a mysteriously-missing op is one
                    // command from an explanation instead of a silent gap in this listing.
                    let disabled = agent.executor.disabled_ops_for(&registry);
                    let rendered: Vec<String> = names
                        .into_iter()
                        .map(|name| {
                            if disabled.contains(&name) {
                                format!("{name} (disabled by config)")
                            } else {
                                name
                            }
                        })
                        .collect();
                    eprintln!("tools: {}", rendered.join(", "));
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
                                // D-183: `/resume` opens a (possibly different) session — finish
                                // an interrupted turn on it before the next input runs, same as
                                // the REPL's own startup and one-shot `flux run`.
                                let mut sink = cost.sink(&agent, 0);
                                resurrect_on_open(&agent, &session_id, &mut sink).await;
                            }
                            Err(e) => eprintln!("cannot resume `{id}`: {e}"),
                        }
                    }
                }
                "compact" => {
                    eprintln!("{}", style::dim("checking context for compaction…"));
                    let cancel = tokio_util::sync::CancellationToken::new();
                    let mut sink = cost.sink(&agent, 0);
                    match agent.maybe_compact(&session_id, &mut sink, &cancel).await {
                        Ok(outcome) => {
                            eprintln!("{}", style::dim(&compact_repl_message(outcome)))
                        }
                        Err(e) => eprintln!("{} {e}", style::red("compact error:")),
                    }
                }
                "insights" => {
                    let direction = rest
                        .strip_prefix("insights")
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    run_interruptible(|cancel| {
                        run_repl_insights(
                            &agent,
                            &session_id,
                            (!direction.is_empty()).then_some(direction.as_str()),
                            cancel,
                        )
                    })
                    .await;
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
                other => match command_files.iter().find(|c| c.name == other) {
                    Some(cmd) => {
                        // D-186: the substituted body enters the turn exactly like typed input.
                        let raw_args = rest.strip_prefix(other).unwrap_or("").trim();
                        let prompt =
                            flux_runtime::metadata::expand_command_arguments(&cmd.body, raw_args);
                        let agent_ref = &agent;
                        let cost_ref = &cost;
                        let sid_ref = session_id.as_str();
                        run_interruptible(move |c| async move {
                            let mut sink = cost_ref.sink(agent_ref, agent_ref.max_iterations);
                            if let Err(e) = agent_ref
                                .run_turn_cancellable(sid_ref, &prompt, &mut sink, &c)
                                .await
                            {
                                eprintln!("{} {e:#}", style::red("error:"));
                            }
                        })
                        .await;
                    }
                    None => eprintln!("unknown command /{other} (try /help)"),
                },
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

async fn run_repl_insights(
    agent: &FlowEngine,
    session_id: &str,
    direction: Option<&str>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let redactor = agent.executor.context().redactor.clone();
    let pricing = flux_credentials::load_pricing_table();
    let facts = match flux_flow::insights::collect_facts(
        &agent.events,
        &flux_flow::insights::InsightScope::Session {
            root: session_id.to_string(),
            label: format!("current session · {session_id}"),
        },
        &pricing,
        &redactor,
    ) {
        Ok(facts) => facts,
        Err(error) => {
            eprintln!("{} {error}", style::red("insights error:"));
            return;
        }
    };
    println!("{}", facts.render());
    if facts.is_empty() {
        return;
    }
    let (summary, usage) = flux_flow::insights::narrate(
        agent.provider.as_ref(),
        &agent.model,
        &facts,
        direction,
        &redactor,
        &cancel,
    )
    .await;
    let model = flux_core::canonical_model_spec(Some(agent.provider.name()), &agent.model);
    if let Err(error) = agent
        .events
        .record_unscoped_call_usage(session_id, &model, usage)
    {
        eprintln!("{} {error}", style::red("insights accounting error:"));
        return;
    }
    match summary {
        Ok(summary) => println!("\nSummary\n{summary}"),
        Err(error) => eprintln!("{} {error}", style::red("insights error:")),
    }
}

/// Run `make(cancel)` to completion, but cancel it on Ctrl-C (the token's clones are linked, so
/// cancelling here aborts the in-flight work). Used to wrap turns and autopilot loops in the REPL.
pub(super) async fn run_interruptible<F, Fut>(make: F)
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
pub(super) async fn run_goal(
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
pub(super) async fn run_loop(
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
pub(super) fn parse_loop_args(args: &str) -> (usize, String) {
    let mut it = args.splitn(2, char::is_whitespace);
    let first = it.next().unwrap_or("");
    if let Ok(n) = first.parse::<usize>() {
        (n.max(1), it.next().unwrap_or("").trim().to_string())
    } else {
        (1, args.trim().to_string())
    }
}

/// Interactive approval prompt for tool calls not covered by a rule.
pub(super) struct StdinApprover;

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
                .map(|s| style::yellow(&trim_subject(s)))
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

    /// The whole-plan confirm. `always` here trusts every plan for the rest of the session.
    async fn request_plan(&self, plan: &flux_runtime::PlanApprovalRequest) -> ApprovalChoice {
        read_choice(
            plan_prompt(plan),
            ApprovalChoice::AllowAlways("*plans*".to_string()),
        )
        .await
    }
}

/// Trim a path-like subject to its last two components so long absolute paths don't swamp a prompt.
pub(super) fn trim_subject(s: &str) -> String {
    std::path::Path::new(s)
        .components()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<std::path::PathBuf>()
        .display()
        .to_string()
}

/// The whole-plan confirm prompt. The plain CLI renders no plan tree before the confirm (the batch
/// observations are deliberately unrendered — see `CliSink::observation`), so this prompt is the one
/// place the user sees WHAT they are approving: the op names, the concrete resources and commands
/// statically visible at approval time, and the destructive flag. Ends with the answer line and no
/// trailing newline — the cursor waits on it.
pub(super) fn plan_prompt(plan: &flux_runtime::PlanApprovalRequest) -> String {
    let mut out = format!("\n{} this plan? ({})", style::yellow("run"), plan.subject());
    if plan.destructive {
        out.push_str(&format!(
            "\n  {}",
            style::yellow("⚠ contains a destructive operation")
        ));
    }
    if !plan.ops.is_empty() {
        let ops: Vec<String> = plan.ops.iter().map(|o| style::bold(o)).collect();
        out.push_str(&format!("\n  ops: {}", ops.join(", ")));
    }
    // Concrete targets: typed authority requirements (paths / named resources) plus process
    // commands from the statically-visible intents. Only literal args are known at approval time —
    // dispatch re-derives and enforces the real set, and an undisclosed destructive op still
    // re-fires the per-op gate inside the approved scope.
    let mut lines: Vec<String> = Vec::new();
    for req in &plan.requirements {
        // Operation requirements only repeat the ops line above.
        if req.resource.kind == flux_policy::ResourceKind::Operation {
            continue;
        }
        let subject = req
            .resource
            .path
            .as_deref()
            .or(req.resource.name.as_deref())
            .unwrap_or(&req.resource.id);
        if subject == "*" {
            continue;
        }
        lines.push(format!(
            "{} → {}",
            req.action.0,
            style::yellow(&trim_subject(subject))
        ));
    }
    for intent in &plan.intents.intents {
        lines.push(style::yellow(&truncate(&intent.approval_subject(), 80)));
    }
    let mut seen = std::collections::HashSet::new();
    lines.retain(|l| seen.insert(l.clone()));
    const MAX_LINES: usize = 8;
    let extra = lines.len().saturating_sub(MAX_LINES);
    for line in lines.iter().take(MAX_LINES) {
        out.push_str(&format!("\n  {line}"));
    }
    if extra > 0 {
        out.push_str(&format!("\n  {}", style::dim(&format!("+{extra} more"))));
    }
    out.push_str("\n[y]es / [a]lways / [N]o: ");
    out
}

/// Print `prompt`, then read a y/a/N answer **off the async runtime** so the turn's future YIELDS while
/// waiting — a blocking read inside the poll would freeze the task and make Ctrl-C inert. On a terminal
/// we read a single keypress via crossterm in raw mode: the keystroke is consumed cleanly (no leaked
/// line-reader that would fight reedline for stdin on the next prompt), and Ctrl-C / Ctrl-D / `n` / Esc
/// all decline. Off a terminal (pipes, eval) we read a line — EOF ends it and there's no prompt to
/// corrupt. `always` is returned for `a`/`always`.
pub(super) async fn read_choice(prompt: String, always: ApprovalChoice) -> ApprovalChoice {
    // Own the stderr line for the whole prompt: clears a live spinner line once and blocks every
    // repaint/clear until the answer is read — including sink events (`planning(false)`, spinner
    // ticks) drained while the turn future waits here. Without this the prompt is erased within
    // one 80 ms tick and the user sees only a spinner that looks hung.
    let _line = PromptGate::global().acquire().await;
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    if !std::io::stdin().is_terminal() {
        let choice = match read_stdin_line().await {
            Some(line) => parse_choice(&line, always),
            None => ApprovalChoice::Deny,
        };
        eprintln!(); // piped answers echo nothing on stderr — close the prompt line
        return choice;
    }
    let choice = tokio::task::spawn_blocking(move || read_key_choice(always))
        .await
        .unwrap_or(ApprovalChoice::Deny);
    eprintln!(); // raw mode echoes nothing — close the prompt line
    choice
}

/// Restores cooked mode on drop, so a panic or early return inside the key-read never leaves the
/// terminal in raw mode.
pub(super) struct RawModeGuard;
impl RawModeGuard {
    pub(super) fn enable() -> std::io::Result<Self> {
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
pub(super) fn read_key_choice(always: ApprovalChoice) -> ApprovalChoice {
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
pub(super) async fn read_stdin_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok().map(|_| line)
    })
    .await
    .ok()
    .flatten()
}

/// Map a typed y/a/N line to a choice (the non-terminal fallback). `always` is returned for `a`/`always`.
pub(super) fn parse_choice(line: &str, always: ApprovalChoice) -> ApprovalChoice {
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
/// Run a one-shot prompt turn.
pub(super) async fn run_prompt(flags: AgentFlags, prompt_words: Vec<String>) -> Result<()> {
    let prompt = prompt_words.join(" ");

    if prompt.trim().is_empty() {
        bail!("provide a prompt, e.g. `flux run \"summarize the README\"`");
    }

    // One engine: a prompt always runs the agentic Flux-Lang engine. `-p`/`--print` only means
    // print-and-exit (a chat-only turn just answers in prose; pass `--yes` for non-interactive
    // tool approval). The legacy tool-less raw-completion path is gone — there is one engine.
    run_agentic(&flags, prompt).await
}
