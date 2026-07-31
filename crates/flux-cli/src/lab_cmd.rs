//! D-179: the Deterministic Agent Lab's CLI surface — `flux record` and `flux test`.
//!
//! The CLI is the reference app on the SDK, so both commands are thin drivers over
//! [`flux_sdk::test::Scenario`] (D-174) rather than a parallel implementation: `record` runs ONE
//! live turn and writes a committed-safe fixture directory, `test` re-runs the real agent against
//! those fixtures offline — deny-all approver, never-called provider, zero network, $0 — and exits
//! non-zero if any of them regressed, so it can be a CI gate.
//!
//! A fixture is an ordinary `Storage::dir` store (`events.db` + `flow.db`) plus the Test Kit's own
//! `model.jsonl` / `plan.flux.snap` / `scenario.toml`. That is deliberate: `flux replay|fork|diff
//! --store tests/scenarios/<name>` opens one with the existing session tools, no fixture-specific
//! code path anywhere.

use super::*;

/// A never-called provider: `flux test` must never reach the model. If it does, that is a defect in
/// the hermetic replay path, not a cost surprise to swallow quietly.
struct NeverProvider;

#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "offline"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream, flux_core::Error> {
        Err(flux_core::Error::Other(
            "`flux test` replays hermetically and must never call the model — this is a bug in the \
             replay path, not a missing credential"
                .into(),
        ))
    }
}

/// Build the SDK client `Scenario::record` records against: the CLI's resolved provider and model,
/// this invocation's approval posture, and the session store `--store` selected (so a recording is
/// also an ordinary session that `flux sessions`/`flux replay` can see).
fn record_client(flags: &AgentFlags) -> Result<flux_sdk::Client> {
    let cwd = std::env::current_dir().context("current dir")?;
    let cfg = flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
    let model_spec = resolve_model_spec(&flags.model, &cfg);
    // Eager: a recording that fails on a missing credential must say so BEFORE it writes a
    // half-formed fixture directory.
    let ResolvedProvider {
        provider, model, ..
    } = resolve_cli_provider(&model_spec, true)?;
    flux_sdk::Client::builder()
        .model(model)
        .auto_approve(flags.yes)
        // C-307: `flux record` runs a real, live turn, so the operator's `[limits]` ceilings apply
        // to it exactly as they do to `flux run`. (`flux test`'s [`offline_client`] is deliberately
        // NOT wired — see its doc comment.)
        .resource_limits(cli_resource_limits(&cfg))
        .storage(flux_sdk::Storage::dir(flux_store_dir()?))
        .build(provider, &cwd)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// The offline client every `flux test` fixture replays against: deny-all approver (the builder
/// default — no `auto_approve`) and a provider that refuses to answer. Between them, a replay that
/// tried to do anything live would fail loudly instead of quietly costing money.
///
/// C-307 audited every surface that assembles a runtime without ceilings and wired all of them
/// except this one, **deliberately**: `flux test` is a regression gate whose whole value is that its
/// verdict depends only on the fixture. Reading the local `[limits]` table here would let a machine's
/// config decide a replay — a saturated `max_concurrent_tool_calls` refuses a queued call with a tool
/// error after `tool_call_queue_timeout`, which is a red test on one developer's box and green on
/// another. The bound this would buy is also not needed: a replay drives recorded traffic against a
/// never-called provider, so there is no runaway workload to cap.
fn offline_client() -> Result<flux_sdk::Client> {
    let cwd = std::env::current_dir().context("current dir")?;
    flux_sdk::Client::builder()
        .model("offline")
        .storage(flux_sdk::Storage::in_memory())
        .build(Box::new(NeverProvider), &cwd)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// A scenario/fixture name must be exactly one plain path segment (D-185): reject an empty name,
/// any path separator, and the `.`/`..` special components. Without the dot check, `flux record ..
/// "x"` resolves `dir.join("..")` to the PARENT of the scenarios root and — because no
/// `scenario.toml` exists there — the clobber guard never trips, so fixture files land outside
/// `--dir` entirely. Shared by `run_record` and `discover_fixtures` (`flux test <name>`) so both
/// commands reject exactly the same names.
fn validate_fixture_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains(std::path::MAIN_SEPARATOR)
    {
        bail!(
            "scenario name must be a single plain path segment (no separators, not `.`/`..`), \
             got `{name}`"
        );
    }
    Ok(())
}

/// `flux record <name> "<prompt>"` — run one live turn and write `<dir>/<name>/` as a fixture.
pub(super) async fn run_record(
    name: &str,
    prompt: Vec<String>,
    dir: std::path::PathBuf,
    flags: &AgentFlags,
) -> Result<()> {
    validate_fixture_name(name)?;
    let prompt = prompt.join(" ");
    let path = dir.join(name);
    let client = record_client(flags)?;
    eprintln!(
        "{}",
        style::dim(&format!("record · {name} · one live turn"))
    );
    let scenario = flux_sdk::test::Scenario::record(&client, &prompt, &path)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let m = scenario.manifest();
    println!("recorded {} → {}", m.name, path.display());
    println!("  session {}  model {}", m.session, m.model);
    println!(
        "  replay it offline with `flux test {name}` (or inspect it with \
         `flux replay --store {}`)",
        path.display()
    );
    Ok(())
}

/// One fixture's verdict.
struct TestRow {
    name: String,
    failure: Option<String>,
}

/// `flux test [<name>]` — replay every (or one) fixture offline and gate on the result.
pub(super) async fn run_test(
    name: Option<String>,
    dir: std::path::PathBuf,
    json: bool,
) -> Result<()> {
    let fixtures = discover_fixtures(&dir, name.as_deref())?;
    if fixtures.is_empty() {
        bail!(
            "no scenario fixtures under {} — record one with `flux record <name> \"<prompt>\"`",
            dir.display()
        );
    }
    let client = offline_client()?;
    let mut rows = Vec::new();
    for path in &fixtures {
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        rows.push(TestRow {
            name: label,
            failure: check_fixture(&client, path).await,
        });
    }

    let failed = rows.iter().filter(|r| r.failure.is_some()).count();
    if json {
        let report = serde_json::json!({
            "total": rows.len(),
            "failed": failed,
            "fixtures": rows.iter().map(|r| serde_json::json!({
                "name": r.name,
                "ok": r.failure.is_none(),
                "failure": r.failure,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for row in &rows {
            match &row.failure {
                None => println!("{} {}", style::green("ok"), row.name),
                Some(why) => {
                    println!("{} {}", style::red("FAILED"), row.name);
                    for line in why.lines() {
                        println!("    {line}");
                    }
                }
            }
        }
        eprintln!(
            "{}",
            style::dim(&format!(
                "{} fixture(s) · {failed} failed · offline, $0",
                rows.len()
            ))
        );
    }
    if failed > 0 {
        // `diff`-style: a regression is a non-zero exit so CI fails on it.
        std::process::exit(1);
    }
    Ok(())
}

/// Replay one fixture and return its failure text, or `None` when it reproduced its recording.
/// Both halves are checked and reported together: the WORLD (`faithful` — no divergence, every
/// recorded cell consumed) and the PLAN (`plan_snapshot` — the canonical Flux-Lang source still
/// matches the committed golden).
async fn check_fixture(client: &flux_sdk::Client, path: &std::path::Path) -> Option<String> {
    let scenario = match flux_sdk::test::Scenario::load(path) {
        Ok(s) => s,
        Err(e) => return Some(format!("cannot load fixture: {e}")),
    };
    let outcome = match scenario.replay(client).await {
        Ok(o) => o,
        Err(e) => return Some(format!("replay failed: {e}")),
    };
    let mut problems = Vec::new();
    if let Err(e) = outcome.faithful() {
        problems.push(e);
    }
    if let Err(e) = outcome.plan_snapshot() {
        problems.push(e);
    }
    (!problems.is_empty()).then(|| problems.join("\n\n"))
}

/// The fixture directories to run: one named fixture, or every immediate subdirectory of `dir` that
/// carries a `scenario.toml` (so unrelated files in the scenarios directory are simply skipped).
fn discover_fixtures(dir: &std::path::Path, name: Option<&str>) -> Result<Vec<std::path::PathBuf>> {
    if let Some(name) = name {
        validate_fixture_name(name)?;
        let path = dir.join(name);
        if !path.join("scenario.toml").exists() {
            bail!("no scenario fixture at {}", path.display());
        }
        return Ok(vec![path]);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut found: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("scenario.toml").exists())
        .collect();
    // Deterministic order — a test gate's output must not depend on directory iteration order.
    found.sort();
    Ok(found)
}
