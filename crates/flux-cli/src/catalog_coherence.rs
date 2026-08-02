//! Metadata-coherence gate over the **production** operation catalog (C-208).
//!
//! # Why this module exists, given C-191 already exists
//!
//! C-191 encoded three invariants in [`flux_spec::coherence`] — I1 risk floor, I2 destructive
//! floor, I3 repeatability floor — and gated them at two places: over `try_register_builtins` (a
//! build-time test in `flux-tools`) and over plugin manifests at load. Neither of those is the
//! catalog a running agent dispatches against.
//!
//! The production registry is assembled in [`crate::execution::build_agent_with`], and beyond the
//! built-in pack it also holds the cognition pack, `flux_eval::try_register_eval_ops`, the
//! reflect/flows/render packs, `flux_web::try_register_web`, the datasource and endpoint ops,
//! `TaskTool`, and every config-authored model stage. All of them reach the same
//! `Executor::dispatch`, the same `PermissionManager` floor, and the same approver
//! (`resolve_permissions` installs `StdinApprover`, or `AllowApprover` under `--yes`), so all of
//! them are gated by what they declare about themselves. When this gate was first run it raised
//! **22 violations across 19 operations** — the eleven the story had itemised, plus `explore` and
//! `grade`, which nobody had found because nothing had ever walked this catalog.
//!
//! # Why the gate lives here and not next to the invariants
//!
//! It cannot live in `flux-tools` (where C-191's gate is): `flux-web`, `flux-eval` and
//! `flux-cognition` sit *above* `flux-tools` in the layer map (`flux_codegate::layer` — L5/L3 over
//! L2), so `flux-tools` cannot see the ops it would need to walk. `flux-cli` is L6 and depends on
//! everything, which makes it the only crate that can assemble the whole catalog. The layering rule
//! is not bent to place this gate; the gate moved to fit the layering.
//!
//! `flux-cli` has no library target, so this is a `#[cfg(test)]` module inside the binary rather
//! than an integration test under `tests/`. That is the point, not a workaround: it lets the census
//! call the *same* private `register_tool_packs` production calls, instead of a parallel copy of it
//! that would drift.
//!
//! # The census, and how it is kept honest
//!
//! [`production_catalog`] mirrors the registration sequence in `build_agent_with`, with the
//! configuration switches that gate optional ops (`[consult] model`, `[wakeup] enabled`, `--dev`)
//! all turned **on**, so the census is the widest catalog the CLI can assemble rather than the
//! default one. Providers and stores are the offline `mock` provider and an in-memory event store —
//! the census registers ops and reads their specs, it never executes one.
//!
//! A census like this rots the moment someone adds a pack to `build_agent_with` and not here.
//! [`every_registration_seam_in_the_cli_assembly_is_classified`] is the guard against that: it
//! recursively parses every production Rust module below `flux-cli/src` and fails on any
//! registration call it has not been told about. A new pack must therefore be added to the census
//! or explicitly excluded with a reason, regardless of which CLI module wires it in.
//!
//! # Posture
//!
//! The eight `Network`-at-`Risk::Low`-without-`Read` violations were not a mechanical fix, and the
//! decision that resolved them (Group A gains `Read`; Group B — the billable model calls — rises to
//! `Risk::Medium`) is recorded in `docs/designs/security-assurance.md`. Read that before changing
//! any declaration this gate covers.

use std::collections::BTreeSet;
use std::sync::Arc;

use flux_runtime::ToolRegistry;
use flux_spec::{metadata_violations, Risk};
use syn::visit::Visit;

/// The audit source label `build_agent_with` registers `TaskTool` under. Shared with the census so
/// the drift guard's `COVERED_SOURCES` entry and the registration it approves cannot drift apart:
/// changing the label in `execution.rs` and nowhere else makes the guard report an unclassified
/// label instead of silently approving a name that no longer appears.
const TASK_OP_SOURCE: &str = "flux-cli sub-agent task operation";

/// The audit source label `try_register_fleet` registers the `fleet.*` ops under, held here for the
/// same reason [`TASK_OP_SOURCE`] is: they arrive through the generic `try_register_all_from`, whose
/// method name alone approves nothing.
const FLEET_OP_SOURCE: &str = "flux-cli fleet dispatch";

/// The audit source label `try_register_fleet` registers the C-243 worker-**lifecycle** ops under.
/// Separate from [`FLEET_OP_SOURCE`] because the two halves have genuinely different authority — the
/// dispatch ops make network requests, the lifecycle ops create OS processes — and an audit that
/// lumped them under one label could not say which of the two a registration widened.
const FLEET_LIFECYCLE_SOURCE: &str = "flux-cli fleet lifecycle";

/// The domain the census binds a representative work board under (A-131).
///
/// A board's real domain is the Program's `datasource` name, so no fixed name is *the* production
/// one — but every board generates the same six operations from the same host code, so one
/// representative domain covers the shape all of them share, exactly as `census_stage` does for
/// config-authored model stages.
const CENSUS_BOARD_DOMAIN: &str = "census_board";

/// Every op the CLI can register, assembled the way `build_agent_with` assembles it, with each
/// config-gated pack switched on.
///
/// Optional inputs are substituted, never faked: the cognition/consult providers are the offline
/// `MockCliProvider` that `-m mock` already selects, the event store is in-memory, and the
/// datasource backend is the same `MemoryBackend` `build_doc_index` wraps. Nothing here executes an
/// op; the census exists to read `Tool::spec()`.
fn production_catalog() -> ToolRegistry {
    let events = Arc::new(flux_events::EventStore::in_memory().expect("in-memory event store"));

    // Every config switch that gates an *optional* op, turned on — `consult` and `schedule_wakeup`
    // are off in a default workspace, and an op nobody can see is still an op that dispatches once
    // an operator enables it.
    let mut cfg = flux_config::Config::default();
    cfg.consult.model = Some("mock".to_string());
    cfg.wakeup.enabled = true;

    let flags = crate::args::AgentFlags::from_model_yes(Some("mock"), true);
    let cog_provider = crate::execution::resolve_cli_provider("mock", true)
        .expect("the offline mock provider resolves")
        .provider;

    let mut registry = ToolRegistry::new();
    // The built-in pack — already gated by C-191, included so the census is the whole catalog and
    // not "the part C-191 misses".
    flux_tools::try_register_builtins(&mut registry).expect("built-ins register");
    // `--dev`: reachable in production behind a flag, so it is part of the catalog.
    flux_tools::try_register_dev_builtins(&mut registry).expect("dev built-ins register");
    // C-223: the `pane.*` ops, surfaced by the presence of a `SurfaceSink` at assembly time rather
    // than by config. Registered here with the decision switched **on**, for the same reason
    // `[consult] model` and `[wakeup] enabled` are: an op only an interactive surface assembles is
    // still an op that reaches `Executor::dispatch`, and a reader looking it up must find it.
    flux_tools::try_register_surface_ops(&mut registry, true).expect("the pane ops register");
    flux_tools::try_register_user_interaction(
        &mut registry,
        Some(flux_runtime::InteractionCapabilities::text()),
    )
    .expect("the user interaction op registers");
    registry
        .try_register_from(TASK_OP_SOURCE, Arc::new(flux_orchestrate::TaskTool))
        .expect("the task op registers");

    // Cognition + consult + wakeup + eval + reflect/flows/render, through the *production*
    // registrar rather than a copy of its body.
    crate::execution::register_tool_packs(
        &mut registry,
        Some(cog_provider),
        "mock",
        &flags,
        &cfg,
        "mock",
        &events,
    )
    .expect("the CLI tool packs register");

    let backend: Arc<dyn flux_capabilities::DatasourceBackend> =
        Arc::new(flux_capabilities::MemoryBackend::new());
    flux_capabilities::try_register_datasource_ops(&mut registry, backend.clone())
        .expect("datasource ops register");

    // A declared work board (A-131). Since `build_datasources` resolves a `board:<backend>` kind
    // into a `WorkBoard`, its six generated operations are catalog ops a `flux app run` session
    // dispatches against — four of them *writes* — so they belong inside this gate. The production
    // call site is `app_cmd.rs`'s registration loop over `ProgramDatasources::boards`, not
    // `execution.rs`; the census reaches the same `try_register_work_board` the loop calls, which is
    // what makes the classification below more than a name on a list.
    flux_capabilities::try_register_work_board(
        &mut registry,
        CENSUS_BOARD_DOMAIN,
        Arc::new(flux_capabilities::MemoryBoard::new()),
    )
    .expect("a declared work board registers");

    // The record sink matters to the census: `web.fetch` / `web.crawl` disclose their durable
    // `web.page` datasource contribution as the `write_db` *semantic* effect only when a sink is
    // wired (C-58), and `metadata_violations` reads semantic effects for I2. Registering the
    // catalog-only shape here would gate a spec production never assembles.
    flux_web::try_register_web(
        &mut registry,
        &flux_web::WebOptions {
            records: Some(Arc::new(crate::execution::BackendRecordSink { backend })),
            ..flux_web::WebOptions::default()
        },
    )
    .expect("the web pack registers");

    // The endpoint ops, as `assemble_integrations` builds them. The broker needs no plugin to be
    // loaded — an empty registry still registers all five ops.
    let plugins = Arc::new(flux_capabilities::PluginRegistry::new());
    let endpoints = Arc::new(flux_capabilities::EndpointRegistry::with_path(
        std::path::PathBuf::new(),
    ));
    let broker = Arc::new(flux_capabilities::EndpointBroker::new(
        Arc::new(flux_capabilities::HostProviderInvoker::new(plugins.clone())),
        plugins,
        endpoints.clone(),
    ));
    registry
        .try_register_all_from(
            "cli endpoint integration",
            flux_capabilities::endpoint_tools(broker, endpoints),
        )
        .expect("the endpoint ops register");

    // A config-authored model stage. Every `[agent.stages.*]` entry lowers through this one
    // registrar with a caller-supplied name and schema, so one representative stage covers the
    // shape all of them share.
    flux_tools::reflect::try_register_model_stage(
        &mut registry,
        "census_stage",
        "A representative config-authored model stage.",
        serde_json::json!({"type": "object"}),
        serde_json::json!({"type": "object"}),
    )
    .expect("a config-authored model stage registers");

    registry
}

const RISK_REFERENCE_EXCLUDED: &[(&str, &str)] = &[(
    "web.search",
    "the first-party websearch plugin is installed from the separately released plugin pack and \
     is not part of the native CLI catalog until a signed plugin descriptor is loaded",
)];

/// The in-repo reference, read once, from the one path both directions of the coherence check use.
fn in_repo_reference() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../flux-flow/docs/ops-reference.md"),
    )
    .expect("the ops reference is readable")
}

#[derive(Debug, Default)]
struct RiskReferenceCheck {
    checked: usize,
    excluded: Vec<String>,
    errors: Vec<String>,
}

fn check_published_risk_column(reference: &str, registry: &ToolRegistry) -> RiskReferenceCheck {
    let mut result = RiskReferenceCheck::default();
    let mut risk_column = None;
    let mut seen = BTreeSet::new();
    for line in reference.lines() {
        let cells = markdown_table_cells(line);
        if !line.trim_start().starts_with('|') {
            risk_column = None;
            continue;
        }
        if let Some(index) = cells.iter().position(|cell| cell == "risk") {
            risk_column = Some(index);
            continue;
        }
        let Some(index) = risk_column else { continue };
        let Some(op) = cells
            .get(1)
            .and_then(|cell| cell.strip_prefix('`'))
            .and_then(|cell| cell.strip_suffix('`'))
        else {
            continue;
        };
        if !seen.insert(op.to_string()) {
            result
                .errors
                .push(format!("the risk table documents `{op}` more than once"));
            continue;
        }
        let Some(documented) = cells.get(index).map(String::as_str) else {
            result
                .errors
                .push(format!("`{op}` has no value in the published risk column"));
            continue;
        };
        let Some(tool) = registry.get(op) else {
            if let Some((_, reason)) = RISK_REFERENCE_EXCLUDED
                .iter()
                .find(|(excluded, _)| *excluded == op)
            {
                result.excluded.push(format!("{op}: {reason}"));
            } else {
                result.errors.push(format!(
                    "`{op}` is published in the risk table but cannot be resolved in the production catalog"
                ));
            }
            continue;
        };
        let declared = match tool.spec().risk {
            Risk::Low => "Low",
            Risk::Medium => "Medium",
            Risk::High => "High",
            Risk::Destructive => "Destructive",
        };
        result.checked += 1;
        if documented != declared {
            result.errors.push(format!(
                "`{op}` is declared `{declared}` but the reference documents it as `{documented}`"
            ));
        }
    }
    result
}

fn markdown_table_cells(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            cell.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
            cell.push(ch);
        } else if ch == '|' {
            cells.push(cell.trim().to_string());
            cell.clear();
        } else {
            cell.push(ch);
        }
    }
    cells.push(cell.trim().to_string());
    cells
}

/// Operations that cannot be a literal row in the in-repo reference, and why. Matched as a name
/// prefix so one entry covers a whole generated family.
///
/// This list is *only* for ops whose production name does not exist until a program or an operator
/// chooses it. "Niche", "evidence-gated", or "the other reference has it" are not reasons — C-248
/// exists because the whole eval family was absent for exactly those excuses.
const REFERENCE_COVERAGE_EXCLUDED: &[(&str, &str)] = &[
    (
        "census_board.",
        "a work board's eleven operations are generated under the *program's* datasource name \
         (A-131), so no literal name is the production one; the reference documents the shape as \
         `<domain>.list` / `.get` / … instead",
    ),
    (
        "census_stage",
        "a config-authored model stage is named by the operator under `[agent.stages.<name>]`, so \
         the reference documents the seam rather than any one name",
    ),
];

#[derive(Debug, Default)]
struct ReferenceCoverage {
    documented: usize,
    exercised_exclusions: BTreeSet<&'static str>,
    missing: Vec<String>,
}

/// Every op the production catalog registers must appear as a row in one of the reference's op
/// tables — a *row*, not a passing prose mention, so it arrives with a signature and a description
/// and (where the table carries a Risk column) lands inside
/// [`check_published_risk_column`]'s reach too.
fn check_reference_coverage(reference: &str, registry: &ToolRegistry) -> ReferenceCoverage {
    let mut documented = BTreeSet::new();
    for line in reference.lines() {
        if !line.trim_start().starts_with('|') {
            continue;
        }
        if let Some(op) = markdown_table_cells(line)
            .get(1)
            .and_then(|cell| cell.strip_prefix('`'))
            .and_then(|cell| cell.strip_suffix('`'))
        {
            documented.insert(op.to_string());
        }
    }

    let mut result = ReferenceCoverage::default();
    for name in registry.names() {
        if let Some((prefix, _)) = REFERENCE_COVERAGE_EXCLUDED
            .iter()
            .find(|(prefix, _)| name.starts_with(prefix))
        {
            result.exercised_exclusions.insert(prefix);
        } else if documented.contains(&name) {
            result.documented += 1;
        } else {
            result.missing.push(name);
        }
    }
    result
}

/// C-248, the other direction of the same coherence: [`check_published_risk_column`] walks the
/// *reference* and holds every row to the catalog, so a documented op that drifts or disappears
/// reddens the gate — but an op that was never written down at all is invisible to it. That is how
/// `crates/flux-flow/docs/ops-reference.md` came to document **none** of the eval / self-improvement
/// family (`eval_run`, `gate_check`, `git_snapshot`, `git_tag`, `git_reset`, `guard_protected`, …)
/// while the gate stayed green, and why C-238's `git_revert` → `git_reset` rename had exactly one
/// guarded reference to update instead of two.
///
/// Shape (a) of the story: the in-repo reference covers the eval family too, pinned the way
/// `website/docs/language/ops.md` is pinned by
/// `operations_reference_covers_the_registered_public_catalog`. Shape (b) — scoping the file to the
/// builtin catalog — was rejected: these ops reach the same `Executor::dispatch` in a running
/// `flux` (they are in this very census, which is why the metadata gate above already covers them),
/// so an agent reading the in-repo catalog and finding nothing is reading a reference that is
/// wrong, not one that is narrow.
#[test]
fn the_in_repo_reference_covers_the_whole_production_catalog() {
    let registry = production_catalog();
    let result = check_reference_coverage(&in_repo_reference(), &registry);
    assert!(
        result.documented > 140,
        "only {} catalog ops resolved to a reference row — the table parser or the census probably \
         stopped early",
        result.documented
    );
    assert_eq!(
        result.exercised_exclusions.len(),
        REFERENCE_COVERAGE_EXCLUDED.len(),
        "a reasoned coverage exclusion was not exercised: {:?}",
        result.exercised_exclusions
    );
    assert!(
        result.missing.is_empty(),
        "crates/flux-flow/docs/ops-reference.md documents no row for {} production op(s): {:?}",
        result.missing.len(),
        result.missing
    );
}

/// The guard on the guard: an op present in the catalog and absent from the reference must be named,
/// and a prose mention must not satisfy it. Without this, the check above could quietly degrade into
/// a substring search that any nearby backtick satisfies — the failure mode that let the eval family
/// hide behind a *paragraph* naming it while no row existed.
#[test]
fn an_undocumented_catalog_op_is_reported_and_prose_does_not_count() {
    let registry = production_catalog();
    let reference = "| op | signature | risk | description |\n\
                     |---|---|---|---|\n\
                     | `bash` | `command` | High | Run a shell command |\n\
                     \n\
                     The eval family (`gate_check`, `git_snapshot`) is documented elsewhere.\n";
    let result = check_reference_coverage(reference, &registry);
    assert_eq!(result.documented, 1, "{result:?}");
    for op in ["gate_check", "git_snapshot", "eval_run"] {
        assert!(
            result.missing.iter().any(|missing| missing == op),
            "`{op}` was not reported as missing: {result:?}"
        );
    }
}

/// C-233: the Risk column operators read is checked against the same widest production census as
/// metadata coherence. No unresolved row is silently skipped; the separately shipped plugin alias
/// is the sole reasoned exception, and the count floor makes an empty/changed table fail closed.
#[test]
fn the_published_risk_column_matches_the_production_catalog() {
    let registry = production_catalog();
    let reference = in_repo_reference();
    let result = check_published_risk_column(&reference, &registry);
    assert!(
        result.checked > 60,
        "only {} published Risk rows resolved in the production catalog — the table parser or \
         census probably stopped early; exclusions: {:?}",
        result.checked,
        result.excluded
    );
    assert_eq!(
        result.excluded.len(),
        RISK_REFERENCE_EXCLUDED.len(),
        "a reasoned risk-table exclusion disappeared or was not exercised: {:?}",
        result.excluded
    );
    assert!(
        result.errors.is_empty(),
        "crates/flux-flow/docs/ops-reference.md has drifted from the production catalog:\n  {}",
        result.errors.join("\n  ")
    );
}

#[test]
fn a_non_builtin_published_risk_drift_is_caught() {
    let registry = production_catalog();
    let reference = "| op | signature | risk | description |\n\
                     |---|---|---|---|\n\
                     | `browser.close` | `session` | Low | deliberately wrong fixture |";
    let result = check_published_risk_column(reference, &registry);
    assert_eq!(result.checked, 1);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("`browser.close` is declared `Medium`")),
        "a non-built-in risk drift passed silently: {result:?}"
    );
}

#[test]
fn an_unresolved_published_risk_row_fails_with_its_name() {
    let registry = production_catalog();
    let reference = "| op | risk |\n|---|---|\n| `future.unregistered` | Low |";
    let result = check_published_risk_column(reference, &registry);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("`future.unregistered`")),
        "an unresolved row was silently skipped: {result:?}"
    );
}

/// Every `metadata_violations` sentence raised by every op in `registry`, sorted for a stable
/// failure message.
fn violations_in(registry: &ToolRegistry) -> Vec<String> {
    let mut violations = Vec::new();
    for name in registry.names() {
        let tool = registry.get(&name).expect("a named op resolves");
        violations.extend(metadata_violations(&tool.spec(), &tool.semantic_effects()));
    }
    violations.sort();
    violations
}

/// The gate. Every operation the production CLI can dispatch declares a coherent
/// effects/risk/idempotency/access combination — or carries an entry in
/// `flux_spec::coherence::EXEMPT` stating why its declaration is already the honest one.
#[test]
fn every_operation_in_the_production_catalog_is_metadata_coherent() {
    let registry = production_catalog();
    let violations = violations_in(&registry);
    assert!(
        violations.is_empty(),
        "{} operation(s) in the production catalog declare an incoherent metadata combination:\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

/// The census must be *strictly wider* than C-191's, or this gate is an expensive alias for a test
/// that already exists. A regression here means a pack stopped being assembled — the failure mode
/// where this test keeps passing while covering less.
#[test]
fn the_census_is_strictly_wider_than_the_builtin_pack() {
    let mut builtins = ToolRegistry::new();
    flux_tools::try_register_builtins(&mut builtins).expect("built-ins register");
    let catalog = production_catalog();

    for name in builtins.names() {
        assert!(
            catalog.get(&name).is_some(),
            "the census lost the built-in op `{name}`"
        );
    }
    // One sentinel per pack the census exists to reach. Named individually so a dropped pack points
    // at itself instead of failing an opaque count.
    for (op, pack) in [
        ("ai.judge", "cognition"),
        ("consult", "consult (config-gated)"),
        ("schedule_wakeup", "wakeup (config-gated)"),
        ("improve_log", "eval"),
        ("detect_intent", "reflect"),
        ("flow_list", "flows"),
        ("flow_render", "render"),
        ("web.fetch", "flux-web"),
        ("browser.close", "flux-web browser tier"),
        ("search", "datasource"),
        ("endpoint.import", "endpoint"),
        ("task", "sub-agent delegation"),
        ("fleet.dispatch", "outbound A2A fleet dispatch"),
        ("census_board.claim", "declared work boards"),
        ("census_stage", "config-authored model stages"),
        (
            "pane.open",
            "the agent-authored surface (sink-presence gated)",
        ),
        (
            "user.ask",
            "typed user interaction (responder-presence gated)",
        ),
    ] {
        assert!(
            catalog.get(op).is_some(),
            "the census no longer covers the {pack} pack (`{op}` is missing)"
        );
    }
    assert!(
        catalog.names().len() > builtins.names().len(),
        "the census is no wider than the built-in pack C-191 already gates"
    );
}

/// A-131, the named failing-first test: A-116 landed `fleet.dispatch` / `.status` / `.cancel`, but
/// they were constructed nowhere outside their own module — the only other mention in the workspace
/// was a re-export. An op the production assembly never registers cannot be called by a Program, so
/// the fleet existed in code and was unreachable from a running `flux`.
///
/// Asserted over the production census rather than over a hand-built registry, so it also proves the
/// ops reach the metadata-coherence gate above.
#[test]
fn the_fleet_ops_are_reachable_from_the_production_catalog() {
    let catalog = production_catalog();
    for op in ["fleet.dispatch", "fleet.status", "fleet.cancel"] {
        assert!(
            catalog.get(op).is_some(),
            "the production catalog does not register `{op}` — a Program cannot dispatch to a \
             remote worker"
        );
    }
    // C-243, the same argument one layer down: a dispatch op is only reachable for a worker that
    // exists, and until the lifecycle ops were registered nothing could make one.
    for op in ["fleet.start", "fleet.worker_status", "fleet.stop"] {
        assert!(
            catalog.get(op).is_some(),
            "the production catalog does not register `{op}` — a Program cannot start a worker, so \
             every wave is a wave of one"
        );
    }
}

/// The sub-agent registry (`child_base` in `build_agent_with`) is one of the two open registration
/// seams C-208 had to account for. It is not open: it is exactly `try_register_builtins`, so a
/// child agent's catalog is a strict subset of its parent's and is gated twice over. Asserted
/// rather than assumed, because "the child registry is a subset" is a property a future edit could
/// falsify silently — the moment a pack is added there and not to the parent, this fails.
#[test]
fn the_sub_agent_base_registry_is_a_coherent_subset_of_the_catalog() {
    let mut child_base = ToolRegistry::new();
    flux_tools::try_register_builtins(&mut child_base).expect("built-ins register");
    assert!(violations_in(&child_base).is_empty());

    let catalog = production_catalog();
    for name in child_base.names() {
        assert!(
            catalog.get(&name).is_some(),
            "the sub-agent base registry offers `{name}`, which the parent catalog does not — the \
             child surface is no longer a subset of the gated one"
        );
    }
}

/// Registration methods whose production call sites are represented by [`production_catalog`].
const COVERED_REGISTRATION_SEAMS: &[&str] = &[
    "try_register_builtins",
    "try_register_dev_builtins",
    "try_register_surface_ops",
    "try_register_user_interaction",
    "try_register_eval_ops",
    "try_register_reflect",
    "try_register_flows",
    "try_register_render",
    "try_register_datasource_ops",
    "try_register_work_board",
    "try_register_fleet",
    "try_register_web",
    "try_register_model_stage",
    "try_register_from",
    "try_register_all_from",
    "try_register",
];

/// Registration methods that cannot be represented by a repository-owned static census.
const EXCLUDED_REGISTRATION_SEAMS: &[(&str, &str)] = &[(
    "try_register_op",
    "flux-sdk's embedder seam accepts operations authored outside this repository; plugin metadata \
     is checked when it crosses the trust boundary by flux-plugin's op_coherence_warnings",
)];

/// Dynamic source expressions that cannot name a repository-owned static pack.
const EXCLUDED_REGISTRATION_SOURCES: &[(&str, &str)] = &[(
    "source",
    "assemble_integrations uses a runtime source for endpoint and subprocess-plugin operations; \
     endpoint operations are represented directly and plugin metadata is checked at load",
)];

#[derive(Debug, PartialEq, Eq)]
struct RegistrationCall {
    module: String,
    seam: String,
    source: Option<String>,
    /// Every argument of the call, rendered by [`registration_source`]. Kept for **all** seams, not
    /// just the source-labelled ones, because some seams carry a *decision* rather than a pack —
    /// C-305's `try_register_surface_ops(registry, surface_sink.is_some())` is the case in point,
    /// and the thing worth pinning there is where the decision came from.
    arguments: Vec<String>,
}

fn covered_registration_sources() -> [String; 6] {
    [
        "\"flux-cli cognition pack\"".to_string(),
        format!("{TASK_OP_SOURCE:?}"),
        format!("{FLEET_OP_SOURCE:?}"),
        format!("{FLEET_LIFECYCLE_SOURCE:?}"),
        "ConsultTool".to_string(),
        "WakeupTool".to_string(),
    ]
}

fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let syn::Meta::List(meta) = &attr.meta else {
            return false;
        };
        meta.path.is_ident("cfg") && meta.tokens.to_string() == "test"
    })
}

fn registration_source(expr: &syn::Expr) -> String {
    match expr {
        syn::Expr::Lit(expr) => match &expr.lit {
            syn::Lit::Str(value) => format!("{:?}", value.value()),
            _ => "<non-string literal>".to_string(),
        },
        syn::Expr::Path(expr) => expr.path.segments.last().map_or_else(
            || "<empty path>".to_string(),
            |segment| segment.ident.to_string(),
        ),
        syn::Expr::Reference(expr) => registration_source(&expr.expr),
        syn::Expr::Paren(expr) => registration_source(&expr.expr),
        syn::Expr::Group(expr) => registration_source(&expr.expr),
        syn::Expr::MethodCall(expr) if expr.method == "clone" && expr.args.is_empty() => {
            registration_source(&expr.receiver)
        }
        // Any other method call carries a *decision*, not a handle, so the method is part of what
        // is being rendered: `surface_sink.is_some()` and `surface_sink.is_none()` name the same
        // receiver and mean opposite things, and rendering only the receiver let the inverted one
        // through — it would advertise `pane.*` in every headless catalog (C-305).
        syn::Expr::MethodCall(expr) => {
            format!("{}.{}()", registration_source(&expr.receiver), expr.method)
        }
        syn::Expr::Call(expr) => match expr.func.as_ref() {
            syn::Expr::Path(function) => function
                .path
                .segments
                .iter()
                .rev()
                .find(|segment| segment.ident != "new")
                .map_or_else(
                    || "<empty constructor>".to_string(),
                    |segment| segment.ident.to_string(),
                ),
            _ => "<complex constructor>".to_string(),
        },
        _ => "<complex expression>".to_string(),
    }
}

struct RegistrationVisitor<'a> {
    module: &'a str,
    calls: Vec<RegistrationCall>,
}

impl RegistrationVisitor<'_> {
    fn record<'ast>(
        &mut self,
        seam: &syn::Ident,
        arguments: impl Iterator<Item = &'ast syn::Expr>,
    ) {
        let seam = seam.to_string();
        if !seam.starts_with("try_register") {
            return;
        }
        let arguments: Vec<String> = arguments.into_iter().map(registration_source).collect();
        let source = matches!(
            seam.as_str(),
            "try_register" | "try_register_from" | "try_register_all_from"
        )
        .then(|| {
            arguments
                .first()
                .cloned()
                .unwrap_or_else(|| "<missing argument>".to_string())
        });
        self.calls.push(RegistrationCall {
            module: self.module.to_string(),
            seam,
            source,
            arguments,
        });
    }
}

impl<'ast> Visit<'ast> for RegistrationVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !has_cfg_test(&item.attrs) {
            syn::visit::visit_item_fn(self, item);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(function) = call.func.as_ref() {
            if let Some(seam) = function.path.segments.last().map(|segment| &segment.ident) {
                self.record(seam, call.args.iter());
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "try_register" {
            let receiver = registration_source(&call.receiver);
            let source = if receiver == "registry" {
                call.args
                    .first()
                    .map_or_else(|| "<missing argument>".to_string(), registration_source)
            } else {
                receiver
            };
            self.calls.push(RegistrationCall {
                module: self.module.to_string(),
                seam: call.method.to_string(),
                source: Some(source),
                arguments: call.args.iter().map(registration_source).collect(),
            });
        } else {
            self.record(&call.method, call.args.iter());
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}

fn registration_calls_in_source(
    module: &str,
    source: &str,
) -> Result<Vec<RegistrationCall>, String> {
    let syntax =
        syn::parse_file(source).map_err(|error| format!("cannot parse {module}: {error}"))?;
    let mut visitor = RegistrationVisitor {
        module,
        calls: Vec::new(),
    };
    visitor.visit_file(&syntax);
    Ok(visitor.calls)
}

fn rust_sources_below(root: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    fn walk(
        directory: &std::path::Path,
        sources: &mut Vec<std::path::PathBuf>,
    ) -> Result<(), String> {
        let entries = std::fs::read_dir(directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "cannot read an entry below {}: {error}",
                    directory.display()
                )
            })?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, sources)?;
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                sources.push(path);
            }
        }
        Ok(())
    }

    let mut sources = Vec::new();
    walk(root, &mut sources)?;
    sources.sort();
    Ok(sources)
}

fn external_module_candidates(parent: &std::path::Path, name: &str) -> Vec<std::path::PathBuf> {
    let directory = parent.parent().expect("a Rust source has a parent");
    let mut candidates = vec![
        directory.join(format!("{name}.rs")),
        directory.join(name).join("mod.rs"),
    ];
    if parent.file_name().is_some_and(|name| name != "mod.rs") {
        if let Some(stem) = parent.file_stem() {
            candidates.push(directory.join(stem).join(format!("{name}.rs")));
            candidates.push(directory.join(stem).join(name).join("mod.rs"));
        }
    }
    candidates
}

fn test_only_external_modules(
    sources: &[std::path::PathBuf],
) -> Result<BTreeSet<std::path::PathBuf>, String> {
    let mut excluded = BTreeSet::new();
    for source_path in sources {
        let source = std::fs::read_to_string(source_path)
            .map_err(|error| format!("cannot read {}: {error}", source_path.display()))?;
        let syntax = syn::parse_file(&source)
            .map_err(|error| format!("cannot parse {}: {error}", source_path.display()))?;
        for item in syntax.items {
            let syn::Item::Mod(module) = item else {
                continue;
            };
            if module.content.is_some() || !has_cfg_test(&module.attrs) {
                continue;
            }
            for candidate in external_module_candidates(source_path, &module.ident.to_string()) {
                if candidate.is_file() {
                    excluded.insert(candidate);
                }
            }
        }
    }
    Ok(excluded)
}

fn cli_registration_calls() -> Result<(usize, Vec<RegistrationCall>), String> {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let sources = rust_sources_below(&source_root)?;
    let test_only = test_only_external_modules(&sources)?;
    let mut calls = Vec::new();
    let mut scanned = 0usize;

    for path in sources {
        if test_only.contains(&path) {
            continue;
        }
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let module = path
            .strip_prefix(&source_root)
            .unwrap_or(&path)
            .display()
            .to_string();
        calls.extend(registration_calls_in_source(&module, &source)?);
        scanned += 1;
    }
    Ok((scanned, calls))
}

fn registration_classification_errors(calls: &[RegistrationCall]) -> Vec<String> {
    let covered_sources = covered_registration_sources();
    let mut errors = BTreeSet::new();

    for call in calls {
        if !COVERED_REGISTRATION_SEAMS.contains(&call.seam.as_str())
            && !EXCLUDED_REGISTRATION_SEAMS
                .iter()
                .any(|(seam, _)| *seam == call.seam)
        {
            errors.insert(format!(
                "{}: unclassified registration seam `{}`",
                call.module, call.seam
            ));
        }
        let Some(source) = &call.source else {
            continue;
        };
        if !covered_sources.contains(source)
            && !EXCLUDED_REGISTRATION_SOURCES
                .iter()
                .any(|(name, _)| *name == source)
        {
            errors.insert(format!(
                "{}: unclassified source label {} at seam `{}`",
                call.module, source, call.seam
            ));
        }
    }
    errors.into_iter().collect()
}

/// Drift guard over the recursively discovered production Rust modules below `flux-cli/src`.
///
/// Parsing the modules as Rust syntax makes comments, string contents, and `#[cfg(test)]` modules
/// inert. Both registration method names and source labels are classified because the generic
/// `try_register_from` methods otherwise allow a new pack to inherit an already-approved seam.
///
/// The remaining limit is deliberate and narrow: a new pack can reuse an already-classified source
/// label. Registry labels are audit descriptions rather than unique pack identities, so preventing
/// reuse requires a separate identity contract rather than a stronger source scan.
#[test]
fn every_registration_seam_in_the_cli_assembly_is_classified() {
    let (scanned, calls) = cli_registration_calls().expect("the CLI source tree parses");
    let seams: BTreeSet<_> = calls.iter().map(|call| call.seam.as_str()).collect();
    let sources: BTreeSet<_> = calls
        .iter()
        .filter_map(|call| call.source.as_deref())
        .collect();

    assert!(
        scanned >= 20 && calls.len() >= 20 && seams.len() >= 8 && sources.len() >= 4,
        "registration census looks vacuous: scanned={scanned}, calls={}, seams={seams:?}, \
         sources={sources:?}",
        calls.len()
    );
    assert!(
        EXCLUDED_REGISTRATION_SEAMS
            .iter()
            .all(|(_, reason)| !reason.trim().is_empty())
            && EXCLUDED_REGISTRATION_SOURCES
                .iter()
                .all(|(_, reason)| !reason.trim().is_empty()),
        "every registration exclusion needs an auditable reason"
    );

    let errors = registration_classification_errors(&calls);
    assert!(
        errors.is_empty(),
        "unclassified CLI registration calls:\n{}",
        errors.join("\n")
    );
}

/// **C-305.** The CLI's assembly must take the pane-surfacing decision *from the sink it is about
/// to install*, at exactly one production call site.
///
/// This is a source assertion rather than a behavioural one because the behaviour it guards is a
/// single argument inside `build_agent_with`, and `build_agent_with` cannot be called from a test:
/// it reads the process cwd, creates `~/.flux` roots, opens an event store and indexes the
/// workspace. Everything downstream of the argument is covered end to end by
/// `crates/flux-cli/tests/pane_surface_wiring.rs`; this pins the one link that suite has to
/// reconstruct instead of invoke.
///
/// It is worth its length because of what the alternative looks like: hard-coding `false` here
/// leaves the whole vocabulary inert with **every** existing test still green (verified by doing
/// it), and hard-coding `true` puts `pane.*` in every headless `flux run`, `flux-server` and SDK
/// catalog — the exact failure C-223's fail-closed seam exists to prevent. `registration_source`
/// renders a literal as `<non-string literal>`, so both mutations are caught.
///
/// The rendered argument keeps the **method**, not just its receiver, and that is load-bearing: an
/// inverted `surface_sink.is_none()` has the same receiver as the correct call and the opposite
/// meaning, and it is the mutation that fails *open*.
#[test]
fn the_pane_surfacing_decision_comes_from_the_assembling_surfaces_own_sink() {
    let (_, calls) = cli_registration_calls().expect("the CLI source tree parses");
    let sites: Vec<_> = calls
        .iter()
        .filter(|call| call.seam == "try_register_surface_ops")
        .collect();

    assert_eq!(
        sites.len(),
        1,
        "the pane vocabulary must be registered at exactly one place in the CLI assembly, found \
         {sites:?}"
    );
    let site = sites[0];
    assert_eq!(
        site.module, "execution.rs",
        "the surfacing decision belongs in the shared agent assembly, not in {}",
        site.module
    );
    assert_eq!(
        site.arguments.get(1).map(String::as_str),
        Some("surface_sink.is_some()"),
        "`try_register_surface_ops` must be told whether THIS assembly minted a `SurfaceSink` \
         (`surface_sink.is_some()`); it is currently passed {:?}, which either leaves the pane \
         vocabulary inert or advertises it to every headless catalog",
        site.arguments.get(1)
    );
}

#[test]
fn registration_scan_ignores_comments_strings_and_test_only_calls() {
    let calls = registration_calls_in_source(
        "fixture.rs",
        r##"
        fn production(registry: &mut Registry, tool: Tool) {
            // registry.try_register_from("comment pack", tool)?;
            let _text = r#"registry.try_register_from("string pack", tool)"#;
            registry.try_register_from("flux-cli cognition pack", tool);
        }

        #[cfg(test)]
        mod tests {
            fn probe(registry: &mut Registry, tool: Tool) {
                registry.try_register_from("test pack", tool);
            }
        }
        "##,
    )
    .expect("fixture parses");

    assert_eq!(
        calls,
        vec![RegistrationCall {
            module: "fixture.rs".to_string(),
            seam: "try_register_from".to_string(),
            source: Some("\"flux-cli cognition pack\"".to_string()),
            arguments: vec![
                "\"flux-cli cognition pack\"".to_string(),
                "tool".to_string()
            ],
        }]
    );
}

#[test]
fn a_fresh_app_command_source_label_is_rejected() {
    let calls = registration_calls_in_source(
        "app_cmd.rs",
        r#"
        fn assemble(registry: &mut Registry, tool: Tool) {
            registry.try_register_from("fresh app pack", tool);
        }
        "#,
    )
    .expect("fixture parses");
    let errors = registration_classification_errors(&calls);

    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].contains("app_cmd.rs")
            && errors[0].contains("fresh app pack")
            && errors[0].contains("try_register_from"),
        "unexpected classification error: {errors:?}"
    );
}

#[test]
fn a_fresh_direct_registration_identity_is_rejected() {
    let calls = registration_calls_in_source(
        "execution.rs",
        r#"
        fn assemble(registry: &mut Registry) {
            registry.try_register(FreshTool::new());
        }
        "#,
    )
    .expect("fixture parses");
    let errors = registration_classification_errors(&calls);

    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].contains("execution.rs")
            && errors[0].contains("FreshTool")
            && errors[0].contains("try_register"),
        "unexpected classification error: {errors:?}"
    );
}
