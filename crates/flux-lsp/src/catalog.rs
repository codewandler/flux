//! `catalog` — the op catalog Flux-Lang authors see in diagnostics, completion, and hover.
//!
//! Three layers, in resolution order:
//! 1. the **host built-ins** (`flux-tools` + cognition + datasource + web) — the same registry the
//!    CLI installs, built once at startup;
//! 2. the **workspace composites** the host loads from disk (`.flux/flows`, `.flux/ops`, and their
//!    global twins) via `flux_flow::composites::DynamicComposites` — L-89. Without these the editor
//!    reported "unknown operation" for a composite that `flux flow run` executes happily;
//! 3. the **module-local composites** declared in the buffer being edited.
//!
//! The invariant the first epic set still holds: **the LSP is a reader**. None of these
//! constructors performs model, network, or credential IO. Layer 2 is the epic's only new IO — it is
//! read-only, goes through `flux_system::System`, runs once when the workspace root is known, and
//! refreshes on `didSave` rather than per keystroke.

use std::collections::HashSet;
use std::sync::Arc;

use flux_lang::opspec::OpSignature;
use flux_lang::program::{CompositeOpDecl, Module};

/// The base host registry: built-ins, cognition, datasource, and web.
///
/// Kept as a helper so tests exercise the exact registry the server installs rather than a
/// hand-built approximation.
pub fn authoring_registry() -> flux_runtime::ToolRegistry {
    let mut reg = flux_runtime::ToolRegistry::new();
    flux_tools::try_register_builtins(&mut reg)
        .expect("flux-lsp built-in authoring catalog registration failed");

    // Catalog-only registrations: none of these constructors performs IO. The provider never
    // generates, the datasource is empty and in-memory, and WebOptions::default is public-only with
    // no audit/record sink. Execution still belongs to the real host; the LSP only reads specs.
    flux_cognition::CognitionPack::new(Arc::new(flux_provider::NullProvider), "flux-lsp")
        .try_register_from("flux-lsp cognition authoring catalog", &mut reg)
        .expect("flux-lsp cognition authoring catalog registration failed");
    flux_capabilities::try_register_datasource_ops(
        &mut reg,
        Arc::new(flux_capabilities::MemoryBackend::new()),
    )
    .expect("flux-lsp datasource authoring catalog registration failed");
    flux_web::try_register_web(&mut reg, &flux_web::WebOptions::default())
        .expect("flux-lsp web authoring catalog registration failed");
    reg
}

/// The composite ops the host would install from the workspace flow home.
///
/// Lenient by contract, matching `load_flows_dir`: a workspace we cannot open, or a `.flux` file
/// that does not parse, yields fewer composites — never an error that would take the editor's
/// diagnostics down with it.
pub fn workspace_composites(root: &std::path::Path) -> Vec<CompositeOpDecl> {
    let Ok(workspace) = flux_system::Workspace::new(root) else {
        return Vec::new();
    };
    let system = flux_system::System::new(workspace);
    match flux_flow::composites::DynamicComposites::load(&system) {
        Ok(loaded) => loaded.active_for_session(""),
        Err(_) => Vec::new(),
    }
}

pub fn composite_signature(op: &CompositeOpDecl) -> OpSignature {
    let param_types = op
        .params
        .iter()
        .map(|param| (param.name.0.clone(), param.ty.clone()))
        .collect();
    OpSignature {
        name: op.name.clone(),
        description: op.meta.description.clone(),
        effects: op.meta.effects.clone(),
        risk: op.meta.risk,
        idempotency: op.meta.idempotency,
        required_params: op.params.iter().map(|param| param.name.0.clone()).collect(),
        optional_params: Vec::new(),
        param_types,
        output: op.returns.clone().unwrap_or(flux_lang::ast::TypeRef::Any),
        // Composite ops don't yet declare their own semantic-effect tier (D-138 scopes catalog
        // semantics to leaf ops); see `flux_flow::registry::composite_signature`.
        semantic_effects: Vec::new(),
    }
}

/// The composite ops declared in the buffer itself. Local declarations participate in authoring
/// even when `expose false`: exposure controls planner advertising, not whether another declaration
/// in the same module may call the op.
/// Lowered from the *cached* tree — never from a fresh parse of the text (L-90).
pub fn document_composites(parse: &flux_lang::parser::Parse) -> Vec<CompositeOpDecl> {
    match flux_lang::lower_cst::cst_to_module(parse) {
        Ok(lowered) => match lowered.module {
            Module::Program(program) => program.ops,
            Module::Flow(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

/// Base host ops plus the workspace composites plus the buffer's own, sorted by name and
/// de-duplicated with the nearest declaration winning.
pub fn signatures_for(
    base: &[OpSignature],
    workspace: &[CompositeOpDecl],
    document: &[CompositeOpDecl],
) -> Vec<OpSignature> {
    let mut ops = base.to_vec();
    let mut known: HashSet<String> = ops.iter().map(|op| op.name.clone()).collect();
    for op in document.iter().chain(workspace.iter()) {
        if known.insert(op.name.clone()) {
            ops.push(composite_signature(op));
        }
    }
    ops.sort_by(|a, b| a.name.cmp(&b.name));
    ops
}

#[cfg(test)]
pub fn authoring_op_signatures() -> Vec<OpSignature> {
    let reg = authoring_registry();
    flux_flow::registry::OpRegistry::new(&reg).signatures()
}

/// A throwaway workspace root for tests — the repo builds these by hand rather than pulling in a
/// dev-dependency (see `flux_flow::staged`'s test helper).
#[cfg(test)]
pub struct TempWorkspace(std::path::PathBuf);

#[cfg(test)]
impl TempWorkspace {
    /// Create `<tmp>/<label>-<pid>-<n>/.flux/flows` and write each `(file, contents)` into it.
    pub fn with_flows(label: &str, files: &[(&str, &str)]) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::SeqCst)
        ));
        let flows = root.join(".flux/flows");
        std::fs::create_dir_all(&flows).expect("create the flow home");
        for (name, contents) in files {
            std::fs::write(flows.join(name), contents).expect("write a flow file");
        }
        TempWorkspace(root)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal, complete composite op — the shape `load_flows_dir` accepts.
    pub const SHOUT: &str = "op shout(text: String) -> String\n  description \"Shout\"\n  risk \"low\"\n  idempotency \"idempotent\"\n  return $text\n";

    #[test]
    fn authoring_catalog_contains_stable_cli_host_ops() {
        let names: HashSet<String> = authoring_op_signatures()
            .into_iter()
            .map(|op| op.name)
            .collect();
        for required in [
            "ai.extract",
            "ai.rank",
            "ai.reason",
            "synth",
            "search",
            "sources",
            "http.request",
            "web.fetch",
        ] {
            assert!(
                names.contains(required),
                "LSP authoring catalog is missing stable CLI op `{required}`"
            );
        }
    }

    #[test]
    fn document_signatures_include_unexposed_local_composites() {
        let src = "op internal(value: String) -> String\n  expose false\n  return $value\n\nflow f\n  return internal(\"x\")\n";
        let parse = flux_lang::parser::parse_cst(src);
        let signatures = signatures_for(
            &authoring_op_signatures(),
            &[],
            &document_composites(&parse),
        );
        assert!(signatures.iter().any(|op| op.name == "internal"));
    }

    #[test]
    fn workspace_composites_come_from_the_flow_home() {
        let workspace = TempWorkspace::with_flows(
            "flux-lsp-catalog",
            &[
                ("shout.flux", SHOUT),
                // A file that does not parse must be skipped, not fatal (`load_flows_dir` is lenient).
                ("broken.flux", "op ??? not flux at all\n"),
            ],
        );
        let composites = workspace_composites(workspace.path());
        assert!(
            composites.iter().any(|op| op.name == "shout"),
            "the workspace composite is discovered: {:?}",
            composites.iter().map(|op| &op.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_unreadable_workspace_yields_an_empty_catalog_layer() {
        let missing = std::path::Path::new("/definitely/not/a/workspace/anywhere");
        assert!(workspace_composites(missing).is_empty());
    }
}
