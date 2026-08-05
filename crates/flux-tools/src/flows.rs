//! `flow_list` / `flow_run`: discover and run stored Flux-Lang flows & ops.
//!
//! Flows and ops live as `.flux` files under `.flux/flows` (project) and `~/.flux/flows`
//! (global — the `@global_flows` named root the CLI registers), plus the legacy
//! `.flux/ops` / `@global_ops` dirs, kept readable during the ops→flows unification.
//! `flow_list` enumerates them (flows *and* composite ops, with descriptions + params);
//! `flow_run` runs a named stored flow, a workspace-relative `.flux` path, or Flux source supplied
//! directly as an inline program in the CURRENT session through the engine's depth-guarded authored
//! flow host. Path-addressed source is reread for every call; all forms inherit the approval + IO
//! envelope, provider, session, and current operation catalog while holding no engine state of their
//! own.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_lang::ast::{DraftAst, Node, Param};
use flux_lang::program::Module;
use flux_runtime::{LoopHost, OperationPlacement, Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};
use flux_system::{PathAccess, System};

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

/// Register the flow discovery/run pack. `flow_run` needs the model-in-the-loop host for nested
/// authored execution and remains model-facing.
pub fn try_register_flows(registry: &mut ToolRegistry) -> Result<()> {
    registry.try_register_all_from_with_placement(
        "flux-tools stored-flow pack",
        vec![
            Arc::new(FlowListTool) as Arc<dyn Tool>,
            Arc::new(FlowRunTool),
        ],
        OperationPlacement::LocalControlPlane,
    )
}

/// Compatibility wrapper for pre-fallible pack installers.
///
/// # Deprecated
///
/// Production assembly should call [`try_register_flows`].
pub fn register_flows(registry: &mut ToolRegistry) {
    try_register_flows(registry).expect("flux-tools stored-flow pack registration failed");
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
            StoredFlowCatalog::load(ctx.system().as_ref()).render(),
        ))
    }
}

/// Arguments for `flow_run`.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FlowRunInput {
    /// A filename stem under .flux/flows (e.g. "mr_update") or a declared stored-flow name.
    #[serde(default)]
    name: Option<String>,
    /// A workspace-relative `.flux` file path, read afresh for this call (e.g.
    /// "examples/review.flux").
    #[serde(default)]
    path: Option<String>,
    /// Flux-Lang source supplied directly for this call. Exactly one of `name`, `path`, or
    /// `inline_program` is required.
    #[serde(default)]
    inline_program: Option<String>,
    /// Optional JSON object of inputs bound as `$key` before the run (seeded as literal binds; a
    /// flow-local bind shadows them).
    #[serde(default)]
    inputs: Option<Value>,
}

/// `flow_run(name | path | inline_program, inputs?) -> Outcome + route receipt` — run a flow in this
/// session.
struct FlowRunTool;

#[async_trait]
impl Tool for FlowRunTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "flow_run".into(),
            description:
                "Run a Flux-Lang flow by exactly one address: `name` selects a stored flow under \
                 .flux/flows or ~/.flux/flows; `path` selects a workspace-relative .flux file such \
                 as examples/review.flux and rereads it on every call; `inline_program` parses Flux \
                 source supplied directly in the request. `inputs` are bound as `$key` before the \
                 run. The flow executes in the current session through the same approval + IO \
                 envelope, is revalidated against the current live operation catalog, and returns \
                 an Outcome with a route receipt; bounded by a reentry-depth cap. Discover stored \
                 names with flow_list."
                    .into(),
            input_schema: flux_spec::tool_input_schema::<FlowRunInput>(),
            output_schema: None,
            // Stored addresses read Flux source; every inner operation still declares and gates its
            // own effects independently when the authored flow re-enters the loop host.
            effects: vec![Effect::Read, Effect::Filesystem],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        if let Some(path) = params.get("path").and_then(Value::as_str) {
            vec![path.to_string()]
        } else {
            params
                .get("name")
                .and_then(Value::as_str)
                .map(|name| vec![format!("flow:{name}")])
                .unwrap_or_default()
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: FlowRunInput = crate::parse_params(params, "flow_run")?;
        let resolved = resolve_flow(ctx, &args).await?;
        let resolved_path = resolved.resolved_path;
        let mut ast = resolved.ast;
        let flow_name = ast
            .name
            .clone()
            .or_else(|| resolved_path.as_deref().map(basename))
            .unwrap_or_else(|| "inline".into());
        let mut seeded_input_keys = Vec::new();

        // Preserve the agent tool's existing compatibility semantics: arbitrary input keys are
        // literal binds and a flow-local bind can shadow them. The strict declared-param contract is
        // intentionally a CLI-only policy.
        if let Some(inputs) = args.inputs {
            let obj = inputs
                .as_object()
                .ok_or_else(|| Error::Other("flow_run: `inputs` must be a JSON object".into()))?;
            seeded_input_keys.extend(obj.keys().cloned());
            seeded_input_keys.sort();
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
        let outcome = loop_host(ctx)?.run_authored_flow(ast_json).await?;
        let route = serde_json::json!({
            "operation": "flow_run",
            "resolved_path": resolved_path,
            "flow_name": flow_name,
            "seeded_input_keys": seeded_input_keys,
        });
        let with_receipt = match outcome {
            Value::Object(mut object) => {
                object.insert("route".into(), route);
                Value::Object(object)
            }
            other => serde_json::json!({"outcome": other, "route": route}),
        };
        let content = serde_json::to_string(&with_receipt)
            .map_err(|e| Error::Other(format!("flow_run: serialize outcome: {e}")))?;
        Ok(ToolResult::ok(content))
    }
}

struct ResolvedFlowAddress {
    resolved_path: Option<String>,
    ast: DraftAst,
}

async fn resolve_flow(ctx: &ToolContext, args: &FlowRunInput) -> Result<ResolvedFlowAddress> {
    match (
        args.name.as_deref(),
        args.path.as_deref(),
        args.inline_program.as_deref(),
    ) {
        (Some(name), None, None) => {
            let resolved = StoredFlowCatalog::load(ctx.system().as_ref())
                .resolve(name)
                .map_err(|e| Error::Other(format!("flow_run: {e}")))?;
            Ok(ResolvedFlowAddress {
                resolved_path: Some(resolved.path),
                ast: resolved.ast,
            })
        }
        (None, Some(path), None) => {
            let resolved = resolve_workspace_flow_path(ctx.system().as_ref(), path).await?;
            Ok(ResolvedFlowAddress {
                resolved_path: Some(resolved.path),
                ast: resolved.ast,
            })
        }
        (None, None, Some(source)) => Ok(ResolvedFlowAddress {
            resolved_path: None,
            ast: parse_runnable_flow(source, "`inline_program`")?,
        }),
        _ => Err(Error::Other(
            "flow_run: provide exactly one of `name`, `path`, or `inline_program`".into(),
        )),
    }
}

async fn resolve_workspace_flow_path(system: &System, path: &str) -> Result<ResolvedStoredFlow> {
    if path.is_empty()
        || std::path::Path::new(path).is_absolute()
        || path.starts_with('@')
        || !path.ends_with(".flux")
    {
        return Err(Error::Other(format!(
            "flow_run: `path` must be a workspace-relative .flux file, got {path:?}"
        )));
    }
    let resolved_path = system
        .workspace()
        .path_identity(path, PathAccess::Read)
        .map_err(|e| Error::Other(format!("flow_run: {e}")))?;
    if std::path::Path::new(&resolved_path).is_absolute() {
        return Err(Error::Other(format!(
            "flow_run: path {path:?} resolves outside the primary workspace"
        )));
    }
    let source = system
        .read_file(&resolved_path)
        .await
        .map_err(|e| Error::Other(format!("flow_run: read {resolved_path}: {e}")))?;
    let ast = parse_runnable_flow(&source, &resolved_path)?;
    Ok(ResolvedStoredFlow {
        path: resolved_path,
        source,
        ast,
    })
}

fn parse_runnable_flow(source: &str, label: &str) -> Result<DraftAst> {
    let module = Module::parse_str(source)
        .map_err(|e| Error::Other(["flow_run: parse ", label, ": ", &e.to_string()].concat()))?;
    match module {
        Module::Flow(ast) => Ok(ast),
        Module::Program(program) => match (program.flows.as_slice(), program.journeys.as_slice()) {
            ([flow], []) => Ok(flow.clone()),
            ([], [journey]) => Ok(journey.flow.clone()),
            _ => Err(Error::Other(
                [
                    "flow_run: ",
                    label,
                    " needs a bare flow or a module with exactly one flow/journey",
                ]
                .concat(),
            )),
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::Workspace;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

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
        (
            temp,
            System::new(workspace)
                .with_worktree_base(crate::test_worktrees::pinned_worktree_base()),
        )
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

    #[derive(Default)]
    struct CapturingLoopHost {
        asts: Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl LoopHost for CapturingLoopHost {
        async fn run_authored_flow(&self, ast: Value) -> Result<Value> {
            self.asts.lock().unwrap().push(ast);
            Ok(serde_json::json!({
                "result": "ran",
                "transcript": [],
                "steps": 1,
                "suspension": null,
            }))
        }
    }

    fn flow_run_context(system: System, host: Arc<CapturingLoopHost>) -> ToolContext {
        let mut ctx = ToolContext::new(Arc::new(system));
        ctx.loop_host = Some(host);
        ctx
    }

    /// C-376 failing-first: the model-facing operation must address a literal workspace flow path,
    /// reread it on every call, and say exactly which route ran. Before C-376, `path` is rejected by
    /// `FlowRunInput`'s `deny_unknown_fields`, so `examples/review.flux` is CLI-only.
    #[tokio::test]
    async fn flow_run_workspace_path_is_fresh_and_returns_a_route_receipt() {
        let (temp, system) = fixture();
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join("examples")).unwrap();
        let path = project.join("examples/fresh.flux");
        std::fs::write(&path, "flow first\n  return \"one\"\n").unwrap();

        let host = Arc::new(CapturingLoopHost::default());
        let ctx = flow_run_context(system, host.clone());
        let first = FlowRunTool
            .execute(
                &ctx,
                serde_json::json!({
                    "path": "examples/fresh.flux",
                    "inputs": {"z": 1, "a": 2},
                }),
            )
            .await
            .expect("workspace path should run");
        let first: Value = serde_json::from_str(&first.content).unwrap();
        assert_eq!(
            first["route"],
            serde_json::json!({
                "operation": "flow_run",
                "resolved_path": "examples/fresh.flux",
                "flow_name": "first",
                "seeded_input_keys": ["a", "z"],
            })
        );

        std::fs::write(&path, "flow updated\n  return \"two\"\n").unwrap();
        let second = FlowRunTool
            .execute(&ctx, serde_json::json!({"path": "examples/fresh.flux"}))
            .await
            .expect("updated workspace path should run");
        let second: Value = serde_json::from_str(&second.content).unwrap();
        assert_eq!(second["route"]["flow_name"], "updated");

        let asts = host.asts.lock().unwrap();
        assert_eq!(asts.len(), 2);
        assert_eq!(asts[0]["name"], "first");
        assert_eq!(asts[1]["name"], "updated");
    }

    /// Inline source is a third address form: it is parsed without filesystem IO, lowered through the
    /// same authored-flow host, and identified explicitly in the route receipt.
    #[tokio::test]
    async fn flow_run_executes_an_inline_program() {
        let (_temp, system) = fixture();
        let host = Arc::new(CapturingLoopHost::default());
        let ctx = flow_run_context(system, host.clone());

        let result = FlowRunTool
            .execute(
                &ctx,
                serde_json::json!({
                    "inline_program": "flow inline_shape\n  return {ok: true}\n",
                    "inputs": {"answer": 42},
                }),
            )
            .await
            .expect("inline program should run");
        let result: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(
            result["route"],
            serde_json::json!({
                "operation": "flow_run",
                "resolved_path": null,
                "flow_name": "inline_shape",
                "seeded_input_keys": ["answer"],
            })
        );

        let asts = host.asts.lock().unwrap();
        assert_eq!(asts.len(), 1);
        assert_eq!(asts[0]["name"], "inline_shape");
    }

    /// A path is an alternative address, never a second ambiguous selector. The exact-one rule is
    /// checked before filesystem IO so malformed model output cannot accidentally run a name.
    #[tokio::test]
    async fn flow_run_requires_exactly_one_address() {
        let (_temp, system) = fixture();
        let host = Arc::new(CapturingLoopHost::default());
        let ctx = flow_run_context(system, host);
        for input in [
            serde_json::json!({}),
            serde_json::json!({"name": "saved", "path": "examples/saved.flux"}),
            serde_json::json!({
                "name": "saved",
                "inline_program": "flow saved\n  return null\n"
            }),
            serde_json::json!({
                "path": "examples/saved.flux",
                "inline_program": "flow saved\n  return null\n"
            }),
        ] {
            let error = FlowRunTool
                .execute(&ctx, input)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("exactly one of `name`, `path`, or `inline_program`"),
                "{error}"
            );
        }
    }

    /// The path resolver is the guarded `System`, not ambient `std::fs`; a lexical escape never
    /// reaches the loop host even when the target exists.
    #[tokio::test]
    async fn flow_run_path_cannot_escape_the_workspace() {
        let (temp, system) = fixture();
        std::fs::write(
            temp.path().join("outside.flux"),
            "flow outside\n  return null\n",
        )
        .unwrap();
        let host = Arc::new(CapturingLoopHost::default());
        let ctx = flow_run_context(system, host.clone());
        let error = FlowRunTool
            .execute(&ctx, serde_json::json!({"path": "../outside.flux"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("escapes the workspace root"), "{error}");
        assert!(host.asts.lock().unwrap().is_empty());
    }
}
