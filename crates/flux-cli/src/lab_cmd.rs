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
    record_client_from(
        provider,
        model,
        &cwd,
        flux_sdk::Storage::dir(flux_store_dir()?),
        flags.yes,
        cli_resource_limits(&cfg),
    )
}

/// Assemble the recording client from already-resolved surface decisions.
///
/// A named seam rather than an inline chain in [`record_client`] (C-328): `record_client` resolves
/// the cwd, the config and a *live* provider before it ever reaches the builder, so nothing could
/// reach the chain below to check what it wires. That is exactly how C-314 happened — the
/// `.resource_limits(..)` line could be deleted with the whole `flux-cli` suite staying green. The
/// client this returns is the one `flux record` records against.
fn record_client_from(
    provider: Box<dyn Provider>,
    model: String,
    cwd: &std::path::Path,
    storage: flux_sdk::Storage,
    auto_approve: bool,
    resource_limits: flux_runtime::ResourceLimits,
) -> Result<flux_sdk::Client> {
    flux_sdk::Client::builder()
        .model(model)
        .auto_approve(auto_approve)
        // C-307: `flux record` runs a real, live turn, so the operator's `[limits]` ceilings apply
        // to it exactly as they do to `flux run`. (`flux test`'s [`offline_client`] is deliberately
        // NOT wired — see its doc comment.)
        // flux-pin: record_client_carries_the_configured_ceiling_to_its_executor
        .resource_limits(resource_limits)
        .storage(storage)
        .build(provider, cwd)
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

#[cfg(test)]
mod record_client_ceiling_wiring {
    //! C-314/C-328: the operator's `[limits]` ceilings must reach the client `flux record` records
    //! against.
    //!
    //! This test is deliberately **attributable to one line**: it observes only
    //! [`record_client_from`]'s `.resource_limits(..)`, so deleting `flux review`'s wiring leaves it
    //! green and deleting this one reds it alone. C-305's first round was sent back for the opposite
    //! — a single test that covered both sites and could not say which had regressed.
    //!
    //! What it asserts is the ceiling carried by the **executor the client dispatches through**, one
    //! layer past `Client::resource_limits`'s own field. It stops there rather than measuring
    //! occupancy (C-299's idiom) because `Client` has no post-build op registration, so no blocking
    //! probe can be placed in its registry; that an executor carrying these numbers enforces them is
    //! what `a_configured_limits_table_binds_for_the_cli_executor` already proves.

    use super::*;

    use std::time::Duration;

    /// The `[limits]` values here are ones no default would produce, so a green assertion cannot be
    /// an accident of the builder's own defaults.
    #[test]
    fn record_client_carries_the_configured_ceiling_to_its_executor() {
        let cfg: flux_config::Config = toml::from_str(
            "[limits]\nmax_concurrent_tool_calls = 3\ntool_call_queue_timeout_ms = 4321\n",
        )
        .expect("the `[limits]` concurrency keys must parse");

        let root = std::env::temp_dir().join(format!(
            "flux-c314-record-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&root).expect("create the test workspace root");

        let client = record_client_from(
            Box::<MockCliProvider>::default(),
            "mock".to_string(),
            &root,
            flux_sdk::Storage::in_memory(),
            true,
            // The one C-314 seam on this surface: `flux record` turns `[limits]` into ceilings here.
            cli_resource_limits(&cfg),
        )
        .expect("build the recording client");

        let limits = client.engine().executor.resource_limits();
        assert_eq!(
            limits.max_concurrent_tool_calls(),
            Some(3),
            "`[limits] max_concurrent_tool_calls` did not reach the executor `flux record` \
             dispatches through — the recording client is running unbounded"
        );
        assert_eq!(
            limits.tool_call_queue_timeout(),
            Duration::from_millis(4321),
            "the configured queue window did not reach the recording client's executor"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
