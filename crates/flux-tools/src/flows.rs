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

/// Directories searched, in precedence order (project shadows global; flows shadow ops).
/// `@`-prefixed entries are workspace *named roots* — read only when the CLI registered them.
const FLOW_DIRS: &[&str] = &[".flux/flows", "@global_flows", ".flux/ops", "@global_ops"];

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

/// Every `.flux` file under the flow dirs as `(workspace_path, source)`, in `FLOW_DIRS`
/// precedence order. Missing dirs and unregistered named roots are skipped.
fn flow_files(ctx: &ToolContext) -> Vec<(String, String)> {
    let ws = ctx.system.workspace();
    let mut out = Vec::new();
    for dir in FLOW_DIRS {
        if let Some(root) = dir.strip_prefix('@') {
            if !ws.has_named_root(root) {
                continue;
            }
        }
        if let Ok(files) = ctx.system.read_dir_text_files(dir, "flux") {
            out.extend(files);
        }
    }
    out
}

fn basename(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .strip_suffix(".flux")
        .unwrap_or(path)
        .to_string()
}

fn param_names(ps: &[Param]) -> Vec<String> {
    ps.iter().map(|p| p.name.to_string()).collect()
}

/// One discoverable declaration (a top-level flow or a composite op).
struct Entry {
    name: String,
    kind: &'static str,
    description: String,
    params: Vec<String>,
}

/// Parse one file into its discoverable entries. An unparseable file yields a single
/// `error` entry so `flow_list` surfaces it rather than hiding it.
fn entries_of(path: &str, source: &str) -> Vec<Entry> {
    match Module::parse_str(source) {
        Ok(Module::Flow(ast)) => vec![Entry {
            name: ast.name.clone().unwrap_or_else(|| basename(path)),
            kind: "flow",
            description: String::new(),
            params: param_names(&ast.params),
        }],
        Ok(Module::Program(program)) => {
            let mut v = Vec::new();
            for f in &program.flows {
                v.push(Entry {
                    name: f.name.clone().unwrap_or_else(|| basename(path)),
                    kind: "flow",
                    description: String::new(),
                    params: param_names(&f.params),
                });
            }
            for o in &program.ops {
                v.push(Entry {
                    name: o.name.clone(),
                    kind: "op",
                    description: o.meta.description.clone(),
                    params: param_names(&o.params),
                });
            }
            v
        }
        Err(e) => vec![Entry {
            name: basename(path),
            kind: "error",
            description: format!("parse error: {e}"),
            params: Vec::new(),
        }],
    }
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
        let mut seen = HashSet::new();
        let mut lines = Vec::new();
        for (path, source) in flow_files(ctx) {
            for e in entries_of(&path, &source) {
                // First (highest-precedence) definition of a name wins.
                if !seen.insert(e.name.clone()) {
                    continue;
                }
                let desc = if e.description.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", e.description)
                };
                let params = if e.params.is_empty() {
                    String::new()
                } else {
                    format!("  (params: {})", e.params.join(", "))
                };
                lines.push(format!("{} [{}]{}{}", e.name, e.kind, desc, params));
            }
        }
        if lines.is_empty() {
            return Ok(ToolResult::ok(
                "no flows found — add .flux files under .flux/flows or ~/.flux/flows",
            ));
        }
        Ok(ToolResult::ok(lines.join("\n")))
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
            .map(|n| vec![format!("flow:{n}")])
            .unwrap_or_default()
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: FlowRunInput = crate::parse_params(params, "flow_run")?;
        let mut ast = resolve_flow(ctx, &args.name)?;

        // Seed inputs as literal binds, prepended so a flow-local `bind` can still shadow them
        // (matches FlowStore::seed's last-writer-wins semantics).
        if let Some(inputs) = args.inputs {
            let obj = inputs.as_object().ok_or_else(|| {
                Error::Other("flow_run: `inputs` must be a JSON object".into())
            })?;
            let mut seeded: Vec<Node> = obj
                .iter()
                .map(|(k, v)| Node::Bind {
                    name: k.clone().into(),
                    value: Box::new(Node::Lit { value: v.clone() }),
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

/// Resolve `name` to a runnable flow AST: a file whose stem is `name` (its flow named `name`,
/// else its first flow), or any file declaring a flow named `name`.
fn resolve_flow(ctx: &ToolContext, name: &str) -> Result<DraftAst> {
    let files = flow_files(ctx);
    if let Some((path, source)) = files.iter().find(|(p, _)| basename(p) == name) {
        return flow_from_source(path, source, name);
    }
    for (_path, source) in &files {
        if let Ok(module) = Module::parse_str(source) {
            match module {
                Module::Flow(ast) if ast.name.as_deref() == Some(name) => return Ok(ast),
                Module::Program(program) => {
                    if let Some(f) = program.flow_named(name) {
                        return Ok(f.clone());
                    }
                }
                _ => {}
            }
        }
    }
    Err(Error::Other(format!(
        "flow_run: no flow named `{name}` under .flux/flows or ~/.flux/flows (try flow_list)"
    )))
}

fn flow_from_source(path: &str, source: &str, name: &str) -> Result<DraftAst> {
    match Module::parse_str(source).map_err(|e| Error::Other(format!("{path}: {e}")))? {
        Module::Flow(ast) => Ok(ast),
        Module::Program(program) => {
            if let Some(f) = program.flow_named(name) {
                Ok(f.clone())
            } else if let Some(f) = program.flows.first() {
                Ok(f.clone())
            } else if let Some(o) = program.ops.first() {
                Err(Error::Other(format!(
                    "`{path}` defines op `{}` and no flow — call it directly, e.g. {}({{…}})",
                    o.name, o.name
                )))
            } else {
                Err(Error::Other(format!("`{path}` has no runnable flow")))
            }
        }
    }
}
