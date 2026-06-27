//! flux-flow's execution adapters + thin wrappers over the L0 reference interpreter in
//! [`flux_lang::runtime`]. The interpreter is generic over injected traits; here we implement them
//! over the real safety envelope (`Executor::dispatch` + approver), the SQLite `FlowStore`, and the
//! `AgentSink`, then expose `execute_flow` / `execute_call` with their original signatures so every
//! caller is unchanged.
//!
//! `plan_risk` + `PlanApprover` stay here: they need the concrete `ToolRegistry` and `Tool::intents`
//! (literal-arg destructive/path detection), which the language-level [`OpCatalog`] does not carry.
//! Every op still runs through `Executor::dispatch` — no new bypass surface.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;

use flux_agent::AgentSink;
use flux_runtime::{ApprovalChoice, Approver, Executor, ToolRegistry, ToolResult};
use flux_spec::{IntentSet, Risk};

use flux_lang::host::{OpHost, OpOutcome};
use flux_lang::opspec::OpCatalog;
use flux_lang::sink::FlowSink;

use crate::ast::{DraftAst, Node};
use crate::registry::{schema_params, OpRegistry};
use crate::state::FlowStore;
use crate::Result;

/// The interpreter's public types, re-exported so `flux_flow::runtime::{…}` paths are unchanged.
pub use flux_lang::runtime::{BindSpec, CallOutcome, FlowOutcome};

// ---------------------------------------------------------------------------
// Adapters: the engine's envelope → the interpreter's injected traits
// ---------------------------------------------------------------------------

/// Adapts the real [`Executor`] (dispatch + approver + registry) onto the interpreter's [`OpHost`].
struct ExecutorHost<'a> {
    executor: &'a Executor,
    catalog: OpRegistry<'a>,
}

impl<'a> ExecutorHost<'a> {
    fn new(executor: &'a Executor) -> Self {
        Self {
            catalog: OpRegistry::new(executor.registry()),
            executor,
        }
    }
}

#[async_trait]
impl OpHost for ExecutorHost<'_> {
    async fn dispatch(&self, op: &str, input: serde_json::Value) -> OpOutcome {
        let r = self.executor.dispatch(op, input).await;
        OpOutcome {
            content: r.content,
            view: r.view,
            is_error: r.is_error,
        }
    }

    fn catalog(&self) -> &dyn OpCatalog {
        &self.catalog
    }

    async fn request_approval(
        &self,
        label: &str,
        intents: &IntentSet,
    ) -> flux_lang::host::ApprovalChoice {
        let subjects = [label.to_string()];
        let choice = self
            .executor
            .approver()
            .request("confirm", &subjects, intents)
            .await;
        // `AllowAlways` is an approval too (the user chose "allow & remember"); exhaustive match so a
        // new `ApprovalChoice` variant forces a decision here rather than silently mapping to `Deny`.
        match choice {
            ApprovalChoice::Allow | ApprovalChoice::AllowAlways(_) => {
                flux_lang::host::ApprovalChoice::Allow
            }
            ApprovalChoice::Deny => flux_lang::host::ApprovalChoice::Deny,
        }
    }

    fn trim_output(&self, view: String, op: &str) -> String {
        flux_runtime::trim_tool_output(view, flux_runtime::tool_output_cap(), op)
    }
}

/// Bridges the interpreter's [`FlowSink`] back onto the engine's [`AgentSink`].
struct SinkBridge<'a> {
    inner: &'a mut dyn AgentSink,
}

impl FlowSink for SinkBridge<'_> {
    fn text_delta(&mut self, text: &str) {
        self.inner.text_delta(text);
    }
    fn thinking_delta(&mut self, text: &str) {
        self.inner.thinking_delta(text);
    }
    fn planning(&mut self, active: bool) {
        self.inner.planning(active);
    }
    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        self.inner.tool_call(name, input);
    }
    fn tool_result(&mut self, name: &str, result: &OpOutcome) {
        self.inner.tool_result(
            name,
            &ToolResult {
                content: result.content.clone(),
                view: result.view.clone(),
                is_error: result.is_error,
            },
        );
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.inner.observation(o);
    }
    fn turn_end(&mut self, usage: Option<flux_core::Usage>) {
        self.inner.turn_end(usage);
    }
}

// ---------------------------------------------------------------------------
// Thin wrappers: original signatures, delegating to the interpreter
// ---------------------------------------------------------------------------

/// Execute one registered operation through the envelope, storing the result and (optionally) binding
/// a symbol — the original signature, delegating to [`flux_lang::runtime::execute_call`].
pub async fn execute_call(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    op: &str,
    input: serde_json::Value,
    bind: Option<BindSpec<'_>>,
) -> Result<CallOutcome> {
    let host = ExecutorHost::new(executor);
    flux_lang::runtime::execute_call(store, &host, session_id, op, input, bind).await
}

/// Execute a compiled flow — the original signature, delegating to [`flux_lang::runtime::execute_flow`]
/// with the engine's executor/sink adapted onto the interpreter's traits.
pub async fn execute_flow(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    ast: &DraftAst,
    sink: &mut dyn AgentSink,
) -> Result<FlowOutcome> {
    let host = ExecutorHost::new(executor);
    let mut bridge = SinkBridge { inner: sink };
    flux_lang::runtime::execute_flow(store, &host, session_id, ast, &mut bridge).await
}

// ---------------------------------------------------------------------------
// Plan risk + whole-plan approval
// ---------------------------------------------------------------------------

/// A best-effort risk preview of a compiled plan, aggregated from the ops it calls. Dispatch
/// re-checks every op at execution, so the safety floor never depends on this — it drives the one
/// whole-plan approval prompt.
#[derive(Debug, Clone, Default)]
pub struct PlanRisk {
    /// The highest [`Risk`] across the plan's ops (`None` if it calls nothing registered).
    pub max_risk: Option<Risk>,
    /// True if any op is destructive-shaped — forces a per-op re-confirm even inside an approved plan.
    pub destructive: bool,
    /// True if any op writes / executes / connects out.
    pub mutating: bool,
    /// The distinct op names the plan calls, in first-seen order.
    pub ops: Vec<String>,
}

impl PlanRisk {
    /// A one-line human summary (for the approval prompt).
    pub fn summary(&self) -> String {
        let base = match self.max_risk {
            Some(Risk::Destructive) => "destructive",
            Some(Risk::High) => "high",
            Some(Risk::Medium) => "medium",
            Some(Risk::Low) => "low",
            None => "no-op",
        };
        if self.destructive {
            format!("{base} · contains a destructive operation (will re-confirm)")
        } else if self.mutating {
            format!("{base} · mutating")
        } else {
            base.to_string()
        }
    }
}

/// Compute a plan's [`PlanRisk`] by walking every `call` node and looking up each op's spec (risk)
/// and intents (destructive / mutating) in `registry`. Only literal args are known statically, so
/// they are fed to `Tool::intents` for command/path-shaped detection; `$symbol` args are skipped.
pub fn plan_risk(ast: &DraftAst, registry: &ToolRegistry) -> PlanRisk {
    let mut risk = PlanRisk::default();
    walk_calls(&ast.body, &mut |op, args| {
        if !risk.ops.iter().any(|o| o == op) {
            risk.ops.push(op.to_string());
        }
        let Some(tool) = registry.get(op) else {
            return;
        };
        let spec = tool.spec();
        risk.max_risk = Some(match risk.max_risk {
            Some(r) => r.max(spec.risk),
            None => spec.risk,
        });
        if spec.risk == Risk::Destructive {
            risk.destructive = true;
        }
        let intents = tool.intents(&literal_input(args, &spec.input_schema));
        if intents.is_destructive() {
            risk.destructive = true;
        }
        if intents.is_mutating() {
            risk.mutating = true;
        }
    });
    risk
}

/// Visit every `call` node reachable in `nodes` (recursing through binds, branches, loops, returns,
/// and nested call args), invoking `f(op, args)` for each.
fn walk_calls<'a>(nodes: &'a [Node], f: &mut impl FnMut(&'a str, &'a [Node])) {
    for node in nodes {
        walk_node(node, f);
    }
}

fn walk_node<'a>(node: &'a Node, f: &mut impl FnMut(&'a str, &'a [Node])) {
    match node {
        Node::Call { op, args } => {
            f(op, args);
            walk_calls(args, f);
        }
        Node::Bind { value, .. } => walk_node(value, f),
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            walk_node(cond, f);
            walk_calls(then, f);
            walk_calls(otherwise, f);
        }
        Node::Repeat { until, body, .. } => {
            if let Some(u) = until {
                walk_node(u, f);
            }
            walk_calls(body, f);
        }
        Node::Each { source, body, .. } => {
            walk_node(source, f);
            walk_calls(body, f);
        }
        Node::Assert { cond, .. } => walk_node(cond, f),
        Node::Pipe { steps, .. } => walk_calls(steps, f),
        Node::Seq { body, .. } => walk_calls(body, f),
        Node::Memo { value, .. } => walk_node(value, f),
        Node::Parallel { branches } => {
            for b in branches {
                walk_calls(&b.body, f);
            }
        }
        Node::Return { value } => walk_node(value, f),
        Node::Retry { body, .. } => walk_calls(body, f),
        Node::Try { body, handler, .. } => {
            walk_calls(body, f);
            walk_calls(handler, f);
        }
        Node::Confirm { body, .. } => walk_calls(body, f),
        Node::Loop { until, body, .. } => {
            if let Some(u) = until {
                walk_node(u, f);
            }
            walk_calls(body, f);
        }
        Node::Race { branches, .. } => {
            for b in branches {
                walk_calls(&b.body, f);
            }
        }
        Node::Throttle { body, .. } => walk_calls(body, f),
        Node::Debounce { body, .. } => walk_calls(body, f),
        Node::Unless { body, .. } => walk_calls(body, f),
        Node::Verify { cmd, expect, .. } => {
            walk_node(cmd, f);
            walk_node(expect, f);
        }
        Node::Peek { .. } => {}
        Node::Expr { vars, .. } => {
            for v in vars.values() {
                walk_node(v, f);
            }
        }
        Node::Fmt { .. } => {}
        Node::Jq { input, .. } => walk_node(input, f),
        Node::Var { .. }
        | Node::Lit { .. }
        | Node::Thing { .. }
        | Node::Await { .. }
        | Node::Parse { .. } => {}
    }
}

/// Build a best-effort named input from a call's *literal* args only (for intent preview); non-literal
/// args (`$symbols`) are skipped and arity is not enforced.
fn literal_input(args: &[Node], schema: &serde_json::Value) -> serde_json::Value {
    if let [Node::Lit { value }] = args {
        if value.is_object() {
            return value.clone();
        }
    }
    let (required, optional) = schema_params(schema);
    let order: Vec<String> = required.into_iter().chain(optional).collect();
    let mut input = serde_json::Map::new();
    for (i, arg) in args.iter().enumerate() {
        if let Node::Lit { value } = arg {
            if let Some(name) = order.get(i) {
                input.insert(name.clone(), value.clone());
            }
        }
    }
    serde_json::Value::Object(input)
}

/// An [`Approver`] for a pre-approved plan: a non-destructive op whose name is in the approved set is
/// allowed without prompting; a **destructive** op (or any op not in the set) falls through to
/// `fallback`, so destructive operations still escalate to a per-op confirmation even inside an
/// approved plan — the safety invariant. Installed on the execution executor after the user approves
/// the rendered plan.
pub struct PlanApprover {
    approved: HashSet<String>,
    fallback: Arc<dyn Approver>,
}

impl PlanApprover {
    /// Approve the given op names as a unit; everything else (and any destructive op) defers to
    /// `fallback`.
    pub fn new(approved: impl IntoIterator<Item = String>, fallback: Arc<dyn Approver>) -> Self {
        Self {
            approved: approved.into_iter().collect(),
            fallback,
        }
    }
}

#[async_trait]
impl Approver for PlanApprover {
    async fn request(
        &self,
        tool: &str,
        subjects: &[String],
        intents: &IntentSet,
    ) -> ApprovalChoice {
        if !intents.is_destructive() && self.approved.contains(tool) {
            ApprovalChoice::Allow
        } else {
            self.fallback.request(tool, subjects, intents).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{RunEvent, SymbolName, Value, Visibility};
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;

    use flux_runtime::{
        AllowApprover, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
    };
    use flux_spec::ToolSpec;
    use flux_system::{System, Workspace};

    /// A tool that echoes its `text` param back as content.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "echo",
                "echo text",
                json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
            )
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            params: serde_json::Value,
        ) -> flux_core::Result<ToolResult> {
            Ok(ToolResult::ok(
                params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    /// A tool whose canonical content ("RAW") differs from its model-facing view ("VIEW").
    struct TwoFaceTool;

    #[async_trait]
    impl Tool for TwoFaceTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "twoface",
                "two-face",
                json!({ "type": "object", "properties": {} }),
            )
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _params: serde_json::Value,
        ) -> flux_core::Result<ToolResult> {
            Ok(ToolResult::ok_view("RAW", "VIEW"))
        }
    }

    fn temp_executor(allow: bool) -> Executor {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-rt-{}-{}",
            std::process::id(),
            if allow { "allow" } else { "deny" }
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let perms = if allow {
            PermissionManager::from_rules(&["echo".into()], &[])
        } else {
            PermissionManager::from_rules(&[], &["echo".into()])
        };
        Executor::new(
            reg,
            perms,
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        )
    }

    #[tokio::test]
    async fn single_op_stores_value_binds_symbol_and_traces() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let draft = SymbolName("draft".into());

        let outcome = execute_call(
            &store,
            &ex,
            "sess",
            "echo",
            json!({"text": "renewal follow-up"}),
            Some(BindSpec {
                name: &draft,
                ty: Some("Draft"),
                visibility: Visibility::Visible,
            }),
        )
        .await
        .unwrap();

        assert!(!outcome.is_error);
        let vid = outcome.value_id.clone().unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("renewal follow-up".into()))
        );
        assert_eq!(store.resolve("sess", &draft).unwrap(), Some(vid));

        // the view projects a summary, not the raw value bytes
        let view = store.view("sess").unwrap();
        assert_eq!(view.symbols.len(), 1);
        assert_eq!(view.symbols[0].name, draft);
        assert_eq!(view.symbols[0].summary, "renewal follow-up");

        let events = store.events("sess").unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::StepStarted { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::StepSucceeded { .. })));
    }

    #[tokio::test]
    async fn two_face_result_binds_canonical_shows_view() {
        // The two-face invariant: the bound symbol value (and `{{interpolation}}` source) is the
        // CANONICAL content, while the model/sink-facing outcome carries the distinct VIEW.
        let store = FlowStore::in_memory().unwrap();
        let dir = std::env::temp_dir().join(format!("flux-flow-twoface-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TwoFaceTool));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["twoface".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        let sym = SymbolName("x".into());
        let outcome = execute_call(
            &store,
            &ex,
            "sess",
            "twoface",
            json!({}),
            Some(BindSpec {
                name: &sym,
                ty: None,
                visibility: Visibility::Visible,
            }),
        )
        .await
        .unwrap();

        assert_eq!(outcome.content, "RAW", "canonical content");
        assert_eq!(outcome.view, "VIEW", "model-facing view");
        // The STORED/interpolated value is the canonical content, never the view.
        let vid = outcome.value_id.clone().unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("RAW".into()))
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn denied_op_is_traced_as_failed_and_not_bound() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(false);
        let draft = SymbolName("draft".into());

        let outcome = execute_call(
            &store,
            &ex,
            "sess",
            "echo",
            json!({"text": "x"}),
            Some(BindSpec {
                name: &draft,
                ty: Some("Draft"),
                visibility: Visibility::Visible,
            }),
        )
        .await
        .unwrap();

        assert!(outcome.is_error, "a denied op yields an error outcome");
        assert!(outcome.value_id.is_none());
        assert_eq!(store.resolve("sess", &draft).unwrap(), None);
        let events = store.events("sess").unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::StepFailed { .. })));
    }

    // ---- flow execution + risk (linear v1) ----

    /// A sink that records the op names it was told about.
    #[derive(Default)]
    struct CollectSink {
        calls: Vec<String>,
    }
    impl AgentSink for CollectSink {
        fn tool_call(&mut self, name: &str, _input: &serde_json::Value) {
            self.calls.push(name.to_string());
        }
    }

    fn flow_bind(name: &str, op: &str, args: Vec<Node>) -> Node {
        Node::Bind {
            name: SymbolName(name.into()),
            value: Box::new(Node::Call {
                op: op.into(),
                args,
            }),
            ty: None,
            effect: None,
        }
    }
    fn flow_lit(v: serde_json::Value) -> Node {
        Node::Lit { value: v }
    }
    fn flow_var(name: &str) -> Node {
        Node::Var {
            name: SymbolName(name.into()),
        }
    }

    #[tokio::test]
    async fn execute_flow_runs_a_linear_plan_through_dispatch() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // $a = echo("hi"); $b = echo($a); return $b
        let ast = DraftAst {
            body: vec![
                flow_bind("a", "echo", vec![flow_lit(json!("hi"))]),
                flow_bind("b", "echo", vec![flow_var("a")]),
                Node::Return {
                    value: Box::new(flow_var("b")),
                },
            ],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();

        assert_eq!(outcome.steps, 2, "both echo ops dispatched");
        assert_eq!(outcome.result, "hi");
        assert_eq!(sink.calls, vec!["echo", "echo"]);
        // $b holds the value $a flowed into it (symbols carried the value, not the prose).
        let vid = store
            .resolve("sess", &SymbolName("b".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("hi".into()))
        );
        // The trace records the return.
        assert!(store
            .events("sess")
            .unwrap()
            .iter()
            .any(|e| matches!(e, RunEvent::FlowReturned { .. })));
    }

    #[tokio::test]
    async fn execute_flow_transcript_carries_every_node_not_just_last() {
        // The round feedback must surface ALL of a plan's reads, not just the last node — otherwise a
        // multi-read plan loops (the model re-reads what it couldn't see). `transcript` is that feed.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // $a = echo("alpha"); $b = echo("beta")  (no return)
        let ast = DraftAst {
            body: vec![
                flow_bind("a", "echo", vec![flow_lit(json!("alpha"))]),
                flow_bind("b", "echo", vec![flow_lit(json!("beta"))]),
            ],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            outcome.result, "beta",
            "result is still the LAST node's view"
        );
        assert!(
            outcome.transcript.contains("alpha") && outcome.transcript.contains("beta"),
            "transcript must carry BOTH nodes, got: {}",
            outcome.transcript
        );
        assert!(
            outcome.transcript.contains("[$a = echo]"),
            "transcript labels each node by its bound symbol"
        );
    }

    #[tokio::test]
    async fn execute_flow_when_takes_the_true_branch() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // when true { $taken = echo("then") } else { $taken = echo("else") }
        let ast = DraftAst {
            body: vec![Node::When {
                cond: Box::new(flow_lit(json!(true))),
                then: vec![flow_bind("taken", "echo", vec![flow_lit(json!("then"))])],
                otherwise: vec![flow_bind("taken", "echo", vec![flow_lit(json!("else"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 1, "only the taken branch's op runs");
        let vid = store
            .resolve("sess", &SymbolName("taken".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("then".into()))
        );
    }

    #[tokio::test]
    async fn execute_flow_when_takes_the_false_branch() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::When {
                cond: Box::new(flow_lit(json!(false))),
                then: vec![flow_bind("taken", "echo", vec![flow_lit(json!("then"))])],
                otherwise: vec![flow_bind("taken", "echo", vec![flow_lit(json!("else"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        let vid = store
            .resolve("sess", &SymbolName("taken".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("else".into()))
        );
    }

    #[tokio::test]
    async fn execute_flow_repeat_caps_at_max_and_until_breaks_early() {
        // repeat max 3 { echo } → runs 3 times (no `until`).
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Repeat {
                max: 3,
                until: None,
                body: vec![flow_bind("x", "echo", vec![flow_lit(json!("hi"))])],
                collect: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 3, "repeat runs exactly max times");

        // repeat max 3 until true { echo } → the always-true guard stops it after one iteration.
        let store = FlowStore::in_memory().unwrap();
        let ast = DraftAst {
            body: vec![Node::Repeat {
                max: 3,
                until: Some(Box::new(flow_lit(json!(true)))),
                body: vec![flow_bind("x", "echo", vec![flow_lit(json!("hi"))])],
                collect: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            outcome.steps, 1,
            "`until` true after iteration 1 breaks the loop"
        );
    }

    #[tokio::test]
    async fn execute_flow_repeat_collects_each_iterations_result() {
        // repeat max 3 { $x = echo("hi") } collect $all → $all is the ordered list of results.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Repeat {
                max: 3,
                until: None,
                body: vec![flow_bind("x", "echo", vec![flow_lit(json!("hi"))])],
                collect: Some(SymbolName("all".into())),
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 3, "repeat runs exactly max times");
        // `collect` bound a list of every iteration's last result, in order.
        let vid = store
            .resolve("sess", &SymbolName("all".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::List(vec![
                Value::String("hi".into()),
                Value::String("hi".into()),
                Value::String("hi".into()),
            ]))
        );
    }

    #[tokio::test]
    async fn execute_flow_still_errors_on_await() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Await {
                binding: None,
                source: "input".into(),
                as_type: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("await"));
    }

    #[test]
    fn plan_risk_flags_destructive_and_mutating() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);

        // bash "rm -rf build" (destructive) + write (mutating).
        let ast = DraftAst {
            body: vec![
                flow_bind("x", "bash", vec![flow_lit(json!("rm -rf build"))]),
                flow_bind(
                    "y",
                    "write",
                    vec![flow_lit(json!("out.txt")), flow_lit(json!("data"))],
                ),
            ],
            ..Default::default()
        };
        let risk = plan_risk(&ast, &reg);
        assert!(risk.destructive, "rm -rf is destructive-shaped");
        assert!(risk.mutating);
        assert_eq!(risk.max_risk, Some(Risk::High)); // bash is High, write Medium
        assert_eq!(risk.ops, vec!["bash".to_string(), "write".to_string()]);

        // A read-only plan is neither destructive nor mutating.
        let safe = DraftAst {
            body: vec![flow_bind("r", "read", vec![flow_lit(json!("README.md"))])],
            ..Default::default()
        };
        let risk = plan_risk(&safe, &reg);
        assert!(!risk.destructive);
        assert!(!risk.mutating);
        assert_eq!(risk.max_risk, Some(Risk::Low));
    }

    #[tokio::test]
    async fn plan_approver_allows_approved_nondestructive_and_escalates_destructive() {
        use flux_spec::{Intent, IntentBehavior, IntentCertainty, IntentRole, IntentTarget};
        use std::sync::atomic::{AtomicBool, Ordering};

        /// A fallback approver that records being consulted, then denies.
        struct Recording {
            hit: AtomicBool,
        }
        #[async_trait]
        impl Approver for Recording {
            async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
                self.hit.store(true, Ordering::Relaxed);
                ApprovalChoice::Deny
            }
        }

        let fallback = Arc::new(Recording {
            hit: AtomicBool::new(false),
        });

        // An approved, non-destructive op is allowed without consulting the fallback.
        let approver = PlanApprover::new(["write".to_string()], fallback.clone());
        let empty = IntentSet::new();
        assert!(matches!(
            approver.request("write", &[], &empty).await,
            ApprovalChoice::Allow
        ));
        assert!(
            !fallback.hit.load(Ordering::Relaxed),
            "approved op must not prompt"
        );

        // A destructive op falls through to the fallback even though it is in the approved set.
        let mut destructive = IntentSet::new();
        destructive.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "rm -rf /".into(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        let approver = PlanApprover::new(["bash".to_string()], fallback.clone());
        assert!(matches!(
            approver.request("bash", &[], &destructive).await,
            ApprovalChoice::Deny
        ));
        assert!(
            fallback.hit.load(Ordering::Relaxed),
            "a destructive op must still escalate to the fallback"
        );
    }

    // ---- expanded node kinds (each / assert / pipe / seq / memo / parallel) ----

    #[tokio::test]
    async fn execute_flow_each_iterates_list_and_collects() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // each $f in ["a","b"] { $t = echo($f) } collect $all
        let ast = DraftAst {
            body: vec![Node::Each {
                source: Box::new(flow_lit(json!(["a", "b"]))),
                item: SymbolName("f".into()),
                body: vec![flow_bind("t", "echo", vec![flow_var("f")])],
                collect: Some(SymbolName("all".into())),
                flat: false,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 2, "body runs once per element");
        assert_eq!(sink.calls, vec!["echo", "echo"]);
        // echo echoes $f, so the last iteration's view is "b".
        assert_eq!(outcome.result, "b");
        // `collect` bound a list of the per-iteration results, in order.
        let vid = store
            .resolve("sess", &SymbolName("all".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::List(vec![
                Value::String("a".into()),
                Value::String("b".into()),
            ]))
        );
    }

    #[tokio::test]
    async fn execute_flow_each_rejects_a_non_list_source() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Each {
                source: Box::new(flow_lit(json!("not a list"))),
                item: SymbolName("f".into()),
                body: vec![],
                collect: None,
                flat: false,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("list"));
    }

    #[tokio::test]
    async fn execute_flow_assert_passes_when_true_and_aborts_when_false() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ok = DraftAst {
            body: vec![Node::Assert {
                cond: Box::new(flow_lit(json!(true))),
                message: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        assert!(execute_flow(&store, &ex, "sess", &ok, &mut sink)
            .await
            .is_ok());

        let bad = DraftAst {
            body: vec![Node::Assert {
                cond: Box::new(flow_lit(json!(false))),
                message: Some("nope".into()),
            }],
            ..Default::default()
        };
        let err = execute_flow(&store, &ex, "sess", &bad, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("assertion failed"));
        assert!(err.to_string().contains("nope"));
    }

    #[tokio::test]
    async fn execute_flow_pipe_feeds_output_as_next_first_arg() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // pipe { echo("alpha"); echo() } -> $out  (the 2nd echo gets "alpha" as its first arg)
        let ast = DraftAst {
            body: vec![Node::Pipe {
                steps: vec![
                    Node::Call {
                        op: "echo".into(),
                        args: vec![flow_lit(json!("alpha"))],
                    },
                    Node::Call {
                        op: "echo".into(),
                        args: vec![],
                    },
                ],
                bind: Some(SymbolName("out".into())),
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 2);
        assert_eq!(
            outcome.result, "alpha",
            "the second step received the first's output as its first argument"
        );
        let vid = store
            .resolve("sess", &SymbolName("out".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("alpha".into()))
        );
    }

    #[tokio::test]
    async fn execute_flow_seq_runs_body_and_binds_last() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // seq { echo("one"); $two = echo("two") } -> $last
        let ast = DraftAst {
            body: vec![Node::Seq {
                body: vec![
                    Node::Call {
                        op: "echo".into(),
                        args: vec![flow_lit(json!("one"))],
                    },
                    flow_bind("two", "echo", vec![flow_lit(json!("two"))]),
                ],
                bind: Some(SymbolName("last".into())),
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 2);
        let vid = store
            .resolve("sess", &SymbolName("last".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("two".into())),
            "`bind` captures the block's final value"
        );
    }

    #[tokio::test]
    async fn execute_flow_memo_computes_once_per_session() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Memo {
                name: SymbolName("survey".into()),
                value: Box::new(Node::Call {
                    op: "echo".into(),
                    args: vec![flow_lit(json!("expensive"))],
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        // First run dispatches and binds.
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 1, "first memo run dispatches");
        assert_eq!(sink.calls, vec!["echo"]);

        // Second run on the SAME session+symbol reuses the cache — no dispatch.
        let mut sink2 = CollectSink::default();
        let outcome2 = execute_flow(&store, &ex, "sess", &ast, &mut sink2)
            .await
            .unwrap();
        assert_eq!(outcome2.steps, 0, "a memo hit skips execution");
        assert!(sink2.calls.is_empty(), "no op dispatched on a memo hit");
        assert_eq!(outcome2.result, "expensive", "the cached value is reused");

        // A different session is a fresh memo.
        let mut sink3 = CollectSink::default();
        let outcome3 = execute_flow(&store, &ex, "other", &ast, &mut sink3)
            .await
            .unwrap();
        assert_eq!(outcome3.steps, 1, "a different session recomputes");
    }

    #[tokio::test]
    async fn execute_flow_parallel_runs_branches_and_binds_names() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![
                    crate::ast::Branch {
                        name: SymbolName("left".into()),
                        body: vec![Node::Call {
                            op: "echo".into(),
                            args: vec![flow_lit(json!("L"))],
                        }],
                    },
                    crate::ast::Branch {
                        name: SymbolName("right".into()),
                        body: vec![Node::Call {
                            op: "echo".into(),
                            args: vec![flow_lit(json!("R"))],
                        }],
                    },
                ],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 2, "both branches' ops dispatched");
        // Each branch bound its result to its name.
        let l = store
            .resolve("sess", &SymbolName("left".into()))
            .unwrap()
            .unwrap();
        let r = store
            .resolve("sess", &SymbolName("right".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&l).unwrap(),
            Some(Value::String("L".into()))
        );
        assert_eq!(
            store.get_value(&r).unwrap(),
            Some(Value::String("R".into()))
        );
        // The branches' buffered sink events were replayed into the real sink.
        assert_eq!(sink.calls.len(), 2, "both branch tool-calls replayed");
        assert!(sink.calls.iter().all(|c| c == "echo"));
    }

    #[test]
    fn plan_risk_walks_each_and_parallel_bodies() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);
        // A destructive bash nested in `each` + a mutating write nested in a `parallel` branch.
        let ast = DraftAst {
            body: vec![
                Node::Each {
                    source: Box::new(flow_lit(json!(["x"]))),
                    item: SymbolName("f".into()),
                    body: vec![flow_bind(
                        "d",
                        "bash",
                        vec![flow_lit(json!("rm -rf build"))],
                    )],
                    collect: None,
                    flat: false,
                },
                Node::Parallel {
                    branches: vec![crate::ast::Branch {
                        name: SymbolName("w".into()),
                        body: vec![flow_bind(
                            "o",
                            "write",
                            vec![flow_lit(json!("out.txt")), flow_lit(json!("data"))],
                        )],
                    }],
                },
            ],
            ..Default::default()
        };
        let risk = plan_risk(&ast, &reg);
        assert!(
            risk.destructive,
            "rm -rf inside `each` is seen by the risk walk"
        );
        assert!(
            risk.mutating,
            "write inside a `parallel` branch is seen by the walk"
        );
        assert!(
            risk.ops.contains(&"bash".to_string()) && risk.ops.contains(&"write".to_string()),
            "the walk recurses into the new container nodes"
        );
    }

    // ---- new node kinds: retry / try / confirm / loop / race / throttle / debounce ----

    #[tokio::test]
    async fn execute_flow_retry_succeeds_on_first_attempt() {
        // retry max 3: body always succeeds → runs once, result is the echo output.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Retry {
                max: 3,
                backoff: None,
                delay_ms: None,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("ok"))])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "ok");
        assert_eq!(sink.calls, vec!["echo"]);
    }

    #[tokio::test]
    async fn execute_flow_try_catch_runs_handler_on_error() {
        // try { echo("good") } catch $e — body succeeds, handler not reached.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ok_ast = DraftAst {
            body: vec![Node::Try {
                catch: None,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("good"))])],
                handler: vec![],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_try_ok", &ok_ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "good");
        // handler nodes not executed
        assert_eq!(sink.calls, vec!["echo"]);
    }

    #[tokio::test]
    async fn execute_flow_try_handler_runs_when_body_errors() {
        // try { unknown_op() } catch { echo("caught") } — body errors, handler runs.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let err_ast = DraftAst {
            body: vec![Node::Try {
                catch: None,
                body: vec![Node::Call {
                    op: "this_op_does_not_exist".into(),
                    args: vec![],
                }],
                handler: vec![flow_bind("h", "echo", vec![flow_lit(json!("caught"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_try_err", &err_ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "caught");
        assert_eq!(sink.calls, vec!["echo"]);
    }

    #[tokio::test]
    async fn execute_flow_confirm_allow_runs_body() {
        // An auto-allow executor: confirm should proceed and run the body.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true); // auto-approve = true
        let ast = DraftAst {
            body: vec![Node::Confirm {
                message: "proceed?".into(),
                risk: Some("low".into()),
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("confirmed"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_confirm_ok", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "confirmed");
    }

    #[tokio::test]
    async fn execute_flow_confirm_deny_returns_error() {
        // A denying *approver* (with `echo` permitted, so the body would otherwise run): `confirm`
        // must error and short-circuit before the body. (Using a perm-denied echo would test the
        // wrong thing — the denial has to come from the confirm gate itself.)
        struct DenyApprover;
        #[async_trait]
        impl Approver for DenyApprover {
            async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
                ApprovalChoice::Deny
            }
        }
        let dir =
            std::env::temp_dir().join(format!("flux-flow-rt-{}-confirmdeny", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["echo".into()], &[]),
            Arc::new(DenyApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        let store = FlowStore::in_memory().unwrap();
        let ast = DraftAst {
            body: vec![Node::Confirm {
                message: "dangerous action".into(),
                risk: Some("high".into()),
                body: vec![flow_bind(
                    "r",
                    "echo",
                    vec![flow_lit(json!("should not run"))],
                )],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess_confirm_deny", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("confirm"), "got: {err}");
        assert!(sink.calls.is_empty(), "body must not run when denied");
    }

    #[tokio::test]
    async fn execute_flow_confirm_allow_always_runs_body() {
        // "Allow & always" (`ApprovalChoice::AllowAlways`) is an approval — the confirm body must run.
        // Regression: the engine→language approval adapter once mapped `AllowAlways` to `Deny`.
        struct AllowAlwaysApprover;
        #[async_trait]
        impl Approver for AllowAlwaysApprover {
            async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
                ApprovalChoice::AllowAlways("confirm".into())
            }
        }
        let dir =
            std::env::temp_dir().join(format!("flux-flow-rt-{}-confirmalways", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["echo".into()], &[]),
            Arc::new(AllowAlwaysApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        let store = FlowStore::in_memory().unwrap();
        let ast = DraftAst {
            body: vec![Node::Confirm {
                message: "proceed?".into(),
                risk: Some("medium".into()),
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("did run"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_confirm_always", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            outcome.result, "did run",
            "AllowAlways must run the confirm body"
        );
    }

    #[tokio::test]
    async fn execute_flow_loop_runs_until_deadline() {
        // loop for 50ms every 0ms: body runs at least once; deadline stops it.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Loop {
                for_ms: 50,
                every_ms: 0,
                until: None,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("tick"))])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        execute_flow(&store, &ex, "sess_loop", &ast, &mut sink)
            .await
            .unwrap();
        assert!(!sink.calls.is_empty(), "body must have run at least once");
        assert!(sink.calls.iter().all(|c| c == "echo"));
    }

    #[tokio::test]
    async fn execute_flow_loop_stops_on_until_condition() {
        // loop for 10_000ms every 0ms until lit(true): body runs exactly once then stops.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Loop {
                for_ms: 10_000,
                every_ms: 0,
                until: Some(Box::new(flow_lit(json!(true)))),
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("tick"))])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        execute_flow(&store, &ex, "sess_loop_until", &ast, &mut sink)
            .await
            .unwrap();
        // `until` is checked after the first iteration, so body runs exactly once.
        assert_eq!(sink.calls, vec!["echo"]);
    }

    #[tokio::test]
    async fn execute_flow_race_returns_first_success() {
        // race timeout=1000ms: first branch succeeds → result is first branch's echo.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Race {
                timeout_ms: 1_000,
                bind: Some(SymbolName("winner".into())),
                branches: vec![
                    crate::ast::Branch {
                        name: SymbolName("a".into()),
                        body: vec![flow_bind("ra", "echo", vec![flow_lit(json!("first"))])],
                    },
                    crate::ast::Branch {
                        name: SymbolName("b".into()),
                        body: vec![flow_bind("rb", "echo", vec![flow_lit(json!("second"))])],
                    },
                ],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_race", &ast, &mut sink)
            .await
            .unwrap();
        // The first branch always succeeds, so we get "first".
        assert_eq!(outcome.result, "first");
    }

    #[tokio::test]
    async fn execute_flow_race_errors_when_deadline_exceeded() {
        // race timeout=0ms: deadline is already past before any branch runs.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Race {
                timeout_ms: 0,
                bind: None,
                branches: vec![crate::ast::Branch {
                    name: SymbolName("a".into()),
                    body: vec![flow_bind("r", "echo", vec![flow_lit(json!("x"))])],
                }],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess_race_timeout", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_flow_throttle_allows_under_limit() {
        // throttle max=5 window=60000ms: a single call is well within the limit.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Throttle {
                name: "test_throttle_ok".to_string(),
                max: 5,
                window_ms: 60_000,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("ok"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_throttle_ok", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "ok");
    }

    #[tokio::test]
    async fn execute_flow_throttle_rejects_over_limit() {
        // throttle max=1 window=60000ms: run the AST twice in the same session → second is rejected.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Throttle {
                name: "test_throttle_limit".to_string(),
                max: 1,
                window_ms: 60_000,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("ok"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        // First run: succeeds.
        execute_flow(&store, &ex, "sess_throttle_limit", &ast, &mut sink)
            .await
            .unwrap();
        // Second run in the same session/window: should be rejected.
        let err = execute_flow(&store, &ex, "sess_throttle_limit", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("throttle"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_flow_debounce_runs_body_after_delay() {
        // debounce wait=0ms: body runs (zero delay is fine).
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Debounce {
                name: "test_debounce".to_string(),
                wait_ms: 0,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("debounced"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_debounce", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "debounced");
        assert_eq!(sink.calls, vec!["echo"]);
    }
}
