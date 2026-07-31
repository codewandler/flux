use super::*;

/// Name the supported harness benchmark once per `flux eval` invocation (C-296).
///
/// **On stderr, deliberately.** stdout carries the scored summary a caller reads and `--report`
/// writes a Markdown artifact a caller diffs; a courtesy line in either is a corrupted parse, not a
/// nicety. This is the same routing `Sandbox::posture_disclosure` already uses for the same reason.
/// It is a pointer and not a deprecation: `flux eval` runs unchanged, with the same suites and the
/// same exit codes. Pinned by `crates/flux-cli/tests/bench_pointer.rs`.
fn bench_pointer() {
    eprintln!(
        "note: harness benchmarking lives in flux-bench — it runs the shipped flux binary against a\n\
         curated corpus with the model held fixed: https://github.com/codewandler/flux-bench\n\
         `flux eval` is unchanged: the in-repo scoring engine the self-improvement loop drives."
    );
}

/// `flux eval <adapter> [--tasks a,b] [--members a,b] [--limit N] [-m model] [--trials N]
/// [--report out.md] [--watch]` — run a benchmark suite ad-hoc through flux-eval and print a summary
/// (same adapters + scoring the `eval_run` op and improve loop use). `--watch` streams each task's
/// agent activity live; `--report` writes the categorized Markdown report.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_eval_cmd(
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
    bench_pointer();

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

/// The immutable role registry for the built-in strict-review protocol.
///
/// These names are part of an embedded security protocol, not extension points: consulting project
/// or user-global role discovery here would let an untrusted checkout replace a toolless reviewer
/// with a write-capable autonomous agent. Ordinary agents continue to use [`load_roles`].
pub(super) fn strict_review_roles() -> RoleRegistry {
    RoleRegistry::from_roles(flux_app::review::builtin_review_roles())
}

/// Build the `SubAgents` bundle for the strict-review protocol's reviewer fan-out, shared by both
/// `flux review` ([`run_review`]) and `flux app run strict-review` (the built-in-program branch of
/// [`run_app`]) so the two call sites cannot drift or regain project-controlled reviewer roles.
///
/// `resource_limits` (C-307) is a **required** parameter, resolved once by the caller from its own
/// `[limits]` table — see [`cli_resource_limits`]. It is not defaulted here on purpose: this helper
/// returned a bare `SubAgents::new` until C-307, which left every reviewer child of the hardest
/// fan-out flux ships running with no ceiling at all. A required argument is the seam-level way to
/// keep a future third caller from re-opening that hole by omission. Each child receives a
/// `ResourceLimits::independent_copy` of these ceilings (same numbers, own concurrency budget) —
/// installed by `LocalSpawner::spawn`, and per-child by design: a budget shared across the `task`
/// boundary deadlocks (C-299).
pub(super) fn build_review_sub_agents(
    model_spec: &str,
    model: impl Into<String>,
    max_tokens: u32,
    resource_limits: flux_runtime::ResourceLimits,
) -> Result<SubAgents> {
    let roles = strict_review_roles();
    let mut child_base = ToolRegistry::new();
    flux_tools::try_register_builtins(&mut child_base)?;
    let factory: ProviderFactory = {
        let spec = model_spec.to_string();
        Arc::new(move || provider_for(&spec).map_err(|e| flux_core::Error::Other(e.to_string())))
    };
    Ok(
        SubAgents::new(roles, child_base, factory, model, max_tokens)
            .with_resource_limits(resource_limits),
    )
}

/// `flux review --files <path>… [--format md|json] [--fail-on <severity>]` — run the strict-review
/// protocol (flux L-13; `docs/designs/strict-review-flows.md` "Phase 4") over `files` and print the
/// resulting `ReviewReport`. Runs the SAME embedded `strict_review` flow text
/// (`flux_app::review::STRICT_REVIEW_FLOW_SRC` — the checked-in `examples/strict_review.flux`, the
/// identical source the `review_code` app journey wraps as a composite op) through
/// `flux_sdk::FlowClient::run_flow` — the deterministic `parse` → `analyze` → `execute_with` path, no
/// model round-trip for the flow itself (only the reviewer sub-agents call a model). Self-contained:
/// The immutable reviewer roles and flow text ship in the binary, so this works in any repo without
/// trusting that repo's `.flux/agents/review-*.md`. Read-only: `strict_review`'s reviewer roles all
/// declare `tools: []`, and this command never writes anywhere but stdout.
pub(super) async fn run_review(
    flags: &ReviewFlags,
    files: Vec<String>,
    format: ReviewFormat,
    fail_on: Option<ReviewSeverity>,
) -> Result<()> {
    let cwd = std::env::current_dir().context("current dir")?;
    let cfg = flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
    let model_spec = resolve_model_spec(&flags.model, &cfg);

    let (provider, model): (Arc<dyn Provider>, String) =
        if model_spec == "mock" || model_spec.starts_with("mock/") {
            (Arc::new(MockCliProvider::default()), "mock".to_string())
        } else {
            let (native, _provider_name, m) = build_provider(&model_spec)?;
            (Arc::new(native), m)
        };

    // C-307: `flux review` is the other shipped surface that fans out to reviewer children, and it
    // assembles its envelope through the SDK rather than `build_agent_with` — so it needs the same
    // ceilings wired explicitly. Resolved once and shared by the flow client and the children (each
    // child copies the numbers into its own budget at spawn).
    let resource_limits = cli_resource_limits(&cfg);

    // Wire roles + sub-agents exactly like `build_agent`: `strict_review`'s bounded 3-role reviewer
    // fan-out (via `task`) delegates through the identical envelope the top-level agent uses.
    let sub_agents = build_review_sub_agents(
        &model_spec,
        model.clone(),
        flags.max_tokens,
        resource_limits.clone(),
    )?;

    // `strict_review`'s core is read-only by construction (git_status/git_diff/read_many + `task`
    // against immutable embedded `tools: []` reviewer roles — see the design's security
    // considerations); auto-approving
    // this specific, fixed flow's own ops is not the same authority `--yes` grants an arbitrary
    // prompt-compiled plan, so `review` doesn't offer `--yes` at all (see [`ReviewFlags`]).
    let mut client = flux_sdk::FlowClient::builder()
        .model(model)
        .auto_approve(true)
        .resource_limits(resource_limits)
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
pub(super) fn render_review_markdown(report: &flux_tools::cognition::ReviewReport) -> String {
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
pub(super) fn should_fail(
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
