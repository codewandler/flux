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
//! `Executor::dispatch` and the same `RiskApprover`, so all of them are gated by what they declare
//! about themselves. When this gate was first run it raised **22 violations across 19 operations** —
//! the eleven the story had itemised, plus `explore` and `grade`, which nobody had found because
//! nothing had ever walked this catalog.
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
//! reads `execution.rs` and fails on any registration call it has not been told about, which forces
//! a new pack to be either added to the census or explicitly excluded with a reason.
//!
//! # Posture
//!
//! The eight `Network`-at-`Risk::Low`-without-`Read` violations were not a mechanical fix, and the
//! decision that resolved them (Group A gains `Read`; Group B — the billable model calls — rises to
//! `Risk::Medium`) is recorded in `docs/designs/security-assurance.md`. Read that before changing
//! any declaration this gate covers.

use std::sync::Arc;

use flux_runtime::ToolRegistry;
use flux_spec::metadata_violations;

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
    registry
        .try_register_from(
            "flux-cli sub-agent task operation",
            Arc::new(flux_orchestrate::TaskTool),
        )
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
        ("census_stage", "config-authored model stages"),
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

/// Drift guard. A census assembled by hand goes stale the moment a pack is added to
/// `build_agent_with` and not here, and it goes stale *silently* — the gate keeps passing while
/// covering less. So the registration seams themselves are enumerated from the source: every
/// distinct `try_register*` call in `execution.rs`'s production body must be classified, either as
/// one the census drives or as one deliberately out of scope.
///
/// Adding a pack therefore fails this test until someone states which it is. That is the whole
/// mechanism: the C-191 review's own lesson was that crate proximity does not imply coverage.
#[test]
fn every_registration_seam_in_the_cli_assembly_is_classified() {
    /// Seams the census drives, directly or through `register_tool_packs`.
    const COVERED: &[&str] = &[
        "try_register_builtins",
        "try_register_dev_builtins",
        "try_register_eval_ops",
        "try_register_reflect",
        "try_register_flows",
        "try_register_render",
        "try_register_datasource_ops",
        "try_register_web",
        "try_register_model_stage",
        // The generic registry methods. `try_register_from`/`try_register_all_from` are how
        // `TaskTool`, the endpoint ops and every plugin op arrive; the census drives the first two
        // and excludes the third below. `try_register` is the pack-owned shorthand
        // (`ConsultTool`, `WakeupTool`), both reached through `register_tool_packs`.
        "try_register_from",
        "try_register_all_from",
        "try_register",
    ];
    /// Seams deliberately outside this gate, with the reason.
    const EXCLUDED: &[(&str, &str)] = &[(
        "try_register_op",
        "flux-sdk's embedder seam: ops authored outside this repo, with no compile-time list to \
         walk. Third-party metadata is checked where it crosses the trust boundary — the plugin \
         loader's `op_coherence_warnings` — not at a registration call.",
    )];

    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/execution.rs"),
    )
    .expect("execution.rs is readable");
    // Only the production body. `execution.rs` carries several inline `#[cfg(test)] mod` blocks,
    // and those register probe tools that are deliberately not part of any catalog — a `Risk::Low`
    // write is registered on purpose to prove `RiskApprover` still gates it. Blocks are skipped by
    // brace column, which holds because every top-level item in this repo is rustfmt-formatted.
    let mut seen: Vec<&str> = Vec::new();
    let mut in_test_module = false;
    for line in source.lines() {
        if in_test_module {
            in_test_module = line != "}";
            continue;
        }
        if line.trim() == "#[cfg(test)]" {
            in_test_module = true;
            continue;
        }
        let mut rest = line;
        while let Some(at) = rest.find("try_register") {
            let tail = &rest[at..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(tail.len());
            let ident = &tail[..end];
            if !seen.contains(&ident) {
                seen.push(ident);
            }
            rest = &tail[end..];
        }
    }

    assert!(
        seen.len() >= 8,
        "only {} registration seam(s) found in execution.rs — the scan probably stopped matching, \
         and this test would pass vacuously: {seen:?}",
        seen.len()
    );
    let unclassified: Vec<&&str> = seen
        .iter()
        .filter(|ident| {
            !COVERED.contains(*ident) && !EXCLUDED.iter().any(|(name, _)| name == *ident)
        })
        .collect();
    assert!(
        unclassified.is_empty(),
        "unclassified registration seam(s) in flux-cli's assembly: {unclassified:?}. Add each to \
         the census in `production_catalog` (and to COVERED), or to EXCLUDED with the reason it is \
         out of scope. A pack that registers ops the coherence gate never walks is exactly the gap \
         C-208 closed."
    );
}
