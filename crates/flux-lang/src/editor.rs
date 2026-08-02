//! Host-neutral projection between authored [`DraftAst`] flows and a structured visual-editor IR.
//!
//! This is deliberately not a graph runtime. The editor shape can represent only structured Flux
//! constructs whose edges are derived without inventing arbitrary cycles; lowering returns the same
//! [`DraftAst`] the ordinary analyzer and interpreter consume. A valid flow containing another node
//! kind is reported as source-only, never partially projected or repaired.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ast::{Branch, DraftAst, FlowEffect, Node, Param, SymbolName, TypeRef};
use crate::lower_cst::RangeMap;
use crate::syntax::SyntaxKind;

/// The first and currently only editor wire version.
pub const EDITOR_SCHEMA_VERSION: u32 = 1;

/// One lifecycle phase for an editor-addressable statement activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditorTracePhase {
    /// Execution entered this node activation.
    Entered,
    /// A structured control node selected this branch.
    BranchSelected,
    /// The node activation completed successfully.
    Succeeded,
    /// The node activation failed or was cancelled.
    Failed,
}

/// A value-free execution record that a host can join to its stored editor graph.
///
/// `occurrence` is one-based and scoped to this execution. Repeated loop activations therefore
/// remain distinguishable without placing arguments, results, or other potentially-secret values
/// in the structural trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EditorTraceEvent {
    /// Stable editor identity of the authored node.
    pub node_id: String,
    /// Deterministic path of the node in the lowered AST.
    pub source_path: String,
    /// One-based activation count for this node in the current execution.
    pub occurrence: u64,
    /// Lifecycle phase being reported.
    pub phase: EditorTracePhase,
    /// Selected branch name for [`EditorTracePhase::BranchSelected`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Host callback for editor-addressable execution events.
pub trait EditorTraceObserver: Send + Sync {
    /// Observe one value-free node lifecycle event.
    fn event(&self, event: &EditorTraceEvent);
}

/// Per-execution editor trace state passed to
/// [`crate::runtime::execute_flow_traced`].
///
/// Construct a fresh value for each run: occurrence counters deliberately do not cross execution
/// boundaries. The observer can forward events to a channel, durable run log, or in-memory test
/// recorder without coupling `flux-lang` to any of those host concerns.
#[derive(Clone)]
pub struct EditorExecutionTrace {
    node_map: Arc<BTreeMap<String, String>>,
    observer: Arc<dyn EditorTraceObserver>,
    occurrences: Arc<Mutex<HashMap<String, u64>>>,
}

impl EditorExecutionTrace {
    /// Construct per-execution trace state from an AST-path → editor-id map.
    pub fn new(node_map: BTreeMap<String, String>, observer: Arc<dyn EditorTraceObserver>) -> Self {
        Self {
            node_map: Arc::new(node_map),
            observer,
            occurrences: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Build trace state from a projected graph's path-to-id map.
    pub fn for_flow(flow: &EditorFlow, observer: Arc<dyn EditorTraceObserver>) -> Self {
        Self::new(flow.node_map(), observer)
    }

    pub(crate) fn enter(&self, source_path: &str) -> Option<EditorTraceActivation> {
        let node_id = self.node_map.get(source_path)?.clone();
        let occurrence = {
            let mut occurrences = self.occurrences.lock().unwrap_or_else(|e| e.into_inner());
            let occurrence = occurrences.entry(node_id.clone()).or_default();
            *occurrence += 1;
            *occurrence
        };
        let event = EditorTraceEvent {
            node_id,
            source_path: source_path.to_string(),
            occurrence,
            phase: EditorTracePhase::Entered,
            branch: None,
        };
        self.observer.event(&event);
        Some(EditorTraceActivation {
            trace: self.clone(),
            event,
            finished: false,
        })
    }
}

/// Drop-safe activation guard: every entered node receives a terminal event, including failures
/// propagated by `?` and cancelled concurrent branches.
pub(crate) struct EditorTraceActivation {
    trace: EditorExecutionTrace,
    event: EditorTraceEvent,
    finished: bool,
}

impl EditorTraceActivation {
    pub(crate) fn branch_selected(&self, branch: impl Into<String>) {
        let mut event = self.event.clone();
        event.phase = EditorTracePhase::BranchSelected;
        event.branch = Some(branch.into());
        self.trace.observer.event(&event);
    }

    pub(crate) fn succeed(&mut self) {
        if !self.finished {
            let mut event = self.event.clone();
            event.phase = EditorTracePhase::Succeeded;
            self.trace.observer.event(&event);
            self.finished = true;
        }
    }
}

impl Drop for EditorTraceActivation {
    fn drop(&mut self) {
        if !self.finished {
            let mut event = self.event.clone();
            event.phase = EditorTracePhase::Failed;
            self.trace.observer.event(&event);
        }
    }
}

/// A byte range in the exact UTF-8 Flux source supplied to [`project_source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EditorSourceRange {
    /// Inclusive byte offset.
    pub start: u32,
    /// Exclusive byte offset.
    pub end: u32,
}

/// A limitation of graph mode. Diagnostics do not make otherwise-valid Flux invalid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EditorDiagnostic {
    /// Stable machine-readable limitation code.
    pub code: String,
    /// Human-readable explanation suitable for an editor.
    pub message: String,
    /// Editor node identity, when the limitation belongs to a node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Semantic AST path, when the limitation belongs to a node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Exact source byte range when projection began from source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<EditorSourceRange>,
}

/// The total result of projecting valid Flux: either one complete graph, or diagnostics explaining
/// why the host must keep the exact source in source-only mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EditorProjection {
    /// Complete editable graph, absent whenever any source construct is unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<EditorFlow>,
    /// Limitations that require the host to retain source-only mode.
    #[serde(default)]
    pub diagnostics: Vec<EditorDiagnostic>,
}

/// A structured editor projection of one bare flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EditorFlow {
    /// Wire schema version interpreted by [`lower`].
    pub schema_version: u32,
    /// Optional authored flow name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Authored flow parameters.
    #[serde(default)]
    pub params: Vec<Param>,
    /// Optional authored return type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<TypeRef>,
    /// Structured flow body in execution order.
    #[serde(default)]
    pub body: Vec<EditorNode>,
}

impl EditorFlow {
    /// The AST-path → editor-id map a host retains beside an immutable published version.
    ///
    /// Paths are derived from the graph's current structure rather than the projection-time
    /// `source_path` fields. Moving a node in graph mode therefore moves its runtime address while
    /// preserving its editor id.
    pub fn node_map(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        collect_node_map(&self.body, "body", &mut out);
        out
    }
}

/// One graph node. `source_path` uses the analyzer/range-map spelling (`body[0].then[1]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EditorNode {
    /// Stable opaque editor identity.
    pub id: String,
    /// AST path at projection time; derive current execution paths with [`EditorFlow::node_map`].
    pub source_path: String,
    /// Structured node payload.
    #[serde(flatten)]
    pub kind: EditorNodeKind,
}

/// The intentionally-small v1 visual vocabulary. Expression positions stay ordinary Flux nodes;
/// the analyzer remains their validation authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EditorNodeKind {
    Call {
        op: String,
        #[serde(default)]
        args: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<SymbolName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ty: Option<TypeRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect: Option<FlowEffect>,
    },
    When {
        cond: Node,
        #[serde(default)]
        then: Vec<EditorNode>,
        #[serde(default)]
        otherwise: Vec<EditorNode>,
    },
    Repeat {
        max: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<Node>,
        #[serde(default)]
        body: Vec<EditorNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collect: Option<SymbolName>,
    },
    Each {
        source: Node,
        item: SymbolName,
        #[serde(default)]
        body: Vec<EditorNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collect: Option<SymbolName>,
        #[serde(default)]
        flat: bool,
    },
    Parallel {
        #[serde(default)]
        branches: Vec<EditorBranch>,
    },
    Return {
        value: Node,
    },
}

/// One named parallel branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EditorBranch {
    /// Authored branch binding name.
    pub name: SymbolName,
    /// Structured branch body.
    #[serde(default)]
    pub body: Vec<EditorNode>,
}

/// Why an editor graph could not be lowered to an AST.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EditorError {
    #[error("unsupported editor schema version {0}")]
    UnsupportedVersion(u32),
    #[error("editor node id `{0}` occurs more than once")]
    DuplicateNodeId(String),
}

/// Project a semantic flow. When any statement is outside the visual subset, `graph` is `None` and
/// every such statement is named in `diagnostics`.
pub fn project(ast: &DraftAst, previous: Option<&EditorFlow>) -> EditorProjection {
    let mut previous = PreviousIds::new(previous);
    let mut diagnostics = Vec::new();
    let body = project_nodes(&ast.body, "body", &mut previous, &mut diagnostics);
    if diagnostics.is_empty() {
        EditorProjection {
            graph: Some(EditorFlow {
                schema_version: EDITOR_SCHEMA_VERSION,
                name: ast.name.clone(),
                params: ast.params.clone(),
                returns: ast.returns.clone(),
                body,
            }),
            diagnostics,
        }
    } else {
        EditorProjection {
            graph: None,
            diagnostics,
        }
    }
}

/// Parse and project exact Flux source. Comments deliberately force source-only mode: v1 has no
/// comment node, so graph editing could otherwise erase author-owned text while appearing lossless.
pub fn project_source(
    source: &str,
    previous: Option<&EditorFlow>,
) -> crate::Result<EditorProjection> {
    let parsed = crate::parser::parse_cst(source);
    let lowered = crate::lower_cst::cst_to_draft(&parsed)
        .map_err(|errors| crate::parse::lowering_error(errors, &parsed.syntax()))?;

    if let Some(comment) = parsed
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == SyntaxKind::COMMENT)
    {
        return Ok(EditorProjection {
            graph: None,
            diagnostics: vec![EditorDiagnostic {
                code: "editor.source_trivia".into(),
                message: "comments are preserved in source mode and are not editable in graph mode"
                    .into(),
                node_id: None,
                path: None,
                range: Some(to_editor_range(comment.text_range())),
            }],
        });
    }

    let mut projection = project(&lowered.ast, previous);
    attach_ranges(&mut projection.diagnostics, &lowered.ranges);
    Ok(projection)
}

/// Lower an editor graph into the ordinary semantic AST. This function does not analyze operation
/// names, arguments or types; callers pass the result through the same analyzer as authored source.
pub fn lower(graph: &EditorFlow) -> Result<DraftAst, EditorError> {
    if graph.schema_version != EDITOR_SCHEMA_VERSION {
        return Err(EditorError::UnsupportedVersion(graph.schema_version));
    }
    let mut ids = HashSet::new();
    validate_ids(&graph.body, &mut ids)?;
    Ok(DraftAst {
        name: graph.name.clone(),
        params: graph.params.clone(),
        returns: graph.returns.clone(),
        body: lower_nodes(&graph.body),
    })
}

/// Lower an editor graph and render canonical, re-parseable Flux source.
///
/// Exact authored trivia is intentionally not accepted here: a graph exists only when projection
/// proved that graph editing is lossless for the supported semantic subset. Source-only hosts keep
/// their original bytes instead of calling this function.
pub fn lower_source(graph: &EditorFlow) -> Result<String, EditorError> {
    lower(graph).map(|ast| crate::format::format(&ast))
}

fn project_nodes(
    nodes: &[Node],
    prefix: &str,
    previous: &mut PreviousIds,
    diagnostics: &mut Vec<EditorDiagnostic>,
) -> Vec<EditorNode> {
    nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            let path = format!("{prefix}[{index}]");
            project_node(node, path, previous, diagnostics)
        })
        .collect()
}

fn project_node(
    node: &Node,
    path: String,
    previous: &mut PreviousIds,
    diagnostics: &mut Vec<EditorDiagnostic>,
) -> Option<EditorNode> {
    let fingerprint = node_fingerprint(node);
    let id = previous
        .take(&path, &fingerprint)
        .unwrap_or_else(|| generated_id(&path, node));

    let kind = match node {
        Node::Call { op, args } => EditorNodeKind::Call {
            op: op.clone(),
            args: args.clone(),
            bind: None,
            ty: None,
            effect: None,
        },
        Node::Bind {
            name,
            value,
            ty,
            effect,
        } => match value.as_ref() {
            Node::Call { op, args } => EditorNodeKind::Call {
                op: op.clone(),
                args: args.clone(),
                bind: Some(name.clone()),
                ty: ty.clone(),
                effect: *effect,
            },
            _ => return unsupported(node, path, id, diagnostics),
        },
        Node::When {
            cond,
            then,
            otherwise,
        } => EditorNodeKind::When {
            cond: cond.as_ref().clone(),
            then: project_nodes(then, &format!("{path}.then"), previous, diagnostics),
            otherwise: project_nodes(
                otherwise,
                &format!("{path}.otherwise"),
                previous,
                diagnostics,
            ),
        },
        Node::Repeat {
            max,
            until,
            body,
            collect,
        } => EditorNodeKind::Repeat {
            max: *max,
            until: until.as_deref().cloned(),
            body: project_nodes(body, &format!("{path}.body"), previous, diagnostics),
            collect: collect.clone(),
        },
        Node::Each {
            source,
            item,
            body,
            collect,
            flat,
        } => EditorNodeKind::Each {
            source: source.as_ref().clone(),
            item: item.clone(),
            body: project_nodes(body, &format!("{path}.body"), previous, diagnostics),
            collect: collect.clone(),
            flat: *flat,
        },
        Node::Parallel { branches } => EditorNodeKind::Parallel {
            branches: branches
                .iter()
                .enumerate()
                .map(|(index, branch)| EditorBranch {
                    name: branch.name.clone(),
                    body: project_nodes(
                        &branch.body,
                        &format!("{path}.branches[{index}].body"),
                        previous,
                        diagnostics,
                    ),
                })
                .collect(),
        },
        Node::Return { value } => EditorNodeKind::Return {
            value: value.as_ref().clone(),
        },
        _ => return unsupported(node, path, id, diagnostics),
    };
    Some(EditorNode {
        id,
        source_path: path,
        kind,
    })
}

fn unsupported(
    node: &Node,
    path: String,
    id: String,
    diagnostics: &mut Vec<EditorDiagnostic>,
) -> Option<EditorNode> {
    diagnostics.push(EditorDiagnostic {
        code: "editor.unsupported_node".into(),
        message: format!(
            "`{}` is valid Flux but is not editable in the visual v1 subset",
            node_kind(node)
        ),
        node_id: Some(id),
        path: Some(path),
        range: None,
    });
    None
}

fn lower_nodes(nodes: &[EditorNode]) -> Vec<Node> {
    nodes.iter().map(lower_node).collect()
}

fn lower_node(node: &EditorNode) -> Node {
    match &node.kind {
        EditorNodeKind::Call {
            op,
            args,
            bind,
            ty,
            effect,
        } => {
            let call = Node::Call {
                op: op.clone(),
                args: args.clone(),
            };
            match bind {
                Some(name) => Node::Bind {
                    name: name.clone(),
                    value: Box::new(call),
                    ty: ty.clone(),
                    effect: *effect,
                },
                None => call,
            }
        }
        EditorNodeKind::When {
            cond,
            then,
            otherwise,
        } => Node::When {
            cond: Box::new(cond.clone()),
            then: lower_nodes(then),
            otherwise: lower_nodes(otherwise),
        },
        EditorNodeKind::Repeat {
            max,
            until,
            body,
            collect,
        } => Node::Repeat {
            max: *max,
            until: until.clone().map(Box::new),
            body: lower_nodes(body),
            collect: collect.clone(),
        },
        EditorNodeKind::Each {
            source,
            item,
            body,
            collect,
            flat,
        } => Node::Each {
            source: Box::new(source.clone()),
            item: item.clone(),
            body: lower_nodes(body),
            collect: collect.clone(),
            flat: *flat,
        },
        EditorNodeKind::Parallel { branches } => Node::Parallel {
            branches: branches
                .iter()
                .map(|branch| Branch {
                    name: branch.name.clone(),
                    body: lower_nodes(&branch.body),
                })
                .collect(),
        },
        EditorNodeKind::Return { value } => Node::Return {
            value: Box::new(value.clone()),
        },
    }
}

fn validate_ids(nodes: &[EditorNode], seen: &mut HashSet<String>) -> Result<(), EditorError> {
    for node in nodes {
        if !seen.insert(node.id.clone()) {
            return Err(EditorError::DuplicateNodeId(node.id.clone()));
        }
        match &node.kind {
            EditorNodeKind::When {
                then, otherwise, ..
            } => {
                validate_ids(then, seen)?;
                validate_ids(otherwise, seen)?;
            }
            EditorNodeKind::Repeat { body, .. } | EditorNodeKind::Each { body, .. } => {
                validate_ids(body, seen)?;
            }
            EditorNodeKind::Parallel { branches } => {
                for branch in branches {
                    validate_ids(&branch.body, seen)?;
                }
            }
            EditorNodeKind::Call { .. } | EditorNodeKind::Return { .. } => {}
        }
    }
    Ok(())
}

fn collect_node_map(nodes: &[EditorNode], prefix: &str, out: &mut BTreeMap<String, String>) {
    for (index, node) in nodes.iter().enumerate() {
        let path = format!("{prefix}[{index}]");
        out.insert(path.clone(), node.id.clone());
        match &node.kind {
            EditorNodeKind::When {
                then, otherwise, ..
            } => {
                collect_node_map(then, &format!("{path}.then"), out);
                collect_node_map(otherwise, &format!("{path}.otherwise"), out);
            }
            EditorNodeKind::Repeat { body, .. } | EditorNodeKind::Each { body, .. } => {
                collect_node_map(body, &format!("{path}.body"), out);
            }
            EditorNodeKind::Parallel { branches } => {
                for (branch_index, branch) in branches.iter().enumerate() {
                    collect_node_map(
                        &branch.body,
                        &format!("{path}.branches[{branch_index}].body"),
                        out,
                    );
                }
            }
            EditorNodeKind::Call { .. } | EditorNodeKind::Return { .. } => {}
        }
    }
}

struct PreviousIds {
    by_path: HashMap<String, PreviousNode>,
    by_fingerprint: HashMap<String, Vec<String>>,
    claimed: HashSet<String>,
}

struct PreviousNode {
    fingerprint: String,
    id: String,
}

impl PreviousIds {
    fn new(previous: Option<&EditorFlow>) -> Self {
        let mut by_path = HashMap::new();
        let mut by_fingerprint = HashMap::<String, Vec<String>>::new();
        if let Some(previous) = previous {
            collect_previous(&previous.body, "body", &mut by_path, &mut by_fingerprint);
        }
        Self {
            by_path,
            by_fingerprint,
            claimed: HashSet::new(),
        }
    }

    fn take(&mut self, path: &str, fingerprint: &str) -> Option<String> {
        let exact = self
            .by_path
            .get(path)
            .filter(|node| node.fingerprint == fingerprint)
            .map(|node| node.id.clone());
        if let Some(id) = exact.filter(|id| self.claimed.insert(id.clone())) {
            return Some(id);
        }

        // Prefer a semantic match before falling back to position: when deleting an earlier node
        // shifts its successor into the same path, the successor must keep its own identity.
        let semantic = self
            .by_fingerprint
            .get(fingerprint)
            .and_then(|ids| ids.iter().find(|id| !self.claimed.contains(*id)))
            .cloned();
        if let Some(id) = semantic {
            self.claimed.insert(id.clone());
            return Some(id);
        }

        let positional = self.by_path.get(path).map(|node| node.id.clone());
        positional.filter(|id| self.claimed.insert(id.clone()))
    }
}

fn collect_previous(
    nodes: &[EditorNode],
    prefix: &str,
    by_path: &mut HashMap<String, PreviousNode>,
    by_fingerprint: &mut HashMap<String, Vec<String>>,
) {
    for (index, node) in nodes.iter().enumerate() {
        let path = format!("{prefix}[{index}]");
        let lowered = lower_node(node);
        let fingerprint = node_fingerprint(&lowered);
        by_path.insert(
            path.clone(),
            PreviousNode {
                fingerprint: fingerprint.clone(),
                id: node.id.clone(),
            },
        );
        by_fingerprint
            .entry(fingerprint)
            .or_default()
            .push(node.id.clone());
        match &node.kind {
            EditorNodeKind::When {
                then, otherwise, ..
            } => {
                collect_previous(then, &format!("{path}.then"), by_path, by_fingerprint);
                collect_previous(
                    otherwise,
                    &format!("{path}.otherwise"),
                    by_path,
                    by_fingerprint,
                );
            }
            EditorNodeKind::Repeat { body, .. } | EditorNodeKind::Each { body, .. } => {
                collect_previous(body, &format!("{path}.body"), by_path, by_fingerprint);
            }
            EditorNodeKind::Parallel { branches } => {
                for (branch_index, branch) in branches.iter().enumerate() {
                    collect_previous(
                        &branch.body,
                        &format!("{path}.branches[{branch_index}].body"),
                        by_path,
                        by_fingerprint,
                    );
                }
            }
            EditorNodeKind::Call { .. } | EditorNodeKind::Return { .. } => {}
        }
    }
}

fn node_fingerprint(node: &Node) -> String {
    let encoded = serde_json::to_vec(node).unwrap_or_default();
    hex_prefix(&encoded)
}

fn generated_id(path: &str, node: &Node) -> String {
    let mut bytes = path.as_bytes().to_vec();
    bytes.extend(serde_json::to_vec(node).unwrap_or_default());
    format!("node_{}", hex_prefix(&bytes))
}

fn hex_prefix(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn attach_ranges(diagnostics: &mut [EditorDiagnostic], ranges: &RangeMap) {
    for diagnostic in diagnostics {
        if diagnostic.range.is_none() {
            diagnostic.range = diagnostic
                .path
                .as_deref()
                .and_then(|path| ranges.resolve(path))
                .map(to_editor_range);
        }
    }
}

fn to_editor_range(range: rowan::TextRange) -> EditorSourceRange {
    EditorSourceRange {
        start: u32::from(range.start()),
        end: u32::from(range.end()),
    }
}

fn node_kind(node: &Node) -> &'static str {
    match node {
        Node::Assert { .. } => "assert",
        Node::Pipe { .. } => "pipe",
        Node::Seq { .. } => "seq",
        Node::Memo { .. } => "memo",
        Node::Await { .. } => "await",
        Node::Retry { .. } => "retry",
        Node::Try { .. } => "try",
        Node::Confirm { .. } => "confirm",
        Node::Loop { .. } => "loop",
        Node::Race { .. } => "race",
        Node::Throttle { .. } => "throttle",
        Node::Debounce { .. } => "debounce",
        Node::Unless { .. } => "unless",
        Node::Verify { .. } => "verify",
        Node::Peek { .. } => "peek",
        Node::Var { .. } => "var",
        Node::Lit { .. } => "lit",
        Node::Thing { .. } => "thing",
        Node::Expr { .. } => "expr",
        Node::Fmt { .. } => "fmt",
        Node::Jq { .. } => "jq",
        Node::Parse { .. } => "parse",
        Node::Ctx { .. } => "ctx",
        Node::CtxAppend { .. } => "ctx_append",
        Node::Match { .. } => "match",
        Node::Route { .. } => "route",
        Node::Fallback { .. } => "fallback",
        Node::Timeout { .. } => "timeout",
        Node::Budget { .. } => "budget",
        Node::CapScope { .. } => "cap_scope",
        Node::Scope { .. } => "scope",
        Node::Saga { .. } => "saga",
        Node::Once { .. } => "once",
        Node::Checkpoint { .. } => "checkpoint",
        Node::Obj { .. } => "obj",
        Node::List { .. } => "list",
        Node::Call { .. } => "call",
        Node::Bind { .. } => "bind",
        Node::When { .. } => "when",
        Node::Repeat { .. } => "repeat",
        Node::Each { .. } => "each",
        Node::Parallel { .. } => "parallel",
        Node::Return { .. } => "return",
    }
}
