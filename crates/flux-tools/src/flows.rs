//! `flow_list` / `flow_run`: discover and run stored Flux-Lang flows & ops.
//!
//! Flows and ops live as `.flux` files under `.flux/flows` (project) and `~/.flux/flows`
//! (global — the `@global_flows` named root the CLI registers), plus the legacy
//! `.flux/ops` / `@global_ops` dirs, kept readable during the ops→flows unification.
//! `flow_list` enumerates them (flows *and* composite ops, with descriptions + params);
//! `flow_run` runs a named flow in the CURRENT session through the same depth-guarded
//! `run_plan` reentry the reflect pack uses — so it inherits the approval + IO envelope,
//! the provider, and the session, holding no engine state of its own.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_lang::ast::{DraftAst, Node, Param};
use flux_lang::program::Module;
use flux_runtime::{LoopHost, Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};
use flux_system::System;

/// Directories searched, in precedence order (project flows shadow global flows; flows shadow the
/// legacy ops homes). `@`-prefixed entries are workspace named roots, read only when registered.
const FLOW_DIRS: &[&str] = &[".flux/flows", "@global_flows", ".flux/ops", "@global_ops"];

/// The kind of declaration shown in the stored-flow catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredFlowKind {
    Flow,
    Op,
    Error,
}

impl StoredFlowKind {
    fn label(self) -> &'static str {
        match self {
            Self::Flow => "flow",
            Self::Op => "op",
            Self::Error => "error",
        }
    }
}

/// One visible declaration in a [`StoredFlowCatalog`]. The first declaration of a name wins in
/// directory precedence order, exactly as `flow_list` has always behaved.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFlowEntry {
    pub name: String,
    pub kind: StoredFlowKind,
    pub description: String,
    pub params: Vec<Param>,
    pub path: String,
}

/// A stored flow selected by filename stem or declared flow name.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedStoredFlow {
    pub path: String,
    pub source: String,
    pub ast: DraftAst,
}

#[derive(Debug, Clone)]
struct StoredFlowFile {
    dir_rank: usize,
    path: String,
    stem: String,
    source: String,
    parsed: std::result::Result<Module, String>,
}

/// The one system-backed catalog for saved flows and composite ops. It is independent of a
/// tool/agent session so CLI discovery can run without constructing a provider or event store.
#[derive(Debug, Clone)]
pub struct StoredFlowCatalog {
    files: Vec<StoredFlowFile>,
    entries: Vec<StoredFlowEntry>,
}

impl StoredFlowCatalog {
    /// Discover `.flux` files in the established precedence order. Missing directories,
    /// unregistered named roots, and unreadable directories are skipped as before; a malformed
    /// individual file remains a visible `[error]` entry.
    pub fn load(system: &System) -> Self {
        let ws = system.workspace();
        let mut files = Vec::new();
        for (dir_rank, dir) in FLOW_DIRS.iter().enumerate() {
            if let Some(root) = dir.strip_prefix('@') {
                if !ws.has_named_root(root) {
                    continue;
                }
            }
            let Ok(found) = system.read_dir_text_files(dir, "flux") else {
                continue;
            };
            files.extend(found.into_iter().map(|(path, source)| StoredFlowFile {
                dir_rank,
                stem: basename(&path),
                parsed: Module::parse_str(&source).map_err(|e| e.to_string()),
                path,
                source,
            }));
        }

        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        for file in &files {
            for entry in entries_of(file) {
                if seen.insert(entry.name.clone()) {
                    entries.push(entry);
                }
            }
        }
        Self { files, entries }
    }

    /// Visible, already-shadowed catalog entries in deterministic display order.
    pub fn entries(&self) -> &[StoredFlowEntry] {
        &self.entries
    }

    /// Render exactly the text shared by the `flow_list` tool and `flux flow list`.
    pub fn render(&self) -> String {
        if self.entries.is_empty() {
            return "no flows found — add .flux files under .flux/flows or ~/.flux/flows".into();
        }
        self.entries
            .iter()
            .map(|entry| {
                let desc = if entry.description.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", entry.description)
                };
                let params = if entry.params.is_empty() {
                    String::new()
                } else {
                    format!(
                        "  (params: {})",
                        entry
                            .params
                            .iter()
                            .map(|p| p.name.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                format!("{} [{}]{}{}", entry.name, entry.kind.label(), desc, params)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Resolve a filename stem or declared flow name. Within each directory tier, a filename stem
    /// wins over a declaration alias; directory precedence always wins across tiers. A matching
    /// op-only target is reported explicitly instead of degrading to "not found".
    pub fn resolve(&self, name: &str) -> Result<ResolvedStoredFlow> {
        for dir_rank in 0..FLOW_DIRS.len() {
            if let Some(file) = self
                .files
                .iter()
                .find(|file| file.dir_rank == dir_rank && file.stem == name)
            {
                return resolve_from_stem(file, name);
            }

            for file in self.files.iter().filter(|file| file.dir_rank == dir_rank) {
                match &file.parsed {
                    Ok(Module::Flow(ast)) if ast.name.as_deref() == Some(name) => {
                        return Ok(resolved(file, ast.clone()));
                    }
                    Ok(Module::Program(program)) => {
                        if let Some(ast) = program.flow_named(name) {
                            return Ok(resolved(file, ast.clone()));
                        }
                        if let Some(op) = program.ops.iter().find(|op| op.name == name) {
                            return Err(op_only_error(&file.path, &op.name));
                        }
                    }
                    _ => {}
                }
            }
        }
        Err(Error::Other(format!(
            "no stored flow named `{name}` under .flux/flows or ~/.flux/flows — try `flux flow list` (or `flow_list`)"
        )))
    }
}

/// Register the flow discovery/run pack. Like the reflect pack, `flow_run` is only meaningful
/// with a model-in-the-loop host installed, but it stays model-facing (unlike `run_plan`).
pub fn register_flows(registry: &mut ToolRegistry) {
    registry.register(Arc::new(FlowListTool));
    registry.register(Arc::new(FlowRunTool));
}

fn loop_host(ctx: &ToolContext) -> Result<&dyn LoopHost> {
    ctx.loop_host.as_deref().ok_or_else(|| {
        Error::Other(
            "flow_run needs a model-in-the-loop host, but none is installed in this context".into(),
        )
    })
}

fn basename(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .strip_suffix(".flux")
        .unwrap_or(path)
        .to_string()
}

/// Parse one file into its discoverable entries. An unparseable file yields a single
/// `error` entry so `flow_list` surfaces it rather than hiding it.
fn entries_of(file: &StoredFlowFile) -> Vec<StoredFlowEntry> {
    match &file.parsed {
        Ok(Module::Flow(ast)) => vec![StoredFlowEntry {
            name: ast.name.clone().unwrap_or_else(|| file.stem.clone()),
            kind: StoredFlowKind::Flow,
            description: String::new(),
            params: ast.params.clone(),
            path: file.path.clone(),
        }],
        Ok(Module::Program(program)) => {
            let mut entries = Vec::new();
            for flow in &program.flows {
                entries.push(StoredFlowEntry {
                    name: flow.name.clone().unwrap_or_else(|| file.stem.clone()),
                    kind: StoredFlowKind::Flow,
                    description: String::new(),
                    params: flow.params.clone(),
                    path: file.path.clone(),
                });
            }
            for op in &program.ops {
                entries.push(StoredFlowEntry {
                    name: op.name.clone(),
                    kind: StoredFlowKind::Op,
                    description: op.meta.description.clone(),
                    params: op.params.clone(),
                    path: file.path.clone(),
                });
            }
            entries
        }
        Err(error) => vec![StoredFlowEntry {
            name: file.stem.clone(),
            kind: StoredFlowKind::Error,
            description: format!("parse error: {error}"),
            params: Vec::new(),
            path: file.path.clone(),
        }],
    }
}

fn resolved(file: &StoredFlowFile, ast: DraftAst) -> ResolvedStoredFlow {
    ResolvedStoredFlow {
        path: file.path.clone(),
        source: file.source.clone(),
        ast,
    }
}

fn resolve_from_stem(file: &StoredFlowFile, name: &str) -> Result<ResolvedStoredFlow> {
    match &file.parsed {
        Err(error) => Err(Error::Other(format!("{}: {error}", file.path))),
        Ok(Module::Flow(ast)) => Ok(resolved(file, ast.clone())),
        Ok(Module::Program(program)) => {
            if let Some(ast) = program.flow_named(name) {
                Ok(resolved(file, ast.clone()))
            } else if let Some(ast) = program.flows.first() {
                Ok(resolved(file, ast.clone()))
            } else if let Some(op) = program.ops.first() {
                Err(op_only_error(&file.path, &op.name))
            } else {
                Err(Error::Other(format!(
                    "`{}` has no runnable flow",
                    file.path
                )))
            }
        }
    }
}

fn op_only_error(path: &str, op: &str) -> Error {
    Error::Other(format!(
        "`{path}` defines composite op `{op}` and no runnable flow — standalone ops cannot be run with `flux flow run`; call it from a flow or directly from an agent, e.g. {op}({{…}})"
    ))
}

/// Arguments for `flow_list` (none today).
#[derive(serde::Deserialize, schemars::JsonSchema, Default)]
struct FlowListInput {}

/// `flow_list()` — enumerate available flows/ops so the model can discover them.
struct FlowListTool;

#[async_trait]
impl Tool for FlowListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "flow_list".into(),
            description: "List the Flux-Lang flows and composite ops stored under .flux/flows \
                          (project) and ~/.flux/flows (global). Each line is `name [flow|op] — \
                          description  (params: …)`. Run a flow with flow_run; a listed op can be \
                          called directly by name."
                .into(),
            input_schema: flux_spec::tool_input_schema::<FlowListInput>(),
            output_schema: None,
            effects: vec![Effect::Read, Effect::Filesystem],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            StoredFlowCatalog::load(ctx.system.as_ref()).render(),
        ))
    }
}

/// Arguments for `flow_run`.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FlowRunInput {
    /// The flow to run: a filename stem under .flux/flows (e.g. "mr_update") or a declared flow name.
    name: String,
    /// Optional JSON object of inputs bound as `$key` before the run (seeded as literal binds; a
    /// flow-local bind shadows them).
    #[serde(default)]
    inputs: Option<Value>,
}

/// `flow_run(name, inputs?) -> Outcome` — run a stored flow in the current session.
struct FlowRunTool;

#[async_trait]
impl Tool for FlowRunTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "flow_run".into(),
            description: "Run a stored Flux-Lang flow by name (a file under .flux/flows or \
                          ~/.flux/flows). `inputs` are bound as `$key` before the run. The flow \
                          executes in the current session through the same approval + IO envelope \
                          as any op and returns its Outcome; bounded by a reentry-depth cap. \
                          Discover names with flow_list."
                .into(),
            input_schema: flux_spec::tool_input_schema::<FlowRunInput>(),
            output_schema: None,
            // No host effects of its own: the flow's inner ops declare and gate their own.
            effects: Vec::new(),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: Vec::new(),
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("name")
            .and_then(|v| v.as_str())
            .map(|name| vec![format!("flow:{name}")])
            .unwrap_or_default()
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: FlowRunInput = crate::parse_params(params, "flow_run")?;
        let mut ast = resolve_flow(ctx, &args.name)?;

        // Preserve the agent tool's existing compatibility semantics: arbitrary input keys are
        // literal binds and a flow-local bind can shadow them. The strict declared-param contract is
        // intentionally a CLI-only policy.
        if let Some(inputs) = args.inputs {
            let obj = inputs
                .as_object()
                .ok_or_else(|| Error::Other("flow_run: `inputs` must be a JSON object".into()))?;
            let mut seeded: Vec<Node> = obj
                .iter()
                .map(|(key, value)| Node::Bind {
                    name: key.clone().into(),
                    value: Box::new(Node::Lit {
                        value: value.clone(),
                    }),
                    ty: None,
                    effect: None,
                })
                .collect();
            seeded.append(&mut ast.body);
            ast.body = seeded;
        }

        let ast_json = serde_json::to_value(&ast)
            .map_err(|e| Error::Other(format!("flow_run: serialize flow: {e}")))?;
        let plan = json!({ "kind": "plan", "ast": ast_json, "complete": true });
        let outcome = loop_host(ctx)?.run_plan(plan).await?;
        Ok(ToolResult::ok(
            serde_json::to_string(&outcome).unwrap_or_default(),
        ))
    }
}

fn resolve_flow(ctx: &ToolContext, name: &str) -> Result<DraftAst> {
    StoredFlowCatalog::load(ctx.system.as_ref())
        .resolve(name)
        .map(|resolved| resolved.ast)
        .map_err(|e| Error::Other(format!("flow_run: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::Workspace;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("flux-stored-catalog-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> (TempDir, System) {
        let temp = TempDir::new();
        let project = temp.path().join("project");
        let global = temp.path().join("global-flows");
        std::fs::create_dir_all(project.join(".flux/flows")).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        let mut workspace = Workspace::new(&project).unwrap();
        workspace.add_named_root("global_flows", &global).unwrap();
        (temp, System::new(workspace))
    }

    fn write(base: &Path, path: &str, source: &str) {
        std::fs::write(base.join(path), source).unwrap();
    }

    #[test]
    fn project_declarations_shadow_global_ones() {
        let (temp, system) = fixture();
        let project = temp.path().join("project");
        let global = temp.path().join("global-flows");
        write(
            &project,
            ".flux/flows/project.flux",
            "flow shared(value: String)\n  return \"project\"\n",
        );
        write(
            &global,
            "global.flux",
            "flow shared(value: Number)\n  return \"global\"\n",
        );

        let catalog = StoredFlowCatalog::load(&system);
        let shared: Vec<_> = catalog
            .entries()
            .iter()
            .filter(|entry| entry.name == "shared")
            .collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].params[0].ty, flux_lang::ast::TypeRef::String);
        let resolved = catalog.resolve("shared").unwrap();
        assert_eq!(resolved.path, ".flux/flows/project.flux");
    }

    #[test]
    fn filename_stems_and_declared_names_resolve_to_the_same_flow() {
        let (temp, system) = fixture();
        let project = temp.path().join("project");
        write(
            &project,
            ".flux/flows/by-file.flux",
            "flow by-declaration(name: String)\n  return $name\n",
        );

        let catalog = StoredFlowCatalog::load(&system);
        let by_stem = catalog.resolve("by-file").unwrap();
        let by_name = catalog.resolve("by-declaration").unwrap();
        assert_eq!(by_stem.path, by_name.path);
        assert_eq!(by_stem.ast, by_name.ast);
    }

    #[test]
    fn malformed_files_are_listed_and_fail_resolution() {
        let (temp, system) = fixture();
        let project = temp.path().join("project");
        write(&project, ".flux/flows/broken.flux", "flow broken( ((((\n");

        let catalog = StoredFlowCatalog::load(&system);
        let entry = catalog
            .entries()
            .iter()
            .find(|entry| entry.name == "broken")
            .unwrap();
        assert_eq!(entry.kind, StoredFlowKind::Error);
        assert!(entry.description.starts_with("parse error:"));
        assert!(catalog.render().contains("broken [error] — parse error:"));
        let error = catalog.resolve("broken").unwrap_err().to_string();
        assert!(error.contains(".flux/flows/broken.flux"), "{error}");
    }

    #[test]
    fn op_only_stems_and_declared_names_get_an_actionable_error() {
        let (temp, system) = fixture();
        let project = temp.path().join("project");
        write(
            &project,
            ".flux/flows/helpers.flux",
            "op greet(name: String) -> String\n  return fmt(\"hi {name}\")\n",
        );

        let catalog = StoredFlowCatalog::load(&system);
        for target in ["helpers", "greet"] {
            let error = catalog.resolve(target).unwrap_err().to_string();
            assert!(error.contains("composite op `greet`"), "{target}: {error}");
            assert!(error.contains("call it from a flow"), "{target}: {error}");
        }
    }
}
