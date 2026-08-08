//! `flux ops` — the operation-catalog command group. First tenant: `--explore`.
//!
//! This module is the *assembly* half of the C-643 seam. `flux-tui` renders a `Vec<OpRow>` and
//! knows nothing about registries; everything registry-shaped lives here, so iteration 2 can add
//! plugin-projected rows and iteration 4 connector rows without the TUI learning a second source.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use flux_runtime::ToolRegistry;
use flux_spec::ToolSpec;
use flux_tui::explorer::{OpRow, OpsExplorerOptions, ParamRow};

/// Generated map of op name → website page ids. See `tests/ops_doc_index.rs`, which regenerates and
/// drift-checks it; never hand-edit.
pub(super) const OPS_DOCS_JSON: &str = include_str!("../assets/ops_docs.json");

const PUBLIC_DOCS_BASE: &str = "https://codewandler.github.io/flux/docs";
/// The `flux docs` loopback server's mount point. Labelled in the UI as requiring that server.
const LOCAL_DOCS_BASE: &str = "http://127.0.0.1:8788/flux/docs";
/// Where a name with no better page belongs: the complete hand-maintained op reference.
pub(super) const FALLBACK_PAGE: &str = "language/ops";

pub(super) fn run_ops(explore: bool) -> Result<()> {
    if !explore {
        // Bare `flux ops` is the group's help, not an implicit action. The group exists so
        // `flux ops list --json` can join it later without a breaking change.
        println!("{}", ops_help());
        return Ok(());
    }
    let rows = build_op_rows()?;
    flux_tui::explorer::run_ops_explorer(
        rows,
        OpsExplorerOptions {
            theme: flux_tui::detect_theme(None),
            seed: seed_from_clock(),
        },
    )
}

fn ops_help() -> String {
    "flux ops — browse the operation catalog\n\
     \n\
     Usage: flux ops [OPTIONS]\n\
     \n\
     Options:\n\
     \x20     --explore    Open the full-screen catalog explorer (requires a real terminal)\n\
     \n\
     The operation is Flux's one universal callable unit. `--explore` opens a search-first browser\n\
     over the built-in and web registries with descriptions, parameters, risk and doc links."
        .to_string()
}

/// Non-deterministic only in the animation seed; nothing else in the surface depends on it.
fn seed_from_clock() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xC643)
}

/// The registry recipe. Deliberately the same one `build_core_catalog` uses, so the explorer and
/// the exported catalogue can never disagree about what is registered.
pub(super) fn core_registry() -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    flux_tools::try_register_builtins(&mut registry)
        .context("register built-in operations for the ops explorer")?;
    flux_web::try_register_web(&mut registry, &flux_web::WebOptions::default())
        .context("register web operations for the ops explorer")?;
    Ok(registry)
}

pub(super) fn build_op_rows() -> Result<Vec<OpRow>> {
    let registry = core_registry()?;
    let docs: BTreeMap<String, Vec<String>> =
        serde_json::from_str(OPS_DOCS_JSON).context("parse the generated ops doc index")?;
    let groups = flux_tools::groups::builtin_groups();
    // `specs()` is name-sorted, and that order is the empty-query order in the explorer.
    Ok(registry
        .specs()
        .into_iter()
        .map(|spec| {
            let page = docs
                .get(&spec.name)
                .and_then(|pages| pages.first())
                .map(String::as_str)
                .unwrap_or(FALLBACK_PAGE);
            OpRow {
                category: categorize(&spec, &groups),
                params: params_of(&spec),
                doc_public_url: doc_url(PUBLIC_DOCS_BASE, page, &spec.name),
                doc_local_url: doc_url(LOCAL_DOCS_BASE, page, &spec.name),
                source: registry.source(&spec.name).unwrap_or("builtin").to_string(),
                name: spec.name.clone(),
                description: spec.description.clone(),
                effects: spec.effects.clone(),
                risk: spec.risk,
                idempotency: spec.idempotency,
                group: spec.group.clone(),
            }
        })
        .collect())
}

fn doc_url(base: &str, page: &str, name: &str) -> String {
    format!("{base}/{page}#{name}")
}

/// Parameters, from the op's input schema.
fn params_of(spec: &ToolSpec) -> Vec<ParamRow> {
    let (required, optional) = flux_lang::opspec::schema_params(&spec.input_schema);
    let props = spec
        .input_schema
        .get("properties")
        .and_then(|v| v.as_object());
    let describe = |name: &str, required: bool| {
        let prop = props.and_then(|p| p.get(name));
        ParamRow {
            name: name.to_string(),
            ty: prop
                .and_then(|p| p.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            description: prop
                .and_then(|p| p.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            required,
        }
    };
    required
        .iter()
        .map(|n| describe(n, true))
        .chain(optional.iter().map(|n| describe(n, false)))
        .collect()
}

/// File-shaped ops that carry no group and no dotted prefix, so nothing else would classify them.
const FILE_OPS: &[&str] = &[
    "read",
    "write",
    "edit",
    "glob",
    "grep",
    "ls",
    "multi_edit",
    "notebook_edit",
];

/// Derive an op's display category.
///
/// Strict resolution order, and no new grouping mechanism: (1) the canonical group resolver, which
/// is what the runtime itself uses to decide surfacing; (2) an established dotted-prefix family;
/// (3) the small file-ops set; (4) core. Every step defers to something that already exists, so the
/// explorer cannot drift from the runtime's own idea of what an op belongs to.
pub(super) fn categorize(spec: &ToolSpec, groups: &[flux_evidence::ToolGroup]) -> String {
    if let Some(group) = flux_runtime::effective_group(spec, groups) {
        return group.to_string();
    }
    if let Some((prefix, _)) = spec.name.split_once('.') {
        return prefix.to_string();
    }
    if let Some((prefix, _)) = spec.name.split_once('_') {
        // `web_fetch`, `http_request`, `review_*`, `skill_*`, `pane_*` are the same convention the
        // tool-disable globs already accept, just spelled with an underscore.
        if matches!(
            prefix,
            "web" | "http" | "review" | "skill" | "pane" | "browser"
        ) {
            return prefix.to_string();
        }
    }
    if FILE_OPS.contains(&spec.name.as_str()) {
        return "files".to_string();
    }
    "core".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The category resolver is the one place the explorer could invent a taxonomy, so pin a
    /// representative of every branch. A new grouping mechanism shows up here as a surprise.
    #[test]
    fn categorize_representatives() {
        let registry = core_registry().expect("build the core registry");
        let groups = flux_tools::groups::builtin_groups();
        let specs: BTreeMap<String, ToolSpec> = registry
            .specs()
            .into_iter()
            .map(|s| (s.name.clone(), s))
            .collect();

        // (op, expected category) — between them these exercise every arm of the resolver:
        // canonical group (git/rust/shell/cognition/browser), dotted-prefix family (web/http),
        // the file-ops set, and the core fallthrough.
        let expectations: &[(&str, &str)] = &[
            ("git_status", "git"),
            ("cargo_check", "rust"),
            ("bash", "shell"),
            ("map", "cognition"),
            ("browser.open", "browser"),
            ("web.fetch", "web"),
            ("http.request", "http"),
            ("read", "files"),
            ("write", "files"),
            ("glob", "files"),
            ("now", "core"),
        ];
        for (name, want) in expectations {
            let Some(spec) = specs.get(*name) else {
                // The registry is a moving target; a missing representative is a signal, not a
                // silent skip.
                panic!("representative op `{name}` is not registered — update this test");
            };
            assert_eq!(
                categorize(spec, &groups),
                *want,
                "`{name}` categorized wrong"
            );
        }

        // `endpoint` is a real builtin group with no op registered by *this* iteration's registry
        // (builtins + web), so there is no live representative to pin. Exercise the group arm with
        // a synthetic spec instead of skipping it — the arm is what matters, and a plugin- or
        // connector-projected endpoint op in iteration 2/4 must land here rather than in `core`.
        let endpoint_op = groups
            .iter()
            .find(|g| g.name == "endpoint")
            .and_then(|g| g.tools.first().cloned())
            .expect("the endpoint group still exists and names at least one op");
        let synthetic = ToolSpec::read_only(
            endpoint_op.clone(),
            "synthetic stand-in for an unregistered endpoint op",
            serde_json::json!({"type": "object"}),
        );
        assert_eq!(
            categorize(&synthetic, &groups),
            "endpoint",
            "`{endpoint_op}` must resolve through the canonical group resolver"
        );

        // Every registered op lands somewhere; nothing falls through to an empty string.
        for spec in registry.specs() {
            let category = categorize(&spec, &groups);
            assert!(
                !category.is_empty(),
                "`{}` produced an empty category",
                spec.name
            );
        }
    }

    /// Both URLs point at the same page and anchor, and differ only in host — the local one is the
    /// `flux docs` server, which is why the UI labels it.
    #[test]
    fn doc_urls_derive_from_one_page_id() {
        assert_eq!(
            doc_url(PUBLIC_DOCS_BASE, "language/ops", "read"),
            "https://codewandler.github.io/flux/docs/language/ops#read"
        );
        assert_eq!(
            doc_url(LOCAL_DOCS_BASE, "language/ops", "read"),
            "http://127.0.0.1:8788/flux/docs/language/ops#read"
        );
    }

    /// The committed index must cover the live registry: this is the cheap in-crate half of the
    /// contract that `tests/ops_doc_index.rs` enforces in full.
    #[test]
    fn every_registered_op_has_a_doc_entry() {
        let docs: BTreeMap<String, Vec<String>> =
            serde_json::from_str(OPS_DOCS_JSON).expect("the committed doc index parses");
        let registry = core_registry().expect("build the core registry");
        let missing: Vec<String> = registry
            .specs()
            .into_iter()
            .map(|s| s.name)
            .filter(|n| !docs.contains_key(n))
            .collect();
        assert!(
            missing.is_empty(),
            "the committed ops doc index is stale; regenerate with \
             `FLUX_UPDATE_GOLDEN=1 cargo test -p flux-cli --test ops_doc_index`. Missing: {missing:?}"
        );
    }
}
