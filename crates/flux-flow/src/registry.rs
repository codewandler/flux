//! The runtime adapter over the existing tool registry: a read-only [`OpRegistry`] view that presents
//! `flux_runtime::ToolRegistry` as a catalog of Flux-Lang operations, plus the [`ThingResolver`] /
//! [`ModelClient`] seams the runtime and model-backed operations use.
//!
//! The pure op contracts ([`OpSpec`], [`OpSignature`], [`OpCatalog`], [`schema_params`]) live in
//! `flux-lang`; this module re-exports them so existing `flux_flow::registry::*` paths keep working.
//! Every Flux-Lang operation is a `flux_runtime::Tool` under the hood, so it executes through
//! `Executor::dispatch` like any other tool — the safety envelope is reused, not bypassed.

use std::borrow::Cow;
use std::collections::HashSet;

use async_trait::async_trait;

use flux_lang::analyze::{for_each_node, lower, Diagnostic};
use flux_lang::ast::FlowEffect;
use flux_runtime::{Tool, ToolRegistry};
use flux_spec::{Effect, Risk};

use crate::ast::{Node, ResolvedThing, ThingRef};
pub use flux_lang::opspec::{schema_params, OpCatalog, OpSignature, OpSpec};
use flux_lang::program::CompositeOpDecl;

/// A read-only adapter presenting the existing [`ToolRegistry`] as a registry of operations the
/// analyzer and typed stages can target. Existing tools *are* operations — no separate registration is required.
///
/// `advertised` optionally restricts the **catalog** (`op_names`/`signatures` — what the model is
/// shown) to a precomputed allow-set of op names (evidence-gated surfacing; see
/// [`flux_runtime::advertised_op_names`]). `None` means advertise everything. `get` is *never*
/// filtered, so a pre-authored flow that references a hidden-group op still resolves and executes.
pub struct OpRegistry<'a> {
    tools: &'a ToolRegistry,
    composites: Cow<'a, [CompositeOpDecl]>,
    advertised: Option<HashSet<String>>,
    assumed: HashSet<String>,
}

impl<'a> OpRegistry<'a> {
    /// Wrap a tool registry, advertising every op (no gating).
    pub fn new(tools: &'a ToolRegistry) -> Self {
        Self {
            tools,
            composites: Cow::Borrowed(&[]),
            advertised: None,
            assumed: HashSet::new(),
        }
    }

    /// Resolve `names` even when no tool backs them, with an unconstrained signature.
    ///
    /// This exists for **analysis without assembly**: some operations are only registered once a
    /// live agent is assembled — the datasource pack (`search`/`get`/`sources`/…) needs configured
    /// sources, and the language toolchains surface on a workspace signal — so a static catalog
    /// cannot decide whether they exist. A caller that already holds an authoritative grant for
    /// those names (a Fleet capability ceiling, say) supplies them here so analysis reports real
    /// defects instead of false "unknown operation" noise.
    ///
    /// Assumed names carry no parameter or type information, so argument checks on them degrade to
    /// permissive. Everything structural — symbol binding, `match` subjects, control flow — is still
    /// checked, and a tool-backed op always wins over an assumed one.
    pub fn with_assumed_ops(mut self, names: HashSet<String>) -> Self {
        self.assumed = names;
        self
    }

    fn assumed_signature(&self, name: &str) -> Option<OpSignature> {
        if !self.assumed.contains(name) {
            return None;
        }
        Some(OpSignature::from_spec(&flux_spec::ToolSpec {
            name: name.to_string(),
            description: format!("assumed operation `{name}` (granted, not statically registered)"),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: None,
            effects: Vec::new(),
            risk: Risk::Medium,
            idempotency: flux_spec::Idempotency::NonIdempotent,
            access: Vec::new(),
            group: None,
        }))
    }

    /// Add module-local composite ops to the catalog. Tool lookup still wins on name collision; callers
    /// should validate duplicate names before installing a module.
    pub fn with_composites(mut self, composites: &'a [CompositeOpDecl]) -> Self {
        self.composites = Cow::Borrowed(composites);
        self
    }

    /// Add an owned snapshot of composite ops to the catalog.
    pub fn with_owned_composites(mut self, composites: Vec<CompositeOpDecl>) -> Self {
        self.composites = Cow::Owned(composites);
        self
    }

    /// Restrict the advertised catalog to `advertised` (the surfaced op names). Execution/resolution
    /// via [`get`](Self::get) is unaffected.
    pub fn with_advertised(mut self, advertised: HashSet<String>) -> Self {
        self.advertised = Some(advertised);
        self
    }

    fn is_advertised(&self, name: &str) -> bool {
        self.advertised.as_ref().is_none_or(|a| a.contains(name))
    }

    /// The names of every **advertised** operation, sorted (model-stage catalogs must be byte-stable).
    pub fn op_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tools
            .names()
            .into_iter()
            .filter(|n| self.is_advertised(n))
            .collect();
        names.extend(
            self.composites
                .iter()
                .filter(|c| c.meta.expose)
                .map(|c| c.name.clone()),
        );
        names.sort();
        names
    }

    /// The signature of every **advertised** operation, name-sorted. Stable order preserves provider
    /// prompt caching for model-stage catalogs.
    pub fn signatures(&self) -> Vec<OpSignature> {
        let mut signatures: Vec<OpSignature> = self
            .tools
            .names()
            .into_iter()
            .filter(|n| self.is_advertised(n))
            .filter_map(|n| self.tools.get(&n))
            .map(|t| signature_for_tool(t.as_ref()))
            .collect();
        signatures.extend(
            self.composites
                .iter()
                .filter(|c| c.meta.expose)
                .map(composite_signature),
        );
        signatures.sort_by(|a, b| a.name.cmp(&b.name));
        signatures
    }

    /// The signature of one operation, if registered. Not filtered by surfacing — resolution must
    /// succeed for any registered op a flow names.
    pub fn get(&self, name: &str) -> Option<OpSignature> {
        self.tools
            .get(name)
            .map(|t| signature_for_tool(t.as_ref()))
            .or_else(|| {
                self.composites
                    .iter()
                    .find(|c| c.name == name)
                    .map(composite_signature)
            })
            .or_else(|| self.assumed_signature(name))
    }
}

impl OpCatalog for OpRegistry<'_> {
    /// Delegate to the unfiltered [`get`](OpRegistry::get) so analysis accepts any registered op,
    /// advertised or not.
    fn lookup(&self, name: &str) -> Option<OpSignature> {
        self.get(name)
    }

    fn param_format(&self, op: &str, param: &str) -> Option<String> {
        self.tools.get(op).and_then(|tool| {
            tool.spec()
                .input_schema
                .get("properties")
                .and_then(|v| v.as_object())
                .and_then(|props| props.get(param))
                .and_then(|prop| prop.get("format"))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
    }

    fn composite(&self, name: &str) -> Option<CompositeOpDecl> {
        self.composites.iter().find(|c| c.name == name).cloned()
    }
}

/// Derive a tool's [`OpSignature`], folding its declared semantic-effect tags
/// ([`Tool::semantic_effects`]) into [`OpSignature::semantic_effects`] — the D-138 manifest→catalog
/// adapter: a plugin's manifest-declared `OperationSpec::semantic_effects` (`Money`/`Delete`/
/// `SendExternal`, or any built-in `Tool` impl that overrides the same hook) survives from the
/// registered tool all the way to the catalog the analyzer and typed stages see, with no authored `effect:`
/// tag required at the call site. Unknown tags are silently dropped rather than erroring — a
/// forward-compatible tag from a newer catalog degrades to "no extra semantic tier" instead of
/// breaking resolution.
fn signature_for_tool(tool: &dyn Tool) -> OpSignature {
    let mut sig = OpSignature::from_spec(&tool.spec());
    let mut semantic: Vec<FlowEffect> = Vec::new();
    for tag in tool.semantic_effects() {
        if let Some(e) = FlowEffect::from_tag(&tag) {
            if !semantic.contains(&e) {
                semantic.push(e);
            }
        }
    }
    sig.semantic_effects = semantic;
    sig
}

fn composite_signature(op: &CompositeOpDecl) -> OpSignature {
    let mut param_types = std::collections::BTreeMap::new();
    for p in &op.params {
        param_types.insert(p.name.0.clone(), p.ty.clone());
    }
    OpSignature {
        name: op.name.clone(),
        description: op.meta.description.clone(),
        effects: op.meta.effects.clone(),
        risk: op.meta.risk,
        idempotency: op.meta.idempotency,
        required_params: op.params.iter().map(|p| p.name.0.clone()).collect(),
        optional_params: Vec::new(),
        param_types,
        output: op.returns.clone().unwrap_or(flux_lang::ast::TypeRef::Any),
        // Composite ops don't yet have their own declared semantic-effect tier (they compose
        // existing ops rather than being a leaf `OpSpec`/`OperationSpec`); leave empty for now.
        semantic_effects: Vec::new(),
    }
}

/// Validate module-local composite ops against the live tool registry and each other.
pub fn analyze_composites(
    composites: &[CompositeOpDecl],
    tools: &ToolRegistry,
) -> Result<(), Vec<Diagnostic>> {
    let catalog = OpRegistry::new(tools).with_composites(composites);
    let mut diags = Vec::new();
    let mut names = HashSet::new();
    for op in composites {
        diags.extend(analyze_one_structural(op, tools, &catalog));
        if !names.insert(op.name.clone()) {
            diags.push(Diagnostic::new(format!(
                "duplicate composite op `{}`",
                op.name
            )));
        }
    }
    detect_composite_cycles(composites, &mut diags);
    for op in composites {
        diags.extend(analyze_one_surface(op, &catalog));
    }
    if diags.is_empty() {
        Ok(())
    } else {
        Err(diags)
    }
}

/// Per-composite structural diagnostics against a catalog: registered-tool name conflict, full
/// lowering (the typed gate any authored flow passes — unknown ops surface here), and the v1
/// `await` ban. Shared by strict [`analyze_composites`] and the C-117 pruning pass.
fn analyze_one_structural(
    op: &CompositeOpDecl,
    tools: &ToolRegistry,
    catalog: &OpRegistry<'_>,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    if tools.get(&op.name).is_some() {
        diags.push(Diagnostic::new(format!(
            "composite op `{}` conflicts with a registered tool",
            op.name
        )));
    }
    // Composites validate through the same typed gate as any authored flow (L-16/F9): full
    // structural analysis + lowering. Params seed the definedness scope; no session set.
    lower(&op.body, catalog, &std::collections::HashSet::new())
        .map(|_| ())
        .unwrap_or_else(|mut e| diags.append(&mut e));
    if body_contains_await(&op.body.body) {
        diags.push(Diagnostic::new(format!(
            "composite op `{}` cannot contain `await` in v1",
            op.name
        )));
    }
    diags
}

/// Per-composite declared-surface diagnostics: the body's transitive risk/effects must be covered
/// by the op's own declaration. Shared by strict [`analyze_composites`] and the C-117 pruning pass.
fn analyze_one_surface(op: &CompositeOpDecl, catalog: &OpRegistry<'_>) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    if let Some((risk, effects)) = transitive_surface(&op.body.body, catalog) {
        if op.meta.risk < risk {
            diags.push(Diagnostic::new(format!(
                "composite op `{}` declares risk {:?} but body requires {:?}",
                op.name, op.meta.risk, risk
            )));
        }
        for effect in effects {
            if !op.meta.effects.contains(&effect) {
                diags.push(Diagnostic::new(format!(
                    "composite op `{}` missing declared effect {:?}",
                    op.name, effect
                )));
            }
        }
    }
    diags
}

/// Per-composite diagnostics shared by [`analyze_composites`] and the C-117 pruning pass:
/// structural checks plus the declared-surface check, against the given catalog.
fn analyze_one(
    op: &CompositeOpDecl,
    tools: &ToolRegistry,
    catalog: &OpRegistry<'_>,
) -> Vec<Diagnostic> {
    let mut diags = analyze_one_structural(op, tools, catalog);
    diags.extend(analyze_one_surface(op, catalog));
    diags
}

/// One pruning round (C-117): each composite that does not resolve against `tools` + the given
/// set — including every participant of a composite→composite cycle — is returned with its joined
/// diagnostics. Callers iterate to a fixed point: removing a callee can invalidate its callers.
pub fn unresolvable_composites(
    composites: &[CompositeOpDecl],
    tools: &ToolRegistry,
) -> Vec<(String, String)> {
    let catalog = OpRegistry::new(tools).with_composites(composites);
    let cyclic = composite_cycle_participants(composites);
    let mut out = Vec::new();
    for op in composites {
        let mut reasons: Vec<String> = analyze_one(op, tools, &catalog)
            .into_iter()
            .map(|d| d.message)
            .collect();
        if let Some(cycle) = cyclic.get(&op.name) {
            reasons.push(cycle.clone());
        }
        if !reasons.is_empty() {
            out.push((op.name.clone(), reasons.join("; ")));
        }
    }
    out
}

/// Every composite participating in a composite→composite cycle, mapped to its cycle description.
fn composite_cycle_participants(
    composites: &[CompositeOpDecl],
) -> std::collections::HashMap<String, String> {
    let mut diags = Vec::new();
    detect_composite_cycles(composites, &mut diags);
    let mut out = std::collections::HashMap::new();
    for diag in diags {
        // Message shape (pinned by detect_composite_cycles): "recursive composite op cycle: a -> b -> a".
        if let Some(path) = diag.message.strip_prefix("recursive composite op cycle: ") {
            for name in path.split(" -> ") {
                out.entry(name.to_string()).or_insert(diag.message.clone());
            }
        }
    }
    out
}

fn body_contains_await(body: &[Node]) -> bool {
    let mut found = false;
    for_each_node(body, &mut |node| {
        if matches!(node, Node::Await { .. }) {
            found = true;
        }
    });
    found
}

fn called_composites(body: &[Node], catalog: &OpRegistry<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for_each_node(body, &mut |node| {
        if let Node::Call { op, .. } = node {
            if catalog.composite(op).is_some() && !out.contains(op) {
                out.push(op.clone());
            }
        }
    });
    out
}

fn detect_composite_cycles(composites: &[CompositeOpDecl], diags: &mut Vec<Diagnostic>) {
    let tools = ToolRegistry::new();
    let catalog = OpRegistry::new(&tools).with_composites(composites);
    for op in composites {
        let mut stack = Vec::new();
        visit_composite(op, &catalog, &mut stack, diags);
    }
}

fn visit_composite(
    op: &CompositeOpDecl,
    catalog: &OpRegistry<'_>,
    stack: &mut Vec<String>,
    diags: &mut Vec<Diagnostic>,
) {
    if stack.contains(&op.name) {
        let mut cycle = stack.clone();
        cycle.push(op.name.clone());
        diags.push(Diagnostic::new(format!(
            "recursive composite op cycle: {}",
            cycle.join(" -> ")
        )));
        return;
    }
    stack.push(op.name.clone());
    for name in called_composites(&op.body.body, catalog) {
        if let Some(next) = catalog.composite(&name) {
            visit_composite(&next, catalog, stack, diags);
        }
    }
    stack.pop();
}

fn transitive_surface(body: &[Node], catalog: &OpRegistry<'_>) -> Option<(Risk, Vec<Effect>)> {
    let mut max_risk: Option<Risk> = None;
    let mut effects = Vec::new();
    for_each_node(body, &mut |node| {
        let Node::Call { op, .. } = node else {
            return;
        };
        let Some(sig) = catalog.lookup(op) else {
            return;
        };
        max_risk = Some(max_risk.map_or(sig.risk, |r| r.max(sig.risk)));
        for effect in sig.effects {
            if !effects.contains(&effect) {
                effects.push(effect);
            }
        }
    });
    max_risk.map(|risk| (risk, effects))
}

/// Resolves an unresolved [`ThingRef`] to an exact identity. Implementations live outside this crate
/// (they perform IO); the runtime resolves things at execution time before any side effect.
#[async_trait]
pub trait ThingResolver: Send + Sync {
    /// Resolve a thing reference to an exact identity.
    async fn resolve(&self, thing: &ThingRef) -> flux_core::Result<ResolvedThing>;
}

/// The narrow seam a `!model` operation uses to call a provider from inside the execution envelope —
/// a thin trait over `flux_provider::Provider` so `flux-runtime`'s `ToolContext` can carry it without
/// depending on a concrete provider.
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// Complete a prompt, optionally constrained to a JSON schema, returning the raw model output.
    async fn complete(
        &self,
        prompt: &str,
        schema: Option<&serde_json::Value>,
    ) -> flux_core::Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_registry_lists_the_same_ops_as_the_tool_registry() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);

        let ops = OpRegistry::new(&reg);
        let mut from_ops = ops.op_names();
        let mut from_tools = reg.names();
        from_ops.sort();
        from_tools.sort();

        assert_eq!(from_ops, from_tools);
        assert!(!from_ops.is_empty(), "builtins should register some ops");
        assert_eq!(ops.signatures().len(), from_tools.len());
        assert!(ops.get("read").is_some());
    }

    #[test]
    fn with_advertised_filters_catalog_but_not_get() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);
        // Advertise only `read`: the catalog shrinks to it, but a hidden op still resolves via `get`.
        let allowed: HashSet<String> = ["read".to_string()].into_iter().collect();
        let ops = OpRegistry::new(&reg).with_advertised(allowed);
        assert_eq!(ops.op_names(), vec!["read".to_string()]);
        assert_eq!(ops.signatures().len(), 1);
        // `git_status` is not advertised…
        assert!(!ops.op_names().contains(&"git_status".to_string()));
        // …but still resolves for execution / pre-authored flows.
        assert!(ops.get("git_status").is_some());
    }

    #[test]
    fn signature_carries_param_names_as_a_set() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);
        let ops = OpRegistry::new(&reg);

        let read = ops.get("read").unwrap();
        assert_eq!(read.required_params, vec!["path".to_string()]);
        // Multi-param (required + optional) renders as a named-object signature `{path, offset, limit}`;
        // order is display-only (a set), not a positional binding.
        assert_eq!(read.param_signature(), "{path, limit, offset}");

        let edit = ops.get("edit").unwrap();
        // `required` is a set (membership, not order): schemars builds it from a map, so its
        // serialization order is non-deterministic. Check as a sorted set.
        let mut req_sorted = edit.required_params.clone();
        req_sorted.sort();
        assert_eq!(
            req_sorted,
            vec![
                "new_string".to_string(),
                "old_string".to_string(),
                "path".to_string()
            ]
        );
        assert_eq!(edit.optional_params, vec!["replace_all".to_string()]);
        // `param_signature` renders required params in declared (schema) order, then the sorted
        // optional set — a membership set, not an alphabetical one (see the req_sorted check above).
        assert_eq!(
            edit.param_signature(),
            "{path, old_string, new_string, replace_all}"
        );
    }
}
