//! The analyzer. M1 validates the single-`call` grammar: the operation must be registered. Later
//! milestones add full name / type / effect / bounded-loop checking over the whole AST, lowering a
//! [`DraftAst`](crate::ast::DraftAst) into a typed [`HirFlow`](crate::ast::HirFlow).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use flux_spec::{Idempotency, Risk};

use crate::ast::{
    is_valid_decl_name, is_valid_op_name, DraftAst, FlowEffect, HirFlow, Node, SymbolName, TypeRef,
};
use crate::opspec::{OpCatalog, OpSignature};

/// A single analyzer diagnostic, suitable for UI display or feeding back into the compile/repair
/// loop. The JSON-pointer-style node path (`body[3].then[1]`) is rendered into `message` — the
/// struct's shape is kept message-only so downstream crates (flux-flow/flux-sdk/flux-cli) keep
/// compiling unchanged (L-16/F11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Diagnostic accumulator that threads the current node path (`body[3].then[1]`) through the
/// structural walk and renders it into every message it emits — the locator a repairing model can
/// act on (L-16/F11). Internal only; the public surface stays `Vec<Diagnostic>`.
#[derive(Default)]
struct Diags {
    items: Vec<Diagnostic>,
    path: Vec<String>,
}

impl Diags {
    /// Record a diagnostic at the current node path.
    fn add(&mut self, message: impl Into<String>) {
        let message = message.into();
        let rendered = if self.path.is_empty() {
            message
        } else {
            format!("{message} (at `{}`)", self.path.join("."))
        };
        self.items.push(Diagnostic::new(rendered));
    }

    /// Run `f` with `seg` pushed onto the node path (popped afterwards, even on early return).
    fn with<R>(&mut self, seg: impl Into<String>, f: impl FnOnce(&mut Self) -> R) -> R {
        self.path.push(seg.into());
        let out = f(self);
        self.path.pop();
        out
    }
}

/// Sanity ceiling for `repeat` `max` (F10): a bound above this is virtually always a model
/// emitting an effectively-unbounded loop, not a real plan — reject it so the repair loop asks
/// for a plausible bound (or an `each` over real data).
const MAX_REPEAT_BOUND: u32 = 100_000;

/// The serde `kind` tag of a node, for diagnostics — read off the tagged serialization so it can
/// never drift from the wire format (and needs no 43-arm match to keep in sync).
fn node_kind_label(node: &Node) -> String {
    serde_json::to_value(node)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str().map(str::to_owned)))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Validate that `op` names a registered operation (the M1 single-call grammar). Returns the
/// collected diagnostics on failure.
pub fn analyze_call(op: &str, ops: &dyn OpCatalog) -> Result<(), Vec<Diagnostic>> {
    if ops.lookup(op).is_some() {
        Ok(())
    } else {
        Err(vec![Diagnostic::new(format!("unknown operation: `{op}`"))])
    }
}

/// Validate a whole flow against the catalog and the structural contract the runtime actually
/// enforces: op resolution and arity, symbol definedness (every `$var` names a flow param, a
/// symbol bound by *some* binder form anywhere in the flow, or a `session_symbols` entry —
/// order-insensitive on purpose, so there are zero false positives and the typo class is caught,
/// L-15/F5), declared-name validity (F8), expression-position legality mirroring the runtime's
/// `eval_arg`/`eval_cond` (F7), loop bounds (F10), and `parallel` bind disjointness (F15).
/// Diagnostics render a JSON-pointer-style node path (`body[3].then[1]`) into their message.
///
/// `session_symbols` is the set of symbols already bound in the executing session (SessionView
/// names, SDK-seeded params, …); composites and presets pass an empty set.
pub fn analyze_flow(
    ast: &DraftAst,
    ops: &dyn OpCatalog,
    session_symbols: &HashSet<String>,
) -> Result<(), Vec<Diagnostic>> {
    let mut d = Diags::default();
    if let Some(name) = &ast.name {
        if !is_valid_decl_name(name) {
            d.add(format!(
                "invalid flow name `{name}` — flow names contain only ASCII letters, digits, \
                 `_`, or `-`"
            ));
        }
    }
    for p in &ast.params {
        check_decl_name(&p.name, "a flow param", &mut d);
    }
    // The definedness set: params + session symbols + every binder form anywhere in the body.
    let mut bound: HashSet<String> = session_symbols.clone();
    bound.extend(ast.params.iter().map(|p| p.name.0.clone()));
    collect_bound_symbols(&ast.body, &mut bound);
    for (i, node) in ast.body.iter().enumerate() {
        d.with(format!("body[{i}]"), |d| check_node(node, ops, &bound, d));
    }
    check_await_position(&ast.body, &mut d);
    check_checkpoint_position(&ast.body, &mut d);
    check_cap_scope_position(&ast.body, &mut d);
    check_cancellation_cleanup_position(&ast.body, &mut d);
    check_cap_scopes(&ast.body, None, &mut d);
    if d.items.is_empty() {
        Ok(())
    } else {
        Err(d.items)
    }
}

/// Statically flag a literal-op `call` that is provably outside the enclosing `with_tools`
/// allowlist(s) — the analyzer-visible half of capability scoping (the runtime dispatch gate is the
/// enforcement authority; this is best-effort early feedback so a plan is rejected before it runs).
/// `active` is the narrowed allowlist in effect at this point (`None` = no scope open, everything is
/// analyzer-permitted here). A `CapScope`'s own `tools` is intersected with `active` on descent — the
/// same narrow-only rule the runtime's `push_cap_scope` enforces — so a nested scope is checked against
/// the *effective*, already-narrowed set, never the outer scope's original one. Non-literal call sites
/// don't exist in this grammar (`op` is always a literal `String`), so every `call` site is checkable.
fn check_cap_scopes(body: &[Node], active: Option<&[String]>, d: &mut Diags) {
    for node in body {
        match node {
            Node::Call { op, .. } => check_call_in_scope(op, active, d),
            Node::CapScope {
                tools, body: inner, ..
            } => {
                let narrowed: Vec<String> = match active {
                    Some(outer) => tools
                        .iter()
                        .filter(|t| outer.contains(t))
                        .cloned()
                        .collect(),
                    None => tools.clone(),
                };
                check_cap_scopes(inner, Some(&narrowed), d);
                continue;
            }
            _ => {}
        }
        // Recurse into every OTHER dispatch-capable child position under the same active scope — a
        // `when`/`each`/`repeat`/`try`/… inside a `with_tools` block is still inside it, and so are
        // a `bind`'s call value, a `when`'s call condition, a `pipe`'s steps, a `route` selector, …
        // `CapScope` already recursed above with its own narrowed set and `continue`d, so it never
        // reaches here.
        for nested in nested_bodies(node) {
            check_cap_scopes(nested, active, d);
        }
    }
}

/// Flag `op` if a capability scope is active and `op` is not in its allowlist.
fn check_call_in_scope(op: &str, active: Option<&[String]>, d: &mut Diags) {
    let Some(allowed) = active else { return };
    if !allowed.iter().any(|t| t == op) {
        d.add(format!(
            "op `{op}` is outside the enclosing `with_tools` scope ({})",
            if allowed.is_empty() {
                "which allows no tools".to_string()
            } else {
                format!("which allows only [{}]", allowed.join(", "))
            }
        ));
    }
}

/// The direct dispatch-capable child positions of `node` (one level, not recursive) — every place
/// a `call`/`with_tools` site can occur: statement bodies, plus the single-node positions the
/// runtime executes via dispatch-capable evaluators (`bind`/`memo`/`return` values, `when`/
/// `unless`/`assert` conditions and `repeat`/`loop` `until` guards — `eval_cond` dispatches a
/// `call` condition — `pipe` steps, a `route` selector, `verify` cmd/expect, a `scope` acquire).
/// Used by [`check_cap_scopes`] to recurse while threading an explicit (possibly narrowed) active
/// allowlist, which the generic [`for_each_node`] callback can't carry.
///
/// Exhaustive on purpose (no `_ =>`, F12): a new node kind must state its child positions here.
fn nested_bodies(node: &Node) -> Vec<&[Node]> {
    use std::slice::from_ref;
    match node {
        Node::When {
            cond,
            then,
            otherwise,
        } => vec![from_ref(cond.as_ref()), then, otherwise],
        Node::Unless { cond, body } => vec![from_ref(cond.as_ref()), body],
        Node::Repeat { until, body, .. } | Node::Loop { until, body, .. } => {
            let mut v: Vec<&[Node]> = Vec::with_capacity(2);
            if let Some(u) = until {
                v.push(from_ref(u.as_ref()));
            }
            v.push(body);
            v
        }
        // An `each` source resolves through `eval_arg` (no dispatch), so only the body counts.
        Node::Each { body, .. } => vec![body],
        Node::Seq { body, .. }
        | Node::Retry { body, .. }
        | Node::Confirm { body, .. }
        | Node::Throttle { body, .. }
        | Node::Debounce { body, .. }
        | Node::Timeout { body, .. }
        | Node::Budget { body, .. }
        | Node::Once { body, .. } => vec![body],
        Node::Try { body, handler, .. } => vec![body, handler],
        Node::Parallel { branches } | Node::Race { branches, .. } => {
            branches.iter().map(|b| b.body.as_slice()).collect()
        }
        Node::Scope {
            acquire,
            body,
            finally,
            ..
        } => {
            let mut v: Vec<&[Node]> = Vec::with_capacity(3);
            if let Some(acq) = acquire {
                v.push(from_ref(acq.as_ref()));
            }
            v.push(body);
            v.push(finally);
            v
        }
        Node::Saga { steps } => steps
            .iter()
            .flat_map(|s| [s.body.as_slice(), s.undo.as_slice()])
            .collect(),
        // A `match` subject / case value must be a literal or bound symbol (enforced by
        // `check_node`), so only the bodies can dispatch.
        Node::Match { cases, default, .. } => cases
            .iter()
            .map(|c| c.body.as_slice())
            .chain(std::iter::once(default.as_slice()))
            .collect(),
        Node::Route {
            selector,
            cases,
            default,
        } => std::iter::once(from_ref(selector.as_ref()))
            .chain(cases.iter().map(|c| c.body.as_slice()))
            .chain(std::iter::once(default.as_slice()))
            .collect(),
        Node::Fallback { branches, .. } => branches.iter().map(|b| b.body.as_slice()).collect(),
        Node::Bind { value, .. } | Node::Memo { value, .. } | Node::Return { value } => {
            vec![from_ref(value.as_ref())]
        }
        Node::Assert { cond, .. } => vec![from_ref(cond.as_ref())],
        Node::Pipe { steps, .. } => vec![steps.as_slice()],
        Node::Verify { cmd, expect, .. } => {
            vec![from_ref(cmd.as_ref()), from_ref(expect.as_ref())]
        }
        // `with_tools` is descended by `check_cap_scopes` itself — it must narrow the active
        // allowlist on the way down, which this label-free helper cannot express.
        Node::CapScope { .. } => Vec::new(),
        // Leaf / pure-expression nodes: no dispatch-capable child positions. Call args, template
        // leaves, `expr` vars, and `jq`/`parse` inputs are argument positions — the analyzer
        // rejects call/control nodes there, so there is nothing for the cap-scope pass to see.
        Node::Call { .. }
        | Node::Await { .. }
        | Node::Peek { .. }
        | Node::Var { .. }
        | Node::Lit { .. }
        | Node::Thing { .. }
        | Node::Expr { .. }
        | Node::Fmt { .. }
        | Node::Jq { .. }
        | Node::Parse { .. }
        | Node::Ctx { .. }
        | Node::CtxAppend { .. }
        | Node::Checkpoint { .. }
        | Node::Obj { .. }
        | Node::List { .. } => Vec::new(),
    }
}

/// `await` may only appear as a **top-level** flow statement: it suspends the whole flow for cross-turn
/// resume (the interpreter records the top-level index and continues from the next statement on resume).
/// Nesting one inside a `when`/`repeat`/`each`/`parallel`/… body has no well-defined resume point in v1,
/// so it is rejected here (a clear analysis error rather than a runtime failure deep in `exec_body`).
fn check_await_position(body: &[Node], d: &mut Diags) {
    for (i, node) in body.iter().enumerate() {
        // A top-level `await` is fine; flag any `await` hiding inside a non-`await` statement's subtree.
        if matches!(node, Node::Await { .. }) {
            continue;
        }
        let mut nested = false;
        for_each_node(std::slice::from_ref(node), &mut |n| {
            if matches!(n, Node::Await { .. }) {
                nested = true;
            }
        });
        if nested {
            d.with(format!("body[{i}]"), |d| {
                d.add(
                    "`await` must be a top-level flow statement — it suspends the whole flow and cannot be nested (v1)",
                )
            });
        }
    }
}

/// `checkpoint` may only appear as a **top-level** flow statement: it is a durable resume cursor keyed
/// on a top-level index, so a `checkpoint` nested inside a `when`/`repeat`/`scope`/… body has no stable
/// resume point. Rejected here (mirrors [`check_await_position`]).
fn check_checkpoint_position(body: &[Node], d: &mut Diags) {
    for (i, node) in body.iter().enumerate() {
        if matches!(node, Node::Checkpoint { .. }) {
            continue;
        }
        let mut nested = false;
        for_each_node(std::slice::from_ref(node), &mut |n| {
            if matches!(n, Node::Checkpoint { .. }) {
                nested = true;
            }
        });
        if nested {
            d.with(format!("body[{i}]"), |d| {
                d.add(
                    "`checkpoint` must be a top-level flow statement — it is a durable resume cursor and cannot be nested (v1)",
                )
            });
        }
    }
}

/// `parallel`/`race` branches run concurrently against the interpreter's ONE shared executor
/// (`futures::future::join_all`), so `with_tools`/`CapScope`'s cap-scope stack is shared, mutable
/// state across every branch. A scope opened in one branch can be intersected away or popped by a
/// sibling branch mid-await: either the effective allowlist is emptied (a spurious `Denied`, fails
/// safe but nondeterministic) or a sibling finishing first pops the wrong guard (LIFO across
/// branches), leaving a wider allowlist active than the branch itself declared (an authorization
/// escape, capped at the outer scope). `with_tools` composes soundly only when the cap-scope stack
/// is used single-threaded, so a `CapScope` nested inside a `parallel`/`race` branch is rejected
/// here (mirrors [`check_await_position`]/[`check_checkpoint_position`]); a sequential `with_tools`
/// outside any concurrent branch is unaffected.
fn check_cap_scope_position(body: &[Node], d: &mut Diags) {
    for (i, node) in body.iter().enumerate() {
        let mut nested = false;
        for_each_node(std::slice::from_ref(node), &mut |n| {
            let branches = match n {
                Node::Parallel { branches } => branches,
                Node::Race { branches, .. } => branches,
                _ => return,
            };
            if branches.iter().any(|b| branch_contains_cap_scope(&b.body)) {
                nested = true;
            }
        });
        if nested {
            d.with(format!("body[{i}]"), |d| {
                d.add(
                    "`with_tools` cannot be nested inside a `parallel`/`race` branch — its capability scope is shared, mutable state across concurrently running branches and does not compose safely (v1)",
                )
            });
        }
    }
}

/// True if `body`'s subtree contains a `CapScope` anywhere — used by [`check_cap_scope_position`]
/// to scan a `parallel`/`race` branch for a nested `with_tools`.
fn branch_contains_cap_scope(body: &[Node]) -> bool {
    let mut found = false;
    for_each_node(body, &mut |n| {
        if matches!(n, Node::CapScope { .. }) {
            found = true;
        }
    });
    found
}

/// `timeout` and `race` cancel work by dropping the unfinished body/loser future. An async
/// `scope.finally` or `with_tools` pop cannot run from `Drop`, so placing either cleanup-bearing
/// node inside a cancellation boundary would violate its unconditional-unwind contract. Reject
/// that shape in v1; wrapping the timeout/race *inside* a cleanup scope remains safe because the
/// outer scope observes the cancellation error and unwinds normally.
fn check_cancellation_cleanup_position(body: &[Node], d: &mut Diags) {
    for (i, node) in body.iter().enumerate() {
        let mut timeout_cleanup = false;
        let mut race_cleanup = false;
        for_each_node(std::slice::from_ref(node), &mut |nested| match nested {
            Node::Timeout { body, .. } => {
                timeout_cleanup |= branch_contains_cleanup_scope(body);
            }
            Node::Race { branches, .. } => {
                race_cleanup |= branches
                    .iter()
                    .any(|branch| branch_contains_cleanup_scope(&branch.body));
            }
            _ => {}
        });
        if timeout_cleanup {
            d.with(format!("body[{i}]"), |d| {
                d.add(
                    "a cleanup scope (`scope`/`with_tools`) cannot be nested inside `timeout` — \
                     timeout cancels by dropping unfinished work, so async cleanup could not be \
                     guaranteed (wrap the timeout in the cleanup scope instead)",
                )
            });
        }
        if race_cleanup {
            d.with(format!("body[{i}]"), |d| {
                d.add(
                    "a cleanup scope (`scope`/`with_tools`) cannot be nested inside a `race` \
                     branch — losing branches are cancelled by dropping their futures, so async \
                     cleanup could not be guaranteed (wrap the race in the cleanup scope instead)",
                )
            });
        }
    }
}

fn branch_contains_cleanup_scope(body: &[Node]) -> bool {
    let mut found = false;
    for_each_node(body, &mut |node| {
        let has_cleanup = match node {
            Node::CapScope { .. } => true,
            Node::Scope { finally, .. } => !finally.is_empty(),
            _ => false,
        };
        if has_cleanup {
            found = true;
        }
    });
    found
}

/// Lower a `DraftAst` to a typed [`HirFlow`]: run the whole-flow analysis (op resolution, grammar,
/// bounded loops, call arity, symbol definedness against `session_symbols`) and gather the flow's
/// semantic effect set. Full type inference over expressions is a later milestone; today the HIR
/// carries the validated body plus the gathered effects an authorizer/optimizer reasons over.
pub fn lower(
    ast: &DraftAst,
    ops: &dyn OpCatalog,
    session_symbols: &HashSet<String>,
) -> Result<HirFlow, Vec<Diagnostic>> {
    analyze_flow(ast, ops, session_symbols)?;
    // Type-check call arguments against the ops' declared param types, tracking symbol types from
    // `param` decls + `bind` annotations. Lenient: only hard scalar/list mismatches are rejected.
    let mut scope: HashMap<String, TypeRef> = ast
        .params
        .iter()
        .map(|p| (p.name.0.clone(), p.ty.clone()))
        .collect();
    let mut d = Diags::default();
    type_check_body(&ast.body, "body", ops, &mut scope, &mut d);
    if !d.items.is_empty() {
        return Err(d.items);
    }
    Ok(HirFlow {
        name: ast.name.clone(),
        params: ast.params.clone(),
        returns: ast.returns.clone(),
        body: ast.body.clone(),
        effects: gather_effects(&ast.body, ops),
    })
}

/// Infer an expression's type for argument checking. Literals, `var`s (via `scope`), and `fmt` (always
/// a string) infer precisely; everything else is `Any` (lenient — no false positives on op outputs).
fn infer_type(node: &Node, scope: &HashMap<String, TypeRef>) -> TypeRef {
    match node {
        Node::Lit { value } => lit_type(value),
        Node::Var { name } => scope.get(&name.0).cloned().unwrap_or(TypeRef::Any),
        Node::Fmt { .. } => TypeRef::String,
        _ => TypeRef::Any,
    }
}

/// The concrete [`TypeRef`] of a JSON literal, for checking a named-arg object's field values.
fn lit_type(value: &serde_json::Value) -> TypeRef {
    match value {
        serde_json::Value::String(_) => TypeRef::String,
        serde_json::Value::Number(_) => TypeRef::Number,
        serde_json::Value::Bool(_) => TypeRef::Bool,
        serde_json::Value::Array(_) => TypeRef::List(Box::new(TypeRef::Any)),
        _ => TypeRef::Any,
    }
}

/// The concrete scalar/list "kind" of a type, or `None` for `Any`/`Named` — which never conflict, so
/// forward-compat named types and unknown-typed args always pass.
fn concrete_kind(t: &TypeRef) -> Option<u8> {
    match t {
        TypeRef::String => Some(0),
        TypeRef::Number => Some(1),
        TypeRef::Bool => Some(2),
        TypeRef::List(_) => Some(3),
        TypeRef::Any | TypeRef::Named(_) => None,
    }
}

/// Two types conflict only when both are concrete and a different kind (string vs number, list vs
/// scalar, …). `Any`/`Named` on either side is lenient.
fn types_conflict(arg: &TypeRef, param: &TypeRef) -> bool {
    matches!((concrete_kind(arg), concrete_kind(param)), (Some(a), Some(p)) if a != p)
}

/// Type-check a call's arguments under **named-args** semantics. A call names its inputs:
/// - a lone object literal is the whole named input (checked field-by-field against `param_types`);
/// - a single bare value binds to the op's **sole** parameter (error if the op declares more than
///   one — ambiguous without a name);
/// - two or more bare values is the deprecated positional form — rejected with a diagnostic that
///   routes the model through the repair loop to emit an object instead.
fn check_call_types(
    op: &str,
    args: &[Node],
    ops: &dyn OpCatalog,
    scope: &HashMap<String, TypeRef>,
    d: &mut Diags,
) {
    let Some(sig) = ops.lookup(op) else {
        return;
    };
    // A lone object literal is the named input map. Every `required_params` key must be present —
    // keys are static even when values are dynamic (L-15/F6) — and each present, declared field's
    // type is checked; extra fields are not errors (the runtime/op decides) but we flag hard
    // scalar mismatches.
    if let [Node::Lit { value }] = args {
        if let Some(obj) = value.as_object() {
            for req in sig.required_params.iter().filter(|r| !obj.contains_key(*r)) {
                d.add(missing_param_diag(op, req, &sig));
            }
            if let Some(props_types) = Some(&sig.param_types).filter(|m| !m.is_empty()) {
                for (name, val) in obj {
                    if let Some(ptype) = props_types.get(name) {
                        let atype = lit_type(val);
                        if types_conflict(&atype, ptype) {
                            d.add(format!(
                                "op `{op}` parameter `{name}` expects {}, got {}",
                                ptype.label(),
                                atype.label()
                            ));
                        }
                    }
                }
            }
            return;
        }
    }
    // A lone `obj` **template** is the named input map exactly like a lone `lit` object (the
    // runtime resolves its fields first) — its KEYS are static, so required-key presence is
    // checkable even though its values are dynamic.
    if let [Node::Obj { fields }] = args {
        for req in sig
            .required_params
            .iter()
            .filter(|r| !fields.contains_key(*r))
        {
            d.add(missing_param_diag(op, req, &sig));
        }
        return;
    }
    let n_params = sig.required_params.len() + sig.optional_params.len();
    // Arity (multi-bare rejection, single-bare-vs-multi) is handled in `check_node` (the structural
    // pass). Here we only type-check the values, and only for *typed* catalogs (n_params > 0);
    // an untyped schema (n_params == 0) gives no param types to check against, so stay lenient.
    if n_params > 0 {
        match args.len() {
            0 => {} // missing-args handled in `check_node`.
            1 => {
                // Single bare value binds to the sole required param (the ergonomic sugar:
                // `read("x")`, `grep("TODO")`) when there's exactly one required, else the sole param.
                let pname = if sig.required_params.len() == 1 {
                    sig.required_params.first().cloned()
                } else {
                    sig.required_params
                        .first()
                        .or(sig.optional_params.first())
                        .cloned()
                };
                if let Some(pname) = pname {
                    if let Some(ptype) = sig.param_types.get(&pname) {
                        let atype = infer_type(&args[0], scope);
                        if types_conflict(&atype, ptype) {
                            d.add(format!(
                                "op `{op}` parameter `{pname}` expects {}, got {}",
                                ptype.label(),
                                atype.label()
                            ));
                        }
                    }
                }
            }
            _ => {} // multi-bare: rejected upstream; nothing to type-check here.
        }
    }
    for (i, a) in args.iter().enumerate() {
        if let Node::Call {
            op: inner,
            args: iargs,
        } = a
        {
            d.with(format!("args[{i}]"), |d| {
                check_call_types(inner, iargs, ops, scope, d)
            });
        }
    }
}

/// Build the "missing required parameter" repair diagnostic. It names the missing param's expected
/// type and the op's full accepted-parameter shape (both already carried by the `OpSignature`), so a
/// repairing model gets the type and the exact key set — not just "add a key" — and can add the right
/// field on the next attempt instead of re-emitting the same broken call (F-002). Degrades cleanly on
/// an untyped catalog: the `(expected …)` clause and per-param types are simply omitted.
fn missing_param_diag(op: &str, req: &str, sig: &OpSignature) -> String {
    let expected = sig
        .param_types
        .get(req)
        .map(|t| format!(" (expected {})", t.label()))
        .unwrap_or_default();
    format!(
        "op `{op}` is missing required parameter `{req}`{expected} — add `{req}` to the argument \
         object. `{op}` accepts: {}",
        describe_params(sig)
    )
}

/// Render an op's accepted parameters for a repair diagnostic, e.g.
/// `ask (String, required), ctx (Ctx, optional)`. A param whose type is unknown (untyped catalog) is
/// shown as `name (required)`.
fn describe_params(sig: &OpSignature) -> String {
    let one = |name: &str, kind: &str| match sig.param_types.get(name) {
        Some(t) => format!("{name} ({}, {kind})", t.label()),
        None => format!("{name} ({kind})"),
    };
    let mut parts: Vec<String> = sig
        .required_params
        .iter()
        .map(|p| one(p, "required"))
        .collect();
    parts.extend(sig.optional_params.iter().map(|p| one(p, "optional")));
    if parts.is_empty() {
        "no parameters".to_string()
    } else {
        parts.join(", ")
    }
}

/// Ordered type-check walk: track each symbol's type (a `bind`/`memo`'s `ty` annotation, else `Any`)
/// and check every `call`'s args. Control bodies are checked with a cloned scope (a branch-local bind
/// doesn't leak out — conservative). Threads the same `label[i]` node path [`analyze_flow`]'s
/// structural walk renders, so a type diagnostic carries the locator a repairing model can act on
/// (L-21; previously these diagnostics were path-less while the structural ones weren't).
fn type_check_body(
    body: &[Node],
    label: &str,
    ops: &dyn OpCatalog,
    scope: &mut HashMap<String, TypeRef>,
    d: &mut Diags,
) {
    for (i, node) in body.iter().enumerate() {
        d.with(format!("{label}[{i}]"), |d| match node {
            Node::Bind {
                name, value, ty, ..
            }
            | Node::Memo {
                name, value, ty, ..
            } => {
                if let Node::Call { op, args } = value.as_ref() {
                    d.with("value", |d| check_call_types(op, args, ops, scope, d));
                }
                scope.insert(name.0.clone(), ty.clone().unwrap_or(TypeRef::Any));
            }
            Node::Call { op, args } => check_call_types(op, args, ops, scope, d),
            Node::Return { value } => {
                if let Node::Call { op, args } = value.as_ref() {
                    d.with("value", |d| check_call_types(op, args, ops, scope, d));
                }
            }
            Node::Pipe { steps, .. } => {
                for (j, s) in steps.iter().enumerate() {
                    if let Node::Call { op, args } = s {
                        d.with(format!("steps[{j}]"), |d| {
                            check_call_types(op, args, ops, scope, d)
                        });
                    }
                }
            }
            Node::When {
                then, otherwise, ..
            } => {
                type_check_body(then, "then", ops, &mut scope.clone(), d);
                type_check_body(otherwise, "otherwise", ops, &mut scope.clone(), d);
            }
            Node::Unless { body, .. } => type_check_body(body, "body", ops, &mut scope.clone(), d),
            Node::Each { item, body, .. } => {
                let mut s = scope.clone();
                s.insert(item.0.clone(), TypeRef::Any);
                type_check_body(body, "body", ops, &mut s, d);
            }
            Node::Repeat { body, .. }
            | Node::Seq { body, .. }
            | Node::Retry { body, .. }
            | Node::Confirm { body, .. }
            | Node::Loop { body, .. }
            | Node::Throttle { body, .. }
            | Node::Debounce { body, .. } => {
                type_check_body(body, "body", ops, &mut scope.clone(), d)
            }
            Node::Try { body, handler, .. } => {
                type_check_body(body, "body", ops, &mut scope.clone(), d);
                type_check_body(handler, "handler", ops, &mut scope.clone(), d);
            }
            Node::Parallel { branches } | Node::Race { branches, .. } => {
                for (j, b) in branches.iter().enumerate() {
                    d.with(format!("branches[{j}]"), |d| {
                        type_check_body(&b.body, "body", ops, &mut scope.clone(), d)
                    });
                }
            }
            Node::Timeout { body, .. } | Node::Budget { body, .. } => {
                type_check_body(body, "body", ops, &mut scope.clone(), d)
            }
            Node::Scope { body, finally, .. } => {
                type_check_body(body, "body", ops, &mut scope.clone(), d);
                type_check_body(finally, "finally", ops, &mut scope.clone(), d);
            }
            Node::Saga { steps } => {
                for (j, step) in steps.iter().enumerate() {
                    d.with(format!("steps[{j}]"), |d| {
                        type_check_body(&step.body, "body", ops, &mut scope.clone(), d);
                        type_check_body(&step.undo, "undo", ops, &mut scope.clone(), d);
                    });
                }
            }
            Node::Once { body, .. } => type_check_body(body, "body", ops, &mut scope.clone(), d),
            Node::Fallback { branches, .. } => {
                for (j, b) in branches.iter().enumerate() {
                    d.with(format!("branches[{j}]"), |d| {
                        type_check_body(&b.body, "body", ops, &mut scope.clone(), d)
                    });
                }
            }
            Node::Match { cases, default, .. } => {
                // The subject is a literal/bound symbol (enforced by `check_node`), so there's no call
                // to type-check here — only the case + default bodies.
                for (j, c) in cases.iter().enumerate() {
                    d.with(format!("cases[{j}]"), |d| {
                        type_check_body(&c.body, "body", ops, &mut scope.clone(), d)
                    });
                }
                type_check_body(default, "default", ops, &mut scope.clone(), d);
            }
            Node::Route {
                selector,
                cases,
                default,
            } => {
                if let Node::Call { op, args } = selector.as_ref() {
                    d.with("selector", |d| check_call_types(op, args, ops, scope, d));
                }
                for (j, c) in cases.iter().enumerate() {
                    d.with(format!("cases[{j}]"), |d| {
                        type_check_body(&c.body, "body", ops, &mut scope.clone(), d)
                    });
                }
                type_check_body(default, "default", ops, &mut scope.clone(), d);
            }
            _ => {}
        });
    }
}

/// The semantic effects a flow declares or implies: each `bind`/`memo`'s declared `effect`, plus the
/// effects implied by the host ops it `call`s (mapped from their host-resource [`Effect`]s). Deduped,
/// in first-seen order.
fn gather_effects(body: &[Node], ops: &dyn OpCatalog) -> Vec<FlowEffect> {
    let mut acc: Vec<FlowEffect> = Vec::new();
    let push = |e: FlowEffect, acc: &mut Vec<FlowEffect>| {
        if !acc.contains(&e) {
            acc.push(e);
        }
    };
    for_each_node(body, &mut |node| match node {
        Node::Bind {
            effect: Some(e), ..
        }
        | Node::Memo {
            effect: Some(e), ..
        } => push(*e, &mut acc),
        Node::Call { op, .. } => {
            if let Some(sig) = ops.lookup(op) {
                for e in sig.effects {
                    if let Some(f) = host_effect_to_flow(e) {
                        push(f, &mut acc);
                    }
                }
            }
        }
        _ => {}
    });
    acc
}

/// Map a host-resource [`Effect`] back to a representative semantic [`FlowEffect`] for HIR effect
/// gathering. Host effects with no clean semantic counterpart (process/browser/local) are skipped.
fn host_effect_to_flow(e: flux_spec::Effect) -> Option<FlowEffect> {
    use flux_spec::Effect;
    match e {
        Effect::Read => Some(FlowEffect::Read),
        Effect::Write | Effect::Filesystem => Some(FlowEffect::WriteFile),
        Effect::Network => Some(FlowEffect::Network),
        Effect::Process | Effect::Browser | Effect::LocalSystem => None,
    }
}

/// Visit every node in `body` and all its nested bodies (depth-first, pre-order), invoking `f` on
/// each. A single generic traversal reused for effect gathering and future HIR passes.
pub fn for_each_node(body: &[Node], f: &mut impl FnMut(&Node)) {
    for node in body {
        f(node);
        match node {
            Node::Bind { value, .. } | Node::Memo { value, .. } => {
                for_each_node(std::slice::from_ref(value), f)
            }
            Node::When {
                cond,
                then,
                otherwise,
            } => {
                for_each_node(std::slice::from_ref(cond), f);
                for_each_node(then, f);
                for_each_node(otherwise, f);
            }
            Node::Unless { cond, body } => {
                for_each_node(std::slice::from_ref(cond), f);
                for_each_node(body, f);
            }
            Node::Repeat { until, body, .. } | Node::Loop { until, body, .. } => {
                if let Some(u) = until {
                    for_each_node(std::slice::from_ref(u), f);
                }
                for_each_node(body, f);
            }
            Node::Each { source, body, .. } => {
                for_each_node(std::slice::from_ref(source), f);
                for_each_node(body, f);
            }
            Node::Assert { cond, .. } => for_each_node(std::slice::from_ref(cond), f),
            Node::Pipe { steps, .. } => for_each_node(steps, f),
            Node::Seq { body, .. }
            | Node::Retry { body, .. }
            | Node::Confirm { body, .. }
            | Node::Throttle { body, .. }
            | Node::Debounce { body, .. } => for_each_node(body, f),
            Node::Try { body, handler, .. } => {
                for_each_node(body, f);
                for_each_node(handler, f);
            }
            Node::Parallel { branches } => {
                for b in branches {
                    for_each_node(&b.body, f);
                }
            }
            Node::Race { branches, .. } => {
                for b in branches {
                    for_each_node(&b.body, f);
                }
            }
            Node::Verify { cmd, expect, .. } => {
                for_each_node(std::slice::from_ref(cmd), f);
                for_each_node(std::slice::from_ref(expect), f);
            }
            Node::Return { value } => for_each_node(std::slice::from_ref(value), f),
            Node::Call { args, .. } => for_each_node(args, f),
            Node::Jq { input, .. } => for_each_node(std::slice::from_ref(input), f),
            Node::Parse { value, .. } => for_each_node(std::slice::from_ref(value), f),
            Node::Expr { vars, .. } => {
                for v in vars.values() {
                    for_each_node(std::slice::from_ref(v), f);
                }
            }
            Node::Match {
                subject,
                cases,
                default,
            } => {
                for_each_node(std::slice::from_ref(subject), f);
                for c in cases {
                    for_each_node(std::slice::from_ref(&c.value), f);
                    for_each_node(&c.body, f);
                }
                for_each_node(default, f);
            }
            Node::Route {
                selector,
                cases,
                default,
            } => {
                for_each_node(std::slice::from_ref(selector), f);
                for c in cases {
                    for_each_node(&c.body, f);
                }
                for_each_node(default, f);
            }
            Node::Fallback { branches, .. } => {
                for b in branches {
                    for_each_node(&b.body, f);
                }
            }
            Node::Timeout { body, .. }
            | Node::Budget { body, .. }
            | Node::CapScope { body, .. } => for_each_node(body, f),
            Node::Scope {
                acquire,
                body,
                finally,
                ..
            } => {
                if let Some(acq) = acquire {
                    for_each_node(std::slice::from_ref(acq.as_ref()), f);
                }
                for_each_node(body, f);
                for_each_node(finally, f);
            }
            Node::Saga { steps } => {
                for step in steps {
                    for_each_node(&step.body, f);
                    for_each_node(&step.undo, f);
                }
            }
            Node::Once { body, .. } => for_each_node(body, f),
            // Value templates: descend into the sub-expressions so symbol reads inside a record/list
            // are seen by liveness (else the optimizer could dead-step a symbol used only in a template).
            Node::Obj { fields } => {
                for v in fields.values() {
                    for_each_node(std::slice::from_ref(v), f);
                }
            }
            Node::List { items } => for_each_node(items, f),
            // Leaf nodes (no nested node bodies).
            Node::Await { .. }
            | Node::Checkpoint { .. }
            | Node::Peek { .. }
            | Node::Var { .. }
            | Node::Lit { .. }
            | Node::Thing { .. }
            | Node::Fmt { .. }
            | Node::Ctx { .. }
            | Node::CtxAppend { .. } => {}
        }
    }
}

/// The effect/risk/idempotency annotation [`annotate_effects`] attributes to one `call` node.
///
/// `effects` mirrors [`gather_effects`]'s two contribution sources — the op's own host effects
/// (mapped onto [`FlowEffect`] via [`host_effect_to_flow`]) plus the semantic effect tag declared
/// on an immediately enclosing `bind`/`memo` (e.g. `$charge = call(charge_card, {…}) effect:
/// money`) — but attributed to this one call instead of deduped into the flow-wide union, so a
/// consumer can tell *which* call moves money rather than only that *something* in the flow does.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectAnnotation {
    /// Semantic effects attributed to this call (deduped, first-seen order — same convention as
    /// [`HirFlow::effects`]).
    pub effects: Vec<FlowEffect>,
    /// The op's declared risk tier, driving approval thresholds.
    pub risk: Risk,
    /// Whether re-running this op is safe.
    pub idempotency: Idempotency,
}

/// Derive one call's [`EffectAnnotation`] from the catalog, or `None` when `op` is unregistered —
/// the same "unknown operation" condition [`check_node`]'s `Node::Call` arm diagnoses. Callers must
/// keep the `None` entry (see [`annotate_effects`]) rather than treat it as "no effects": an unknown
/// op's effects are *unknown*, not empty.
///
/// Three contribution sources fold into `effects`, in order: the op's lowered host effects (mapped
/// back onto a representative [`FlowEffect`] via [`host_effect_to_flow`]), the op's own CATALOG-
/// declared semantics (`sig.semantic_effects` — D-138: `Money`/`Delete`/`SendExternal` a plugin
/// manifest or `OpSpec` declares directly, with no authored tag on the call site), and finally the
/// semantic effect tag declared on an immediately enclosing `bind`/`memo` (D-133's authored
/// `effect:`). All three are additive and deduped — an op that both declares `Money` in its own
/// catalog entry AND is called under an authored `effect: money` bind still reports `Money` once.
fn call_effect_annotation(
    op: &str,
    ops: &dyn OpCatalog,
    enclosing_effect: Option<FlowEffect>,
) -> Option<EffectAnnotation> {
    let sig = ops.lookup(op)?;
    let mut effects: Vec<FlowEffect> = Vec::new();
    for e in sig.effects {
        if let Some(f) = host_effect_to_flow(e) {
            if !effects.contains(&f) {
                effects.push(f);
            }
        }
    }
    for f in sig.semantic_effects {
        if !effects.contains(&f) {
            effects.push(f);
        }
    }
    if let Some(e) = enclosing_effect {
        if !effects.contains(&e) {
            effects.push(e);
        }
    }
    Some(EffectAnnotation {
        effects,
        risk: sig.risk,
        idempotency: sig.idempotency,
    })
}

/// Walk an analyzed flow and return, per `call` node, its [`EffectAnnotation`] keyed by the same
/// JSON-pointer-style node path diagnostics render (`body[3].then[1]`, see [`Diags`]) — the
/// per-node (attributed) sibling of [`gather_effects`]'s deduped flow-level union
/// ([`HirFlow::effects`]): right for the approval envelope, lossy for "which node did this."
///
/// An unregistered op annotates honestly as `None` rather than being silently skipped — the entry
/// still appears at its node path, matching [`analyze_call`]'s "unknown operation" diagnostic — so
/// a consumer (e.g. a visual editor pinning `Money`/`High`-risk nodes) can render "unknown effect"
/// instead of mistaking absence-from-the-list for "no effect."
///
/// Mirrors [`for_each_node`]'s traversal shape and [`check_node`]/[`check_body`]'s path labels in
/// lock-step — the two conventions must never drift apart, or a node path this function emits
/// would not line up with the diagnostic path a repairing model or UI already understands.
pub fn annotate_effects(
    ast: &DraftAst,
    ops: &dyn OpCatalog,
) -> Vec<(String, Option<EffectAnnotation>)> {
    let mut out = Vec::new();
    let mut d = Diags::default();
    annotate_body(&ast.body, "body", ops, &mut d, &mut out);
    out
}

/// [`annotate_effects`]'s statement-list walker — the per-node-path counterpart of [`check_body`].
fn annotate_body(
    body: &[Node],
    label: &str,
    ops: &dyn OpCatalog,
    d: &mut Diags,
    out: &mut Vec<(String, Option<EffectAnnotation>)>,
) {
    for (i, n) in body.iter().enumerate() {
        d.with(format!("{label}[{i}]"), |d| {
            annotate_node(n, ops, None, d, out)
        });
    }
}

/// [`annotate_effects`]'s single-node walker — the per-node-path counterpart of [`check_node`].
/// `enclosing_effect` is the effect tag (if any) declared on the `bind`/`memo` this node is the
/// direct `value` of; it folds into a directly-nested `call`'s annotation (see
/// [`call_effect_annotation`]) and is dropped for every other child position, matching
/// `gather_effects`'s bind-level contribution being "this statement's own tag," not inherited by
/// arbitrarily deep descendants.
///
/// Exhaustive on purpose (no `_ =>`, F12, mirrors [`for_each_node`]/[`check_node`]): a new node
/// kind must state its child positions here so this stays in lock-step with the diagnostic path
/// convention.
fn annotate_node(
    node: &Node,
    ops: &dyn OpCatalog,
    enclosing_effect: Option<FlowEffect>,
    d: &mut Diags,
    out: &mut Vec<(String, Option<EffectAnnotation>)>,
) {
    match node {
        Node::Call { op, args } => {
            out.push((
                d.path.join("."),
                call_effect_annotation(op, ops, enclosing_effect),
            ));
            for (i, a) in args.iter().enumerate() {
                d.with(format!("args[{i}]"), |d| {
                    annotate_node(a, ops, None, d, out)
                });
            }
        }
        Node::Bind { value, effect, .. } | Node::Memo { value, effect, .. } => {
            d.with("value", |d| annotate_node(value, ops, *effect, d, out));
        }
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            d.with("cond", |d| annotate_node(cond, ops, None, d, out));
            annotate_body(then, "then", ops, d, out);
            annotate_body(otherwise, "otherwise", ops, d, out);
        }
        Node::Unless { cond, body } => {
            d.with("cond", |d| annotate_node(cond, ops, None, d, out));
            annotate_body(body, "body", ops, d, out);
        }
        Node::Repeat { until, body, .. } | Node::Loop { until, body, .. } => {
            if let Some(u) = until {
                d.with("until", |d| annotate_node(u, ops, None, d, out));
            }
            annotate_body(body, "body", ops, d, out);
        }
        Node::Each { source, body, .. } => {
            d.with("in", |d| annotate_node(source, ops, None, d, out));
            annotate_body(body, "body", ops, d, out);
        }
        Node::Assert { cond, .. } => {
            d.with("cond", |d| annotate_node(cond, ops, None, d, out));
        }
        Node::Pipe { steps, .. } => {
            for (i, s) in steps.iter().enumerate() {
                d.with(format!("steps[{i}]"), |d| {
                    annotate_node(s, ops, None, d, out)
                });
            }
        }
        Node::Seq { body, .. }
        | Node::Retry { body, .. }
        | Node::Confirm { body, .. }
        | Node::Throttle { body, .. }
        | Node::Debounce { body, .. } => annotate_body(body, "body", ops, d, out),
        Node::Try { body, handler, .. } => {
            annotate_body(body, "body", ops, d, out);
            annotate_body(handler, "handler", ops, d, out);
        }
        Node::Parallel { branches } | Node::Race { branches, .. } => {
            for (i, b) in branches.iter().enumerate() {
                d.with(format!("branches[{i}]"), |d| {
                    annotate_body(&b.body, "body", ops, d, out)
                });
            }
        }
        Node::Verify { cmd, expect, .. } => {
            d.with("cmd", |d| annotate_node(cmd, ops, None, d, out));
            d.with("expect", |d| annotate_node(expect, ops, None, d, out));
        }
        Node::Return { value } => {
            d.with("value", |d| annotate_node(value, ops, None, d, out));
        }
        Node::Jq { input, .. } => {
            d.with("input", |d| annotate_node(input, ops, None, d, out));
        }
        Node::Parse { value, .. } => {
            d.with("value", |d| annotate_node(value, ops, None, d, out));
        }
        Node::Expr { vars, .. } => {
            for (k, v) in vars {
                d.with(format!("vars.{k}"), |d| annotate_node(v, ops, None, d, out));
            }
        }
        Node::Match {
            subject,
            cases,
            default,
        } => {
            d.with("subject", |d| annotate_node(subject, ops, None, d, out));
            for (i, c) in cases.iter().enumerate() {
                d.with(format!("cases[{i}]"), |d| {
                    d.with("value", |d| annotate_node(&c.value, ops, None, d, out));
                    annotate_body(&c.body, "body", ops, d, out);
                });
            }
            annotate_body(default, "default", ops, d, out);
        }
        Node::Route {
            selector,
            cases,
            default,
        } => {
            d.with("selector", |d| annotate_node(selector, ops, None, d, out));
            for (i, c) in cases.iter().enumerate() {
                d.with(format!("cases[{i}]"), |d| {
                    annotate_body(&c.body, "body", ops, d, out)
                });
            }
            annotate_body(default, "default", ops, d, out);
        }
        Node::Fallback { branches, .. } => {
            for (i, b) in branches.iter().enumerate() {
                d.with(format!("branches[{i}]"), |d| {
                    annotate_body(&b.body, "body", ops, d, out)
                });
            }
        }
        Node::Timeout { body, .. } | Node::Budget { body, .. } | Node::CapScope { body, .. } => {
            annotate_body(body, "body", ops, d, out)
        }
        Node::Scope {
            acquire,
            body,
            finally,
            ..
        } => {
            if let Some(acq) = acquire {
                d.with("acquire", |d| annotate_node(acq, ops, None, d, out));
            }
            annotate_body(body, "body", ops, d, out);
            annotate_body(finally, "finally", ops, d, out);
        }
        Node::Saga { steps } => {
            for (i, step) in steps.iter().enumerate() {
                d.with(format!("steps[{i}]"), |d| {
                    annotate_body(&step.body, "body", ops, d, out);
                    annotate_body(&step.undo, "undo", ops, d, out);
                });
            }
        }
        Node::Once { body, .. } => annotate_body(body, "body", ops, d, out),
        Node::Obj { fields } => {
            for (k, v) in fields {
                d.with(format!("fields.{k}"), |d| {
                    annotate_node(v, ops, None, d, out)
                });
            }
        }
        Node::List { items } => {
            for (i, it) in items.iter().enumerate() {
                d.with(format!("items[{i}]"), |d| {
                    annotate_node(it, ops, None, d, out)
                });
            }
        }
        // Leaf nodes: no nested node positions, so none can themselves be (or contain) a `call`.
        Node::Await { .. }
        | Node::Checkpoint { .. }
        | Node::Peek { .. }
        | Node::Var { .. }
        | Node::Lit { .. }
        | Node::Thing { .. }
        | Node::Fmt { .. }
        | Node::Ctx { .. }
        | Node::CtxAppend { .. } => {}
    }
}

/// Every symbol name `body` can bind at runtime, in ANY binder form, added to `out`. Deliberately
/// order-insensitive (L-15/F5): the definedness check wants **zero false positives** — it catches
/// the typo / never-bound class, while use-before-bind stays a precise runtime error. Binder
/// forms mirror the runtime's `bind`/`bind_existing` sites. Exhaustive on purpose (no `_ =>`,
/// F12): a new node kind must state here whether it binds.
fn collect_bound_symbols(body: &[Node], out: &mut HashSet<String>) {
    for_each_node(body, &mut |node| match node {
        Node::Bind { name, .. } | Node::Memo { name, .. } | Node::Ctx { name, .. } => {
            out.insert(name.0.clone());
        }
        Node::Each { item, collect, .. } => {
            out.insert(item.0.clone());
            if let Some(c) = collect {
                out.insert(c.0.clone());
            }
        }
        Node::Repeat { collect, .. } => {
            if let Some(c) = collect {
                out.insert(c.0.clone());
            }
        }
        // `race` branch names are NOT bound at runtime (only the winner's `bind` is) — they are
        // deliberately absent from this list.
        Node::Pipe { bind, .. }
        | Node::Seq { bind, .. }
        | Node::Retry { bind, .. }
        | Node::Loop { bind, .. }
        | Node::Race { bind, .. }
        | Node::Fallback { bind, .. }
        | Node::Timeout { bind, .. }
        | Node::Budget { bind, .. }
        | Node::CapScope { bind, .. }
        | Node::Scope { bind, .. }
        | Node::Once { bind, .. } => {
            if let Some(b) = bind {
                out.insert(b.0.clone());
            }
        }
        Node::Try { catch, .. } => {
            if let Some(c) = catch {
                out.insert(c.0.clone());
            }
        }
        Node::Await { binding, .. } => {
            if let Some(b) = binding {
                out.insert(b.0.clone());
            }
        }
        Node::Parallel { branches } => {
            for b in branches {
                out.insert(b.name.0.clone());
            }
        }
        // Non-binding kinds. (`ctx_append` rebinds an existing pack — it creates nothing.)
        Node::Call { .. }
        | Node::When { .. }
        | Node::Assert { .. }
        | Node::Confirm { .. }
        | Node::Throttle { .. }
        | Node::Debounce { .. }
        | Node::Unless { .. }
        | Node::Verify { .. }
        | Node::Return { .. }
        | Node::Peek { .. }
        | Node::Var { .. }
        | Node::Lit { .. }
        | Node::Thing { .. }
        | Node::Expr { .. }
        | Node::Fmt { .. }
        | Node::Jq { .. }
        | Node::Parse { .. }
        | Node::CtxAppend { .. }
        | Node::Match { .. }
        | Node::Route { .. }
        | Node::Saga { .. }
        | Node::Checkpoint { .. }
        | Node::Obj { .. }
        | Node::List { .. } => {}
    });
}

/// Reject a *declared* symbol name that is not a plain identifier (F8): a dotted name like `a.b`
/// silently reparses as field-access `jq(".b", $a)` through the text round-trip, so it must never
/// be declarable.
fn check_decl_name(name: &SymbolName, what: &str, d: &mut Diags) {
    if !name.is_identifier() {
        d.add(format!(
            "invalid symbol name `${}` declared by {what} — symbol names must be plain \
             identifiers (ASCII letters, digits, `_`); a dotted or spaced name silently changes \
             meaning through the text round-trip",
            name.0
        ));
    }
}

/// [`check_decl_name`] for the many optional `bind`-style declarations.
fn check_opt_decl_name(name: &Option<SymbolName>, what: &str, d: &mut Diags) {
    if let Some(n) = name {
        check_decl_name(n, what, d);
    }
}

/// Reject a condition node kind the runtime cannot evaluate. Coupled to the runtime's `eval_cond`
/// accepted set (runtime.rs, `fn eval_cond`): a `when`/`unless` condition, `repeat`/`loop`
/// `until` guard, or `assert` condition is evaluated only as `call`, `lit`, `var`, or `expr` —
/// every other kind is a runtime error, so reject it at analysis (F7).
fn check_cond_kind(cond: &Node, what: &str, d: &mut Diags) {
    if !matches!(
        cond,
        Node::Call { .. } | Node::Lit { .. } | Node::Var { .. } | Node::Expr { .. }
    ) {
        d.add(format!(
            "`{}` is not a valid {what} condition — the runtime evaluates only `call`, `lit`, \
             `var` ($symbol), or `expr` conditions; bind the value to a symbol first, then test \
             `$name`",
            node_kind_label(cond)
        ));
    }
}

/// Reject a node kind the runtime cannot resolve in an `eval_arg` position. Coupled to the
/// runtime's `eval_arg` accepted set (runtime.rs, `fn eval_arg`): an `each` source, `jq` input, or
/// `parse` value resolves WITHOUT dispatch and takes only `lit`, `var`, and the pure `obj`/`list`
/// templates — anything else (notably a `call`) is a runtime error, so reject it at analysis first
/// (L-21; the same F7 rule call arguments already get).
fn check_eval_arg_position(node: &Node, what: &str, d: &mut Diags) {
    if !matches!(
        node,
        Node::Lit { .. } | Node::Var { .. } | Node::Obj { .. } | Node::List { .. }
    ) {
        d.add(format!(
            "`{}` is not a valid {what} — the runtime accepts only `lit`, `var` ($symbol), and \
             `obj`/`list` templates here; bind it to a symbol first (`$x = …`), then use `$x`",
            node_kind_label(node)
        ));
    }
}

/// Walk a child statement body, extending the node path with `label[i]` per statement.
fn check_body(
    body: &[Node],
    label: &str,
    ops: &dyn OpCatalog,
    bound: &HashSet<String>,
    d: &mut Diags,
) {
    for (i, n) in body.iter().enumerate() {
        d.with(format!("{label}[{i}]"), |d| check_node(n, ops, bound, d));
    }
}

/// Recursively validate a node and its children: op resolution and arity, expression-position
/// legality, declared-name validity, symbol definedness against `bound` (the whole-flow binder
/// set plus params and session symbols), and per-kind structural guard-rails.
fn check_node(node: &Node, ops: &dyn OpCatalog, bound: &HashSet<String>, d: &mut Diags) {
    match node {
        Node::Call { op, args } => {
            if !is_valid_op_name(op) {
                d.add(format!(
                    "invalid operation name `{op}` — op names start with an ASCII letter or `_` \
                     and contain only letters, digits, `_`, `.`, `-`"
                ));
            }
            match ops.lookup(op) {
                None => d.add(format!("unknown operation: `{op}`")),
                Some(sig) => {
                    // Named-args semantics: a lone object is the whole input (exempt); a single bare
                    // value binds to the sole param; two+ bare values is the deprecated positional
                    // form — reject it so the repair loop rewrites the call with a named object.
                    // (`max == 0` ops are skipped: the catalog may be untyped, yet the op accepts a
                    // whole-input object at runtime and the runtime rejects a true overflow.)
                    // A lone `obj` **template** (e.g. `{role: "x", task: $prompt}`, a dynamic field)
                    // is exempt exactly like a lone `lit` object — `eval_arg`/`map_args_to_input`
                    // treat both identically at runtime (a template just resolves its fields first).
                    let lone_object = matches!(args.as_slice(), [Node::Lit { value }] if value.is_object())
                        || matches!(args.as_slice(), [Node::Obj { .. }]);
                    let max = sig.required_params.len() + sig.optional_params.len();
                    if !lone_object && max > 0 && args.len() >= 2 {
                        d.add(format!(
                            "op `{op}`: pass a single object argument naming its parameters \
                             (e.g. `{{\"{}\": …}}`) instead of {n} positional arguments",
                            sig.required_params
                                .first()
                                .or(sig.optional_params.first())
                                .cloned()
                                .unwrap_or_default(),
                            n = args.len()
                        ));
                    }
                    // A single bare value against a multi-param op is ambiguous without names —
                    // BUT a single required param (plus optionals) is the common ergonomic sugar
                    // (`read("x")`, `grep("TODO")`), so allow it.
                    let single_required = sig.required_params.len() == 1;
                    if !lone_object && max > 1 && args.len() == 1 && !single_required {
                        d.add(format!(
                            "op `{op}` takes {max} parameters; pass a single object argument naming \
                             each (e.g. `{{…}}`) instead of one bare value"
                        ));
                    }
                    // Too few: a call with NO args can never bind a required param (zero args cannot
                    // be the lone whole-input object). Surface it at compile time so the planner
                    // re-plans, instead of failing at runtime mid-execution after side effects.
                    if args.is_empty() && !sig.required_params.is_empty() {
                        d.add(format!(
                            "op `{op}` requires argument(s) {} but none were supplied",
                            sig.required_params
                                .iter()
                                .map(|p| format!("`{p}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    check_flux_expr_literal_params(op, args, &sig, ops, d);
                }
            }
            // Coupled to the runtime's `eval_arg` accepted set (runtime.rs, `fn eval_arg`):
            // argument positions resolve WITHOUT dispatch and take only `lit`, `var`, and the
            // pure `obj`/`list` templates — anything else is a runtime error, so reject it here
            // first (F7).
            for (i, a) in args.iter().enumerate() {
                d.with(format!("args[{i}]"), |d| {
                    if !matches!(
                        a,
                        Node::Lit { .. } | Node::Var { .. } | Node::Obj { .. } | Node::List { .. }
                    ) {
                        d.add(format!(
                            "`{}` is not a valid call argument — the runtime accepts only `lit`, \
                             `var` ($symbol), and `obj`/`list` templates in argument position; \
                             bind it to a symbol first (`$x = …`), then pass `$x`",
                            node_kind_label(a)
                        ));
                    }
                    check_node(a, ops, bound, d);
                });
            }
        }
        Node::Bind { name, value, .. } | Node::Memo { name, value, .. } => {
            check_decl_name(name, "`bind`/`memo`", d);
            d.with("value", |d| check_node(value, ops, bound, d));
        }
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            check_cond_kind(cond, "`when`", d);
            d.with("cond", |d| check_node(cond, ops, bound, d));
            check_body(then, "then", ops, bound, d);
            check_body(otherwise, "otherwise", ops, bound, d);
        }
        Node::Repeat {
            max,
            until,
            body,
            collect,
        } => {
            if *max == 0 {
                d.add(
                    "`repeat` requires a non-zero `max` (a `max: 0` loop can never run its body)",
                );
            }
            if *max > MAX_REPEAT_BOUND {
                d.add(format!(
                    "`repeat` `max` {max} exceeds the analyzer bound ({MAX_REPEAT_BOUND}) — \
                     plans must be plausibly bounded; lower the bound or restructure with `each`"
                ));
            }
            if body.is_empty() {
                d.add(
                    "`repeat` has an empty body — nothing runs and nothing is bound per \
                     iteration; put the op(s) in `body`",
                );
            }
            check_opt_decl_name(collect, "`repeat` `collect`", d);
            if let Some(u) = until {
                check_cond_kind(u, "`repeat` `until`", d);
                d.with("until", |d| check_node(u, ops, bound, d));
            }
            check_body(body, "body", ops, bound, d);
        }
        Node::Each {
            source,
            item,
            body,
            collect,
            ..
        } => {
            check_decl_name(item, "`each` `as`", d);
            check_opt_decl_name(collect, "`each` `collect`", d);
            if body.is_empty() {
                d.add(
                    "`each` has an empty body — nothing runs and nothing is bound per item; \
                     put the per-item op(s) in `body`",
                );
            }
            d.with("in", |d| {
                check_eval_arg_position(source, "`each` source", d);
                check_node(source, ops, bound, d)
            });
            check_body(body, "body", ops, bound, d);
        }
        Node::Assert { cond, .. } => {
            check_cond_kind(cond, "`assert`", d);
            d.with("cond", |d| check_node(cond, ops, bound, d));
        }
        Node::Pipe { steps, bind } => {
            check_opt_decl_name(bind, "`pipe` `bind`", d);
            for (i, s) in steps.iter().enumerate() {
                d.with(format!("steps[{i}]"), |d| {
                    if !matches!(s, Node::Call { .. }) {
                        d.add("`pipe` steps must be `call` nodes");
                    }
                    check_node(s, ops, bound, d);
                });
            }
        }
        Node::Seq { body, bind } => {
            check_opt_decl_name(bind, "`seq` `bind`", d);
            check_body(body, "body", ops, bound, d);
        }
        Node::Parallel { branches } => {
            let mut seen: HashSet<&str> = HashSet::new();
            // F15 (analyzer half): branches run concurrently against ONE shared symbol store, so
            // two branches binding the same symbol — via their branch names or ANY binder form
            // inside their bodies — race nondeterministically. Require bind-disjointness.
            let mut bound_by: HashMap<String, &str> = HashMap::new();
            for (i, b) in branches.iter().enumerate() {
                d.with(format!("branches[{i}]"), |d| {
                    check_decl_name(&b.name, "a `parallel` branch", d);
                    if !seen.insert(b.name.0.as_str()) {
                        d.add(format!("duplicate `parallel` branch name `${}`", b.name.0));
                    }
                    if body_contains_return(&b.body) {
                        d.add("`return` is not allowed inside a `parallel` branch");
                    }
                    if b.body.is_empty() {
                        d.add(format!(
                            "`parallel` branch `${0}` has an empty body — put the op(s) that \
                             produce its value in `body`, e.g. \
                             {{\"name\":\"{0}\",\"body\":[{{\"kind\":\"call\",...}}]}}",
                            b.name.0
                        ));
                    }
                    let mut binds: HashSet<String> = HashSet::new();
                    binds.insert(b.name.0.clone());
                    collect_bound_symbols(&b.body, &mut binds);
                    let mut binds: Vec<String> = binds.into_iter().collect();
                    binds.sort();
                    for sym in binds {
                        match bound_by.entry(sym) {
                            std::collections::hash_map::Entry::Occupied(e) => {
                                // Same-name branches are already the `duplicate` diagnostic above.
                                if *e.get() != b.name.0.as_str() {
                                    d.add(format!(
                                        "`parallel` branches `${}` and `${}` both bind `${}` — \
                                         concurrent branches must bind disjoint symbols",
                                        e.get(),
                                        b.name.0,
                                        e.key()
                                    ));
                                }
                            }
                            std::collections::hash_map::Entry::Vacant(v) => {
                                v.insert(b.name.0.as_str());
                            }
                        }
                    }
                    check_body(&b.body, "body", ops, bound, d);
                });
            }
        }
        Node::Return { value } => d.with("value", |d| check_node(value, ops, bound, d)),
        Node::Retry { body, bind, .. } => {
            check_opt_decl_name(bind, "`retry` `bind`", d);
            check_body(body, "body", ops, bound, d);
        }
        Node::Try {
            body,
            catch,
            handler,
        } => {
            check_opt_decl_name(catch, "`try` `catch`", d);
            check_body(body, "body", ops, bound, d);
            check_body(handler, "handler", ops, bound, d);
        }
        Node::Confirm { body, .. } => {
            check_body(body, "body", ops, bound, d);
        }
        Node::Race { branches, bind, .. } => {
            check_opt_decl_name(bind, "`race` `bind`", d);
            let mut seen: HashSet<&str> = HashSet::new();
            let mut bound_by: HashMap<String, &str> = HashMap::new();
            for (i, b) in branches.iter().enumerate() {
                d.with(format!("branches[{i}]"), |d| {
                    check_decl_name(&b.name, "a `race` branch", d);
                    if !seen.insert(b.name.0.as_str()) {
                        d.add(format!("duplicate `race` branch name `${}`", b.name.0));
                    }
                    if b.body.is_empty() {
                        d.add(format!(
                            "`race` branch `${}` has an empty body — an empty branch can never \
                             produce a value; put the op(s) in `body`",
                            b.name.0
                        ));
                    }
                    let mut binds: HashSet<String> = HashSet::new();
                    binds.insert(b.name.0.clone());
                    collect_bound_symbols(&b.body, &mut binds);
                    let mut binds: Vec<String> = binds.into_iter().collect();
                    binds.sort();
                    for sym in binds {
                        match bound_by.entry(sym) {
                            std::collections::hash_map::Entry::Occupied(entry) => {
                                if *entry.get() != b.name.0.as_str() {
                                    d.add(format!(
                                        "`race` branches `${}` and `${}` both bind `${}` — \
                                         concurrent branches must bind disjoint symbols",
                                        entry.get(),
                                        b.name.0,
                                        entry.key()
                                    ));
                                }
                            }
                            std::collections::hash_map::Entry::Vacant(entry) => {
                                entry.insert(b.name.0.as_str());
                            }
                        }
                    }
                    check_body(&b.body, "body", ops, bound, d);
                });
            }
        }
        Node::Throttle {
            max, name, body, ..
        } => {
            if *max == 0 {
                d.add("`throttle` requires a non-zero `max`");
            }
            if name.is_empty() {
                d.add("`throttle` requires a non-empty `name`");
            }
            check_body(body, "body", ops, bound, d);
        }
        Node::Debounce { name, body, .. } => {
            if name.is_empty() {
                d.add("`debounce` requires a non-empty `name`");
            }
            check_body(body, "body", ops, bound, d);
        }
        Node::Loop {
            until,
            body,
            for_ms,
            bind,
            ..
        } => {
            if *for_ms == 0 {
                d.add("`loop` requires a non-zero `for_ms` (unbounded loops are rejected)");
            }
            check_opt_decl_name(bind, "`loop` `bind`", d);
            if let Some(u) = until {
                check_cond_kind(u, "`loop` `until`", d);
                d.with("until", |d| check_node(u, ops, bound, d));
            }
            check_body(body, "body", ops, bound, d);
        }
        Node::Unless { cond, body } => {
            check_cond_kind(cond, "`unless`", d);
            d.with("cond", |d| check_node(cond, ops, bound, d));
            check_body(body, "body", ops, bound, d);
        }
        Node::Verify { cmd, expect, .. } => {
            // `cmd` dispatches (typically a `bash` call); `expect` is an `eval_arg` position
            // (runtime.rs `fn exec_body`, `Node::Verify` arm) — lit/var/obj/list only, no dispatch
            // — so reject a non-`eval_arg` `expect` up front (L-27).
            d.with("cmd", |d| check_node(cmd, ops, bound, d));
            d.with("expect", |d| {
                check_eval_arg_position(expect, "`verify` `expect`", d);
                check_node(expect, ops, bound, d)
            });
        }
        Node::Expr { formula, vars } => {
            for (k, v) in vars {
                d.with(format!("vars.{k}"), |d| check_node(v, ops, bound, d));
            }
            // The runtime tokenizes/evaluates `formula` against `vars` (runtime.rs `eval_expr_value`);
            // `vars` is an explicit, unambiguous scope, so an ident absent from it or a malformed
            // formula is a plan the runtime rejects — reject it here first with zero false positives
            // (L-27). Reuses the runtime tokenizer/parser so the two agree exactly.
            let keys: std::collections::BTreeSet<&str> = vars.keys().map(String::as_str).collect();
            for msg in crate::expr::validate_expr_formula(formula, &keys) {
                d.add(msg);
            }
        }
        Node::Fmt { .. } => {}
        Node::Jq { input, .. } => d.with("input", |d| {
            check_eval_arg_position(input, "`jq` input", d);
            check_node(input, ops, bound, d)
        }),
        Node::Parse { value, as_type } => {
            const VALID: &[&str] = &["f64", "i64", "bool", "json", "string"];
            if !VALID.contains(&as_type.as_str()) {
                d.add(format!(
                    "`parse` as_type must be one of f64/i64/bool/json/string, got `{as_type}`"
                ));
            }
            d.with("value", |d| {
                check_eval_arg_position(value, "`parse` value", d);
                check_node(value, ops, bound, d)
            });
        }
        Node::Ctx { name, budget, .. } => {
            check_decl_name(name, "`ctx`", d);
            if matches!(budget, Some(0)) {
                d.add("`ctx` budget must be non-zero (a 0-char budget drops every member)");
            }
        }
        Node::Match {
            subject,
            cases,
            default,
        } => {
            // The interpreter compares the subject by JSON equality, so it must be a value the
            // interpreter can resolve without dispatch — a literal or a bound symbol. To branch on an
            // op's result, bind it first (`$s = call(); match $s {…}`) or use `route`.
            if !matches!(subject.as_ref(), Node::Lit { .. } | Node::Var { .. }) {
                d.add(
                    "`match` subject must be a literal or a bound symbol (`$x`); bind a call result first, or use `route` to branch on an op",
                );
            }
            d.with("subject", |d| check_node(subject, ops, bound, d));
            if cases.is_empty() {
                d.add("`match` requires at least one case");
            }
            for (i, c) in cases.iter().enumerate() {
                d.with(format!("cases[{i}]"), |d| {
                    if !matches!(c.value, Node::Lit { .. } | Node::Var { .. }) {
                        d.add("`match` case values must be literals or bound symbols");
                    }
                    d.with("value", |d| check_node(&c.value, ops, bound, d));
                    check_body(&c.body, "body", ops, bound, d);
                });
            }
            check_body(default, "default", ops, bound, d);
        }
        Node::Route {
            selector,
            cases,
            default,
        } => {
            d.with("selector", |d| {
                // The runtime dispatches a `call` selector (the `!model` op) but resolves any other
                // kind via `eval_arg` (runtime.rs `fn exec_body`, `Node::Route` arm) — lit/var/obj/
                // list only — so reject a non-`call`, non-`eval_arg` selector up front (L-27).
                if !matches!(selector.as_ref(), Node::Call { .. }) {
                    check_eval_arg_position(selector, "`route` selector", d);
                }
                check_node(selector, ops, bound, d)
            });
            if cases.is_empty() {
                d.add("`route` requires at least one case");
            }
            let mut seen: HashSet<&str> = HashSet::new();
            for (i, c) in cases.iter().enumerate() {
                d.with(format!("cases[{i}]"), |d| {
                    if c.label.is_empty() {
                        d.add("`route` case labels must be non-empty");
                    }
                    if !seen.insert(c.label.as_str()) {
                        d.add(format!("duplicate `route` case label `{}`", c.label));
                    }
                    check_body(&c.body, "body", ops, bound, d);
                });
            }
            check_body(default, "default", ops, bound, d);
        }
        Node::Fallback { branches, bind } => {
            check_opt_decl_name(bind, "`fallback` `bind`", d);
            if branches.is_empty() {
                d.add("`fallback` requires at least one branch");
            }
            for (i, b) in branches.iter().enumerate() {
                d.with(format!("branches[{i}]"), |d| {
                    check_body(&b.body, "body", ops, bound, d)
                });
            }
        }
        Node::Timeout { ms, body, bind } => {
            if *ms == 0 {
                d.add("`timeout` requires a non-zero `ms`");
            }
            check_opt_decl_name(bind, "`timeout` `bind`", d);
            check_body(body, "body", ops, bound, d);
        }
        Node::Budget { limit, body, bind } => {
            if *limit == 0 {
                d.add("`budget` requires a non-zero `limit`");
            }
            check_opt_decl_name(bind, "`budget` `bind`", d);
            check_body(body, "body", ops, bound, d);
        }
        Node::CapScope { body, bind, .. } => {
            // Op-name/type validation is unconditional (same as every other block); the *scope*-aware
            // "this literal op is outside the allowlist" diagnostic is a separate pass
            // (`check_cap_scopes`) run once over the whole flow, since it must thread the narrowing
            // allowlist through nested scopes — information `check_node`'s signature doesn't carry.
            check_opt_decl_name(bind, "`with_tools` `bind`", d);
            check_body(body, "body", ops, bound, d);
        }
        Node::Scope {
            acquire,
            bind,
            body,
            finally,
        } => {
            check_opt_decl_name(bind, "`scope` `bind`", d);
            // `bind` names the *acquired resource*, so it only makes sense with an `acquire`.
            if bind.is_some() && acquire.is_none() {
                d.add("`scope` binds the acquired resource — `-> $name` requires an `acquire`");
            }
            if let Some(acq) = acquire {
                d.with("acquire", |d| check_node(acq, ops, bound, d));
            }
            check_body(body, "body", ops, bound, d);
            check_body(finally, "finally", ops, bound, d);
        }
        Node::Saga { steps } => {
            if steps.is_empty() {
                d.add("`saga` requires at least one step");
            }
            for (i, step) in steps.iter().enumerate() {
                d.with(format!("steps[{i}]"), |d| {
                    check_body(&step.body, "body", ops, bound, d);
                    check_body(&step.undo, "undo", ops, bound, d);
                });
            }
        }
        Node::Once { label, body, bind } => {
            // The label is the durable idempotency key, so it must be a fixed, auditable string.
            if label.trim().is_empty() {
                d.add("`once` requires a non-empty label (its durable idempotency key)");
            }
            check_opt_decl_name(bind, "`once` `bind`", d);
            check_body(body, "body", ops, bound, d);
        }
        Node::Checkpoint { label } => {
            if label.trim().is_empty() {
                d.add("`checkpoint` requires a non-empty label (its durable resume key)");
            }
        }
        Node::Obj { fields } => {
            for (k, v) in fields {
                d.with(format!("fields.{k}"), |d| {
                    check_template_leaf(v, ops, bound, d)
                });
            }
        }
        Node::List { items } => {
            for (i, it) in items.iter().enumerate() {
                d.with(format!("items[{i}]"), |d| {
                    check_template_leaf(it, ops, bound, d)
                });
            }
        }
        Node::Await { binding, .. } => {
            check_opt_decl_name(binding, "`await` `binding`", d);
        }
        Node::Var { name } => {
            // Reference-side of F8: a dotted/spaced `var` name can never be satisfied (no binder
            // may declare one) and silently reparses as field access through the text round-trip.
            if !name.is_identifier() {
                d.add(format!(
                    "invalid symbol reference `${}` — symbol names must be plain identifiers \
                     (ASCII letters, digits, `_`); for field access bind the base symbol and use \
                     `jq`",
                    name.0
                ));
            } else if !bound.contains(&name.0) {
                // L-15/F5: definedness. `bound` already contains every binder form anywhere in
                // the flow (order-insensitive), the flow params, and the session's symbols — so
                // this can only be a typo or a reference to a value that nothing produces.
                d.add(format!(
                    "unbound symbol `${}` — it is not a flow param, is never bound by any \
                     statement in this flow, and is not a session symbol; bind it first or fix \
                     the name",
                    name.0
                ));
            }
        }
        Node::Peek { .. } | Node::Lit { .. } | Node::Thing { .. } | Node::CtxAppend { .. } => {}
    }
}

fn check_flux_expr_literal_params(
    op: &str,
    args: &[Node],
    sig: &OpSignature,
    ops: &dyn OpCatalog,
    d: &mut Diags,
) {
    match args {
        [Node::Lit {
            value: serde_json::Value::Object(map),
        }] => {
            let vars = flux_expr_var_keys_from_json(map);
            for (param, value) in map {
                if ops.param_format(op, param).as_deref() == Some("flux-expr") {
                    if let Some(formula) = value.as_str() {
                        validate_flux_expr_param(op, param, formula, &vars, d);
                    }
                }
            }
        }
        [Node::Obj { fields }] => {
            let vars = flux_expr_var_keys_from_template(fields);
            for (param, value) in fields {
                if ops.param_format(op, param).as_deref() == Some("flux-expr") {
                    if let Node::Lit {
                        value: serde_json::Value::String(formula),
                    } = value.as_ref()
                    {
                        validate_flux_expr_param(op, param, formula, &vars, d);
                    }
                }
            }
        }
        [Node::Lit {
            value: serde_json::Value::String(formula),
        }] => {
            let Some(param) = single_bare_param(sig) else {
                return;
            };
            if ops.param_format(op, &param).as_deref() == Some("flux-expr") {
                let vars = BTreeSet::from(["it".to_string()]);
                validate_flux_expr_param(op, &param, formula, &vars, d);
            }
        }
        _ => {}
    }
}

fn single_bare_param(sig: &OpSignature) -> Option<String> {
    if sig.required_params.len() == 1 {
        sig.required_params.first().cloned()
    } else if sig.required_params.is_empty() && sig.optional_params.len() == 1 {
        sig.optional_params.first().cloned()
    } else {
        None
    }
}

fn flux_expr_var_keys_from_json(
    map: &serde_json::Map<String, serde_json::Value>,
) -> BTreeSet<String> {
    let mut keys = BTreeSet::from(["it".to_string()]);
    if let Some(vars) = map.get("vars").and_then(|v| v.as_object()) {
        keys.extend(vars.keys().cloned());
    }
    keys
}

fn flux_expr_var_keys_from_template(fields: &BTreeMap<String, Box<Node>>) -> BTreeSet<String> {
    let mut keys = BTreeSet::from(["it".to_string()]);
    if let Some(vars) = fields.get("vars") {
        match vars.as_ref() {
            Node::Lit {
                value: serde_json::Value::Object(map),
            } => keys.extend(map.keys().cloned()),
            Node::Obj { fields } => keys.extend(fields.keys().cloned()),
            _ => {}
        }
    }
    keys
}

fn validate_flux_expr_param(
    op: &str,
    param: &str,
    formula: &str,
    var_keys: &BTreeSet<String>,
    d: &mut Diags,
) {
    let refs: BTreeSet<&str> = var_keys.iter().map(String::as_str).collect();
    for msg in crate::expr::validate_expr_formula(formula, &refs) {
        d.add(format!(
            "op `{op}` parameter `{param}` has invalid flux expression: {msg}"
        ));
    }
}

/// A value template (`obj`/`list`) assembles a value with no dispatch, so each leaf must be a **pure
/// value node** (`var`/`lit`/`jq`/`expr`/`fmt`/`parse`/`obj`/`list`). A `call` or control-flow leaf
/// would smuggle side effects into a notionally-pure template, so it is rejected — bind it to a
/// symbol first, then reference `$name`. Recurses so nested templates are checked too.
fn check_template_leaf(node: &Node, ops: &dyn OpCatalog, bound: &HashSet<String>, d: &mut Diags) {
    if !matches!(
        node,
        Node::Var { .. }
            | Node::Lit { .. }
            | Node::Jq { .. }
            | Node::Expr { .. }
            | Node::Fmt { .. }
            | Node::Parse { .. }
            | Node::Obj { .. }
            | Node::List { .. }
    ) {
        d.add(
            "a value template (`obj`/`list`) may only contain pure value leaves \
             (`var`/`lit`/`jq`/`expr`/`fmt`/`parse`/`obj`/`list`); bind a call or control-flow result \
             to a symbol first, then reference it as `$name`",
        );
    }
    // Recurse regardless, so a nested issue (e.g. an unknown op inside the offending call) also surfaces.
    check_node(node, ops, bound, d);
}

/// Whether any statement in `body` is (or reaches, through nested control flow) a `return`. Used to
/// reject `return` inside a `parallel` branch, where which branch's return should win is ambiguous.
/// A nested `parallel`'s own branches are validated separately, so their returns don't count here.
fn body_contains_return(body: &[Node]) -> bool {
    body.iter().any(node_contains_return)
}

/// Exhaustive on purpose (no `_ =>`, F12): a new node kind must state here whether executing it
/// can reach a `return` statement.
fn node_contains_return(node: &Node) -> bool {
    match node {
        Node::Return { .. } => true,
        Node::When {
            then, otherwise, ..
        } => body_contains_return(then) || body_contains_return(otherwise),
        Node::Repeat { body, .. }
        | Node::Each { body, .. }
        | Node::Seq { body, .. }
        | Node::Retry { body, .. }
        | Node::Confirm { body, .. }
        | Node::Loop { body, .. }
        | Node::Throttle { body, .. }
        | Node::Debounce { body, .. }
        | Node::Unless { body, .. }
        | Node::Timeout { body, .. }
        | Node::Budget { body, .. }
        | Node::CapScope { body, .. }
        | Node::Once { body, .. } => body_contains_return(body),
        Node::Try { body, handler, .. } => {
            body_contains_return(body) || body_contains_return(handler)
        }
        Node::Race { branches, .. } => branches.iter().any(|b| body_contains_return(&b.body)),
        Node::Match { cases, default, .. } => {
            cases.iter().any(|c| body_contains_return(&c.body)) || body_contains_return(default)
        }
        Node::Route { cases, default, .. } => {
            cases.iter().any(|c| body_contains_return(&c.body)) || body_contains_return(default)
        }
        Node::Fallback { branches, .. } => branches.iter().any(|b| body_contains_return(&b.body)),
        Node::Scope { body, finally, .. } => {
            body_contains_return(body) || body_contains_return(finally)
        }
        Node::Saga { steps } => steps
            .iter()
            .any(|s| body_contains_return(&s.body) || body_contains_return(&s.undo)),
        // A nested `parallel`'s own branches are validated separately (its own `return` rule),
        // so their returns intentionally don't count here.
        Node::Parallel { .. } => false,
        // Expression / leaf positions cannot execute a `return` statement: bind/memo values are
        // expressions (a statement value is a runtime error), `pipe` steps must be calls, and the
        // rest carry no statement bodies at all.
        Node::Call { .. }
        | Node::Bind { .. }
        | Node::Memo { .. }
        | Node::Assert { .. }
        | Node::Pipe { .. }
        | Node::Await { .. }
        | Node::Verify { .. }
        | Node::Peek { .. }
        | Node::Var { .. }
        | Node::Lit { .. }
        | Node::Thing { .. }
        | Node::Expr { .. }
        | Node::Fmt { .. }
        | Node::Jq { .. }
        | Node::Parse { .. }
        | Node::Ctx { .. }
        | Node::CtxAppend { .. }
        | Node::Checkpoint { .. }
        | Node::Obj { .. }
        | Node::List { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opspec::OpSignature;

    /// A minimal in-memory [`OpCatalog`] for analyzer tests — keeps this module free of any
    /// dependency on the concrete tool registry (`flux-runtime`/`flux-tools`).
    struct MockCatalog(Vec<String>);

    impl OpCatalog for MockCatalog {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            self.0
                .iter()
                .find(|n| n.as_str() == name)
                .map(|n| OpSignature {
                    name: n.clone(),
                    description: String::new(),
                    effects: Vec::new(),
                    risk: flux_spec::Risk::Low,
                    idempotency: flux_spec::Idempotency::Idempotent,
                    required_params: Vec::new(),
                    optional_params: Vec::new(),
                    param_types: Default::default(),
                    semantic_effects: Vec::new(),
                })
        }
    }

    /// The handful of op names the analyzer tests reference.
    fn catalog() -> MockCatalog {
        MockCatalog(vec!["read".into(), "grep".into(), "write".into()])
    }

    /// A richer catalog whose ops carry effects + params, for the HIR lowering / arity tests.
    struct TypedCatalog;
    impl OpCatalog for TypedCatalog {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            let sig = |effects, required: &[&str], optional: &[&str]| OpSignature {
                name: name.into(),
                description: String::new(),
                effects,
                risk: flux_spec::Risk::Low,
                idempotency: flux_spec::Idempotency::Idempotent,
                required_params: required.iter().map(|s| s.to_string()).collect(),
                optional_params: optional.iter().map(|s| s.to_string()).collect(),
                param_types: Default::default(),
                semantic_effects: Vec::new(),
            };
            match name {
                "read" => Some(sig(vec![flux_spec::Effect::Read], &["path"], &[])),
                "write" => Some(sig(
                    vec![flux_spec::Effect::Write, flux_spec::Effect::Filesystem],
                    &["path", "content"],
                    &[],
                )),
                _ => None,
            }
        }
    }

    #[test]
    fn lower_gathers_effects_and_named_args_are_validated() {
        use crate::ast::{Node, TypeRef};
        let ops = TypedCatalog;

        // `write` is a multi-param op: it must be called with a single named object argument.
        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "x".into(),
                    value: Box::new(Node::Call {
                        op: "read".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!("a"),
                        }],
                    }),
                    ty: None,
                    // a declared semantic effect is gathered verbatim
                    effect: Some(FlowEffect::Model),
                },
                Node::Call {
                    op: "write".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!({"path": "p", "content": "c"}),
                    }],
                },
            ],
            ..Default::default()
        };
        let hir: HirFlow = lower(&ast, &ops, &HashSet::new()).unwrap();
        // Read (from `read`) + WriteFile (from `write`) + Model (declared) — deduped.
        assert!(hir.effects.contains(&FlowEffect::Read));
        assert!(hir.effects.contains(&FlowEffect::WriteFile));
        assert!(hir.effects.contains(&FlowEffect::Model));
        let _ = TypeRef::Any;

        // The deprecated positional form (2+ bare args) is rejected — the model must rewrite the
        // call with a named object argument. (Failing-first for the named-args semantics.)
        let positional = DraftAst {
            body: vec![Node::Call {
                op: "write".into(),
                args: vec![
                    Node::Lit {
                        value: serde_json::json!("p"),
                    },
                    Node::Lit {
                        value: serde_json::json!("c"),
                    },
                ],
            }],
            ..Default::default()
        };
        let err = lower(&positional, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("single object argument")),
            "expected a named-object-argument diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        // A single bare value against a multi-param op is ambiguous without names — rejected.
        let bare = DraftAst {
            body: vec![Node::Call {
                op: "write".into(),
                args: vec![Node::Lit {
                    value: serde_json::json!("p"),
                }],
            }],
            ..Default::default()
        };
        let err = lower(&bare, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("single object argument naming each")),
            "expected a single-bare-vs-multi-param diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// A required-param op called with NO args is rejected at analysis — the `python_run`-class
    /// mistake. Zero args can never bind a required input, so it must surface as a compile error
    /// (re-plannable) rather than failing at runtime after side effects.
    #[test]
    fn required_op_with_no_args_is_rejected() {
        use crate::ast::Node;
        let ops = TypedCatalog;
        let empty = DraftAst {
            body: vec![Node::Call {
                op: "read".into(),
                args: vec![],
            }],
            ..Default::default()
        };
        let err = lower(&empty, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("requires argument(s)") && d.message.contains("`path`")),
            "expected a missing-required-arg diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// A lone `obj` **template** argument (a dynamic field, e.g. `{path: "p", content: $x}`) is the
    /// named-input map exactly like a lone `lit` object — `eval_arg`/`map_args_to_input` treat both
    /// identically at runtime (a template just resolves its fields first). The analyzer must not
    /// reject it as an ambiguous "single bare value against a multi-param op": that misclassification
    /// blocked any multi-param op call whose object literal embeds so much as one `$var`/`fmt` field
    /// (e.g. `task({role: "x", task: $prompt})` — L-10's strict-review flow needed exactly this shape).
    #[test]
    fn lone_obj_template_argument_is_the_named_input_not_a_bare_value() {
        use crate::ast::{Node, SymbolName};
        let ops = TypedCatalog;
        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "c".into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!("dynamic content"),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Call {
                    op: "write".into(),
                    args: vec![Node::Obj {
                        fields: [
                            (
                                "path".to_string(),
                                Box::new(Node::Lit {
                                    value: serde_json::json!("p"),
                                }),
                            ),
                            (
                                "content".to_string(),
                                Box::new(Node::Var {
                                    name: SymbolName("c".into()),
                                }),
                            ),
                        ]
                        .into_iter()
                        .collect(),
                    }],
                },
            ],
            ..Default::default()
        };
        let hir = lower(&ast, &ops, &HashSet::new());
        assert!(
            hir.is_ok(),
            "a lone obj-template arg with a dynamic field must analyze cleanly, got: {:?}",
            hir.err()
        );
    }

    /// The P6b control-flow primitives carry their own structural guard-rails, and `return` inside a
    /// `parallel` branch is still rejected when it hides inside one of them.
    #[test]
    fn control_flow_primitives_are_validated() {
        use crate::ast::{Branch, FallbackBranch, MatchCase, RouteCase, SymbolName};
        let ops = catalog();
        let lit = |v: &str| Node::Lit {
            value: serde_json::json!(v),
        };
        let has = |ast: &DraftAst, needle: &str| {
            lower(ast, &ops, &HashSet::new())
                .err()
                .is_some_and(|ds| ds.iter().any(|d| d.message.contains(needle)))
        };
        let wrap = |n: Node| DraftAst {
            body: vec![n],
            ..Default::default()
        };

        // match / route require at least one case.
        assert!(has(
            &wrap(Node::Match {
                subject: Box::new(lit("x")),
                cases: vec![],
                default: vec![],
            }),
            "`match` requires at least one case"
        ));
        assert!(has(
            &wrap(Node::Route {
                selector: Box::new(lit("x")),
                cases: vec![],
                default: vec![],
            }),
            "`route` requires at least one case"
        ));

        // route case labels must be non-empty and distinct.
        assert!(has(
            &wrap(Node::Route {
                selector: Box::new(lit("x")),
                cases: vec![
                    RouteCase {
                        label: "a".into(),
                        body: vec![]
                    },
                    RouteCase {
                        label: "a".into(),
                        body: vec![]
                    },
                ],
                default: vec![],
            }),
            "duplicate `route` case label"
        ));

        // timeout / budget reject a zero bound.
        assert!(has(
            &wrap(Node::Timeout {
                ms: 0,
                body: vec![],
                bind: None,
            }),
            "`timeout` requires a non-zero `ms`"
        ));
        assert!(has(
            &wrap(Node::Budget {
                limit: 0,
                body: vec![],
                bind: None,
            }),
            "`budget` requires a non-zero `limit`"
        ));

        // a `return` buried in a match case inside a parallel branch is still rejected.
        let parallel_with_buried_return = wrap(Node::Parallel {
            branches: vec![Branch {
                name: SymbolName("b".into()),
                body: vec![Node::Match {
                    subject: Box::new(lit("x")),
                    cases: vec![MatchCase {
                        value: lit("x"),
                        body: vec![Node::Return {
                            value: Box::new(lit("v")),
                        }],
                    }],
                    default: vec![],
                }],
            }],
        });
        assert!(has(
            &parallel_with_buried_return,
            "`return` is not allowed inside a `parallel` branch"
        ));

        // a `match` subject must be a value (literal/symbol), not an inline call — the interpreter
        // can't dispatch it; the author binds the result first or uses `route`.
        assert!(has(
            &wrap(Node::Match {
                subject: Box::new(Node::Call {
                    op: "read".into(),
                    args: vec![lit("a")],
                }),
                cases: vec![MatchCase {
                    value: lit("x"),
                    body: vec![],
                }],
                default: vec![],
            }),
            "`match` subject must be a literal or a bound symbol"
        ));

        // an empty `fallback` is rejected (symmetry with match/route).
        assert!(has(
            &wrap(Node::Fallback {
                branches: vec![],
                bind: None,
            }),
            "`fallback` requires at least one branch"
        ));

        // a well-formed fallback analyzes clean.
        let ok = wrap(Node::Fallback {
            branches: vec![FallbackBranch {
                body: vec![Node::Call {
                    op: "read".into(),
                    args: vec![lit("a")],
                }],
            }],
            bind: None,
        });
        let r = lower(&ok, &ops, &HashSet::new());
        assert!(
            r.is_ok(),
            "well-formed fallback should analyze clean: {:?}",
            r.err()
        );
    }

    /// `await` suspends the *whole* flow, so it is only valid as a top-level statement; nesting one is
    /// an analysis error (a clean diagnostic, not a runtime failure).
    #[test]
    fn await_must_be_a_top_level_statement() {
        let ops = catalog();
        let await_node = || Node::Await {
            binding: None,
            source: "user_input".into(),
            as_type: None,
        };
        let lit = || Node::Lit {
            value: serde_json::json!("x"),
        };

        // nested inside a `when` → rejected.
        let nested = DraftAst {
            body: vec![Node::When {
                cond: Box::new(lit()),
                then: vec![await_node()],
                otherwise: vec![],
            }],
            ..Default::default()
        };
        let err = analyze_flow(&nested, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter().any(|d| d
                .message
                .contains("`await` must be a top-level flow statement")),
            "nested await is rejected"
        );

        // a top-level await analyzes clean.
        let top = DraftAst {
            body: vec![await_node()],
            ..Default::default()
        };
        assert!(analyze_flow(&top, &ops, &HashSet::new()).is_ok());
    }

    #[test]
    fn checkpoint_must_be_a_top_level_statement() {
        let ops = catalog();
        let cp = || Node::Checkpoint { label: "p1".into() };

        // nested inside a `repeat` → rejected (no stable resume cursor).
        let nested = DraftAst {
            body: vec![Node::Repeat {
                max: 2,
                until: None,
                body: vec![cp()],
                collect: None,
            }],
            ..Default::default()
        };
        let err = analyze_flow(&nested, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter().any(|d| d
                .message
                .contains("`checkpoint` must be a top-level flow statement")),
            "nested checkpoint is rejected"
        );

        // a top-level checkpoint analyzes clean; an empty label is rejected.
        let top = DraftAst {
            body: vec![cp()],
            ..Default::default()
        };
        assert!(analyze_flow(&top, &ops, &HashSet::new()).is_ok());

        let empty = DraftAst {
            body: vec![Node::Checkpoint { label: "".into() }],
            ..Default::default()
        };
        assert!(analyze_flow(&empty, &ops, &HashSet::new()).is_err());
    }

    /// A catalog with a typed op `dbl(n: Number)` for the argument type-checker.
    struct TypeCat;
    impl OpCatalog for TypeCat {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            (name == "dbl").then(|| OpSignature {
                name: "dbl".into(),
                description: String::new(),
                effects: Vec::new(),
                risk: flux_spec::Risk::Low,
                idempotency: flux_spec::Idempotency::Idempotent,
                required_params: vec!["n".into()],
                optional_params: Vec::new(),
                param_types: [("n".to_string(), crate::ast::TypeRef::Number)]
                    .into_iter()
                    .collect(),
                semantic_effects: Vec::new(),
            })
        }
    }

    /// A catalog with a `where` parameter marked as `format: flux-expr`, mirroring the
    /// transform/predicate cognition ops.
    struct FluxExprCat;
    impl OpCatalog for FluxExprCat {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            (name == "filter").then(|| OpSignature {
                name: "filter".into(),
                description: String::new(),
                effects: Vec::new(),
                risk: flux_spec::Risk::Low,
                idempotency: flux_spec::Idempotency::Idempotent,
                required_params: vec!["items".into()],
                optional_params: vec!["vars".into(), "where".into()],
                param_types: Default::default(),
                semantic_effects: Vec::new(),
            })
        }

        fn param_format(&self, op: &str, param: &str) -> Option<String> {
            (op == "filter" && param == "where").then(|| "flux-expr".to_string())
        }
    }

    #[test]
    fn analyzer_rejects_bad_literal_flux_expr_predicate() {
        let ast = DraftAst {
            body: vec![Node::Call {
                op: "filter".into(),
                args: vec![Node::Lit {
                    value: serde_json::json!({
                        "items": [{"score": 10}],
                        "where": "it.score >",
                    }),
                }],
            }],
            ..Default::default()
        };

        let err = analyze_flow(&ast, &FluxExprCat, &HashSet::new()).unwrap_err();
        assert!(
            err.iter().any(|d| {
                d.message
                    .contains("op `filter` parameter `where` has invalid flux expression")
                    && d.message.contains("body[0]")
            }),
            "expected an early flux-expr diagnostic with a node path, got {err:?}"
        );
    }

    #[test]
    fn analyzer_accepts_literal_flux_expr_predicate_vars() {
        let ast = DraftAst {
            body: vec![Node::Call {
                op: "filter".into(),
                args: vec![Node::Lit {
                    value: serde_json::json!({
                        "items": [{"score": 10}],
                        "where": "it.score > min",
                        "vars": {"min": 5},
                    }),
                }],
            }],
            ..Default::default()
        };

        assert!(analyze_flow(&ast, &FluxExprCat, &HashSet::new()).is_ok());
    }

    #[test]
    fn lower_type_checks_call_arguments() {
        use crate::ast::{Node, TypeRef};
        let call_dbl = |arg: Node| DraftAst {
            body: vec![Node::Call {
                op: "dbl".into(),
                args: vec![arg],
            }],
            ..Default::default()
        };

        // A string literal where the op wants a Number is rejected.
        let bad = call_dbl(Node::Lit {
            value: serde_json::json!("hello"),
        });
        let err = lower(&bad, &TypeCat, &HashSet::new()).unwrap_err();
        assert!(
            err.iter().any(|d| d.message.contains("expects Number")),
            "expected a Number-mismatch diagnostic, got {err:?}"
        );

        // A number literal passes.
        let good = call_dbl(Node::Lit {
            value: serde_json::json!(5),
        });
        assert!(lower(&good, &TypeCat, &HashSet::new()).is_ok());

        // A var of unknown (Any) type passes leniently — no false positive.
        let lenient = DraftAst {
            body: vec![
                Node::Bind {
                    name: "x".into(),
                    value: Box::new(Node::Call {
                        op: "dbl".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!(1),
                        }],
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Call {
                    op: "dbl".into(),
                    args: vec![Node::Var { name: "x".into() }],
                },
            ],
            ..Default::default()
        };
        assert!(
            lower(&lenient, &TypeCat, &HashSet::new()).is_ok(),
            "an Any-typed var argument must pass leniently"
        );

        // A param declared `Number` is tracked: passing it where a Number is wanted is fine; a
        // String-typed param would conflict.
        let _ = TypeRef::Number;
    }

    /// L-21: `type_check_body` diagnostics carry the same JSON-pointer node paths the structural
    /// pass renders (`body[1].then[0]`), so a repairing model can locate the mistyped call — they
    /// were previously path-less while `analyze_flow`'s carried paths (L-16/F11).
    #[test]
    fn type_diagnostics_carry_node_paths() {
        use crate::ast::Node;
        let ast = DraftAst {
            body: vec![
                Node::Call {
                    op: "dbl".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!(1),
                    }],
                },
                Node::When {
                    cond: Box::new(Node::Lit {
                        value: serde_json::json!(true),
                    }),
                    then: vec![Node::Call {
                        op: "dbl".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!("hello"),
                        }],
                    }],
                    otherwise: vec![],
                },
            ],
            ..Default::default()
        };
        let err = lower(&ast, &TypeCat, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("expects Number")
                    && d.message.contains("body[1].then[0]")),
            "expected the type diagnostic to carry its node path, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn known_op_passes_and_unknown_op_fails() {
        let ops = catalog();

        assert!(analyze_call("read", &ops).is_ok());

        let err = analyze_call("does.not.exist", &ops).unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("unknown operation"));
    }

    #[test]
    fn analyze_flow_validates_nested_calls() {
        use crate::ast::{DraftAst, Node};
        let ops = catalog();

        let good = DraftAst {
            body: vec![Node::Call {
                op: "read".into(),
                args: vec![],
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&good, &ops, &HashSet::new()).is_ok());

        let bad = DraftAst {
            body: vec![Node::Return {
                value: Box::new(Node::Call {
                    op: "nope.op".into(),
                    args: vec![],
                }),
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&bad, &ops, &HashSet::new()).is_err());
    }

    #[test]
    fn analyze_validates_nested_calls_in_new_containers() {
        use crate::ast::{Branch, DraftAst, Node};
        let ops = catalog();

        // An unknown op reached only through `each`/`parallel` bodies is still caught.
        let bad = DraftAst {
            body: vec![
                Node::Each {
                    source: Box::new(Node::Lit {
                        value: serde_json::json!([1]),
                    }),
                    item: "x".into(),
                    body: vec![Node::Call {
                        op: "nope.each".into(),
                        args: vec![],
                    }],
                    collect: None,
                    flat: false,
                },
                Node::Parallel {
                    branches: vec![Branch {
                        name: "b".into(),
                        body: vec![Node::Call {
                            op: "nope.par".into(),
                            args: vec![],
                        }],
                    }],
                },
            ],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert_eq!(diags.len(), 2, "both nested unknown ops are reported");
    }

    #[test]
    fn analyze_rejects_pipe_with_a_non_call_step() {
        use crate::ast::{DraftAst, Node};
        let ops = catalog();

        let bad = DraftAst {
            body: vec![Node::Pipe {
                steps: vec![Node::Lit {
                    value: serde_json::json!("x"),
                }],
                bind: None,
            }],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(diags.iter().any(|d| d.message.contains("pipe")));
    }

    #[test]
    fn analyze_rejects_an_impure_template_leaf_but_accepts_a_pure_one() {
        use crate::ast::{DraftAst, Node};
        let ops = catalog();

        // A `call` (side-effecting) leaf inside a record template is rejected — templates stay pure.
        let bad: Node = serde_json::from_value(serde_json::json!({
            "kind": "obj",
            "fields": { "x": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "f"}]} }
        }))
        .unwrap();
        let bad_ast = DraftAst {
            body: vec![Node::Bind {
                name: "r".into(),
                value: Box::new(bad),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        let diags = analyze_flow(&bad_ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            diags.iter().any(|d| d.message.contains("value template")),
            "expected a template-leaf diagnostic, got: {diags:?}"
        );

        // The pure version (field-access + literal + nested list) analyzes clean. `$x` is bound
        // first — the L-15 definedness check rejects a reference nothing binds.
        let good: Node = serde_json::from_value(serde_json::json!({
            "kind": "obj",
            "fields": {
                "intent": {"kind": "jq", "path": ".intent", "input": {"kind": "var", "name": "x"}},
                "ok": {"kind": "lit", "value": true},
                "items": {"kind": "list", "items": [{"kind": "var", "name": "x"}]}
            }
        }))
        .unwrap();
        let good_ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "x".into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!({"intent": "demo"}),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Bind {
                    name: "r".into(),
                    value: Box::new(good),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        assert!(analyze_flow(&good_ast, &ops, &HashSet::new()).is_ok());
    }

    #[test]
    fn analyze_accepts_parse_as_a_template_leaf() {
        use crate::ast::{DraftAst, Node};
        let ops = catalog();
        // `{ data: parse($x, "json") }` — `parse` is a pure coercion, so it composes as a template
        // leaf like the other pure nodes (F-012); before the fix the whitelist rejected it.
        let tmpl: Node = serde_json::from_value(serde_json::json!({
            "kind": "obj",
            "fields": { "data": {"kind": "parse", "value": {"kind": "var", "name": "x"}, "as": "json"} }
        }))
        .unwrap();
        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "x".into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!("{\"a\":1}"),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Bind {
                    name: "r".into(),
                    value: Box::new(tmpl),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        assert!(
            analyze_flow(&ast, &ops, &HashSet::new()).is_ok(),
            "parse is a valid pure template leaf"
        );
    }

    #[test]
    fn analyze_rejects_parallel_return_inside_unless() {
        use crate::ast::{Branch, DraftAst, Node};
        let ops = catalog();

        // A `return` nested inside an `unless` body that lives inside a `parallel`
        // branch must still be detected — the bug was that `node_contains_return`
        // had no arm for `Node::Unless`, so it fell through to `_ => false`.
        let bad = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![Branch {
                    name: "b".into(),
                    body: vec![Node::Unless {
                        cond: Box::new(Node::Lit {
                            value: serde_json::json!(false),
                        }),
                        body: vec![Node::Return {
                            value: Box::new(Node::Lit {
                                value: serde_json::json!(1),
                            }),
                        }],
                    }],
                }],
            }],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            diags.iter().any(|d| d.message.contains("return")),
            "a return nested inside unless inside a parallel branch must be rejected"
        );
    }

    #[test]
    fn analyze_rejects_parallel_return_and_duplicate_branch_names() {
        use crate::ast::{Branch, DraftAst, Node};
        let ops = catalog();

        let bad = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![
                    Branch {
                        name: "dup".into(),
                        body: vec![Node::Return {
                            value: Box::new(Node::Lit {
                                value: serde_json::json!(1),
                            }),
                        }],
                    },
                    Branch {
                        name: "dup".into(),
                        body: vec![Node::Call {
                            op: "read".into(),
                            args: vec![],
                        }],
                    },
                ],
            }],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            diags.iter().any(|d| d.message.contains("return")),
            "a return inside a parallel branch is rejected"
        );
        assert!(
            diags.iter().any(|d| d.message.contains("duplicate")),
            "a duplicate branch name is rejected"
        );
    }

    #[test]
    fn analyze_rejects_empty_parallel_branch() {
        use crate::ast::{Branch, DraftAst, Node};
        let ops = catalog();

        // A `parallel` branch with an empty body binds nothing at runtime (silent no-op); the
        // analyzer must reject it so the planner's repair loop forces the model to fill it.
        let bad = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![Branch {
                    name: "spec_def".into(),
                    body: vec![],
                }],
            }],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            diags.iter().any(|d| d.message.contains("empty body")),
            "an empty parallel branch body is rejected"
        );
    }

    // ---- capability scopes (`with_tools` / L-11 acceptance #5: static analyzer check) ----

    /// A literal-op `call` naming a tool outside the enclosing `with_tools` allowlist is flagged at
    /// analysis time — the static echo of the runtime dispatch gate (which is still the enforcement
    /// authority; this is early feedback so a bad plan is rejected before it runs).
    #[test]
    fn call_outside_with_tools_scope_is_flagged_statically() {
        let ops = catalog();
        let bad = DraftAst {
            body: vec![Node::CapScope {
                tools: vec!["read".into()],
                body: vec![Node::Call {
                    op: "grep".into(),
                    args: vec![],
                }],
                bind: None,
            }],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("`grep`") && d.message.contains("with_tools")),
            "expected a with_tools scope diagnostic, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// A call to a tool that IS in the scope's allowlist analyzes cleanly.
    #[test]
    fn call_inside_with_tools_scope_is_not_flagged() {
        let ops = catalog();
        let good = DraftAst {
            body: vec![Node::CapScope {
                tools: vec!["read".into(), "grep".into()],
                body: vec![Node::Call {
                    op: "grep".into(),
                    args: vec![],
                }],
                bind: None,
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&good, &ops, &HashSet::new()).is_ok());
    }

    /// Nesting narrows: an inner `with_tools` cannot re-grant a tool the outer scope removed, and the
    /// analyzer must flag a call to it even though the inner scope's own literal list names it.
    #[test]
    fn nested_with_tools_cannot_statically_widen() {
        let ops = catalog();
        let bad = DraftAst {
            body: vec![Node::CapScope {
                tools: vec!["read".into()],
                body: vec![Node::CapScope {
                    // Inner scope asks for BOTH — but the outer only allowed `read`.
                    tools: vec!["read".into(), "grep".into()],
                    body: vec![Node::Call {
                        op: "grep".into(),
                        args: vec![],
                    }],
                    bind: None,
                }],
                bind: None,
            }],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            diags.iter().any(|d| d.message.contains("`grep`")),
            "inner scope must not re-grant what the outer removed: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// A call OUTSIDE any `with_tools` scope is never flagged by this pass (only ordinary catalog
    /// validation applies there) — proving the check is scope-local, not a blanket restriction.
    #[test]
    fn call_outside_any_scope_is_unaffected() {
        let ops = catalog();
        let good = DraftAst {
            body: vec![
                Node::CapScope {
                    tools: vec!["read".into()],
                    body: vec![Node::Call {
                        op: "read".into(),
                        args: vec![],
                    }],
                    bind: None,
                },
                Node::Call {
                    op: "grep".into(),
                    args: vec![],
                },
            ],
            ..Default::default()
        };
        assert!(analyze_flow(&good, &ops, &HashSet::new()).is_ok());
    }

    // ---- L-31: `with_tools`/`CapScope` rejected inside a `parallel`/`race` branch ----

    /// `parallel` branches run concurrently against ONE shared executor whose cap-scope stack is
    /// shared, mutable state — a `with_tools` opened inside a branch does not compose with a
    /// sibling branch running concurrently against the same stack. Rejected statically.
    #[test]
    fn cap_scope_inside_parallel_branch_is_rejected() {
        use crate::ast::Branch;
        let ops = catalog();
        let bad = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![Branch {
                    name: "a".into(),
                    body: vec![Node::CapScope {
                        tools: vec!["read".into()],
                        body: vec![Node::Call {
                            op: "read".into(),
                            args: vec![],
                        }],
                        bind: None,
                    }],
                }],
            }],
            ..Default::default()
        };
        let err = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("`with_tools`")
                    && d.message.contains("`parallel`/`race`")),
            "expected a cap-scope-in-parallel diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// Same hazard, `race` branches: first-wins concurrency runs every branch against the same
    /// shared executor until one succeeds, so a `with_tools` inside a branch is equally unsound.
    #[test]
    fn cap_scope_inside_race_branch_is_rejected() {
        use crate::ast::Branch;
        let ops = catalog();
        let bad = DraftAst {
            body: vec![Node::Race {
                timeout_ms: 100,
                branches: vec![Branch {
                    name: "a".into(),
                    body: vec![Node::CapScope {
                        tools: vec!["read".into()],
                        body: vec![Node::Call {
                            op: "read".into(),
                            args: vec![],
                        }],
                        bind: None,
                    }],
                }],
                bind: None,
            }],
            ..Default::default()
        };
        let err = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("`with_tools`")
                    && d.message.contains("`parallel`/`race`")),
            "expected a cap-scope-in-race diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn cleanup_scopes_inside_timeout_are_rejected() {
        let ops = catalog();
        for cleanup in [
            Node::CapScope {
                tools: vec!["read".into()],
                body: vec![Node::Call {
                    op: "read".into(),
                    args: vec![],
                }],
                bind: None,
            },
            Node::Scope {
                acquire: None,
                bind: None,
                body: vec![Node::Call {
                    op: "read".into(),
                    args: vec![],
                }],
                finally: vec![Node::Call {
                    op: "read".into(),
                    args: vec![],
                }],
            },
        ] {
            let bad = DraftAst {
                body: vec![Node::Timeout {
                    ms: 10,
                    body: vec![cleanup],
                    bind: None,
                }],
                ..Default::default()
            };
            let err = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
            assert!(
                err.iter().any(|d| d.message.contains("cleanup scope")
                    && d.message.contains("`timeout`")),
                "expected timeout cleanup-safety diagnostic, got: {:?}",
                err.iter().map(|d| &d.message).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn scope_with_finally_inside_race_branch_is_rejected() {
        use crate::ast::Branch;
        let ops = catalog();
        let bad = DraftAst {
            body: vec![Node::Race {
                timeout_ms: 100,
                branches: vec![Branch {
                    name: "cleanup".into(),
                    body: vec![Node::Scope {
                        acquire: None,
                        bind: None,
                        body: vec![Node::Call {
                            op: "read".into(),
                            args: vec![],
                        }],
                        finally: vec![Node::Call {
                            op: "read".into(),
                            args: vec![],
                        }],
                    }],
                }],
                bind: None,
            }],
            ..Default::default()
        };
        let err = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("cleanup scope") && d.message.contains("`race`")),
            "expected race cleanup-safety diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// A sequential `with_tools` — nested inside `when`, a non-concurrent control construct — is
    /// unaffected by this guard and still analyzes clean.
    #[test]
    fn sequential_with_tools_outside_parallel_is_unaffected() {
        let ops = catalog();
        let good = DraftAst {
            body: vec![Node::When {
                cond: Box::new(Node::Lit {
                    value: serde_json::json!(true),
                }),
                then: vec![Node::CapScope {
                    tools: vec!["read".into()],
                    body: vec![Node::Call {
                        op: "read".into(),
                        args: vec![],
                    }],
                    bind: None,
                }],
                otherwise: vec![],
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&good, &ops, &HashSet::new()).is_ok());
    }

    // ---- L-15: symbol definedness (F5) ----

    /// A `$var` no binder form anywhere in the flow (and no param / session symbol) can satisfy is
    /// a diagnostic naming the symbol — the typo class caught at analysis instead of runtime.
    /// Order-insensitivity is part of the contract: a use BEFORE its bind is NOT flagged (zero
    /// false positives; use-before-bind stays a precise runtime error).
    #[test]
    fn unbound_var_reference_is_a_diagnostic() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "real".into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!("v"),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Call {
                    op: "grep".into(),
                    args: vec![Node::Var {
                        name: "typo".into(),
                    }],
                },
            ],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("unbound symbol `$typo`")),
            "expected an unbound-symbol diagnostic naming $typo, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        assert!(
            !err.iter().any(|d| d.message.contains("$real")),
            "the bound symbol must not be flagged"
        );

        // Order-insensitive: a reference BEFORE its bind is fine, and a flow param satisfies too.
        let late = DraftAst {
            params: vec![crate::ast::Param {
                name: "arg".into(),
                ty: TypeRef::Any,
            }],
            body: vec![
                Node::Call {
                    op: "grep".into(),
                    args: vec![
                        Node::Var {
                            name: "late".into(),
                        },
                        Node::Var { name: "arg".into() },
                    ],
                },
                Node::Bind {
                    name: "late".into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!("x"),
                    }),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        assert!(
            analyze_flow(&late, &ops, &HashSet::new()).is_ok(),
            "use-before-bind and params must not be false positives"
        );
    }

    /// A `$var` satisfied only by the executing session's symbol set analyzes clean through the
    /// session-aware entry point — and is (correctly) unbound through the empty-set delegate.
    #[test]
    fn session_symbols_satisfy_var_references() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![Node::Call {
                op: "grep".into(),
                args: vec![Node::Var {
                    name: "seeded".into(),
                }],
            }],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(err
            .iter()
            .any(|d| d.message.contains("unbound symbol `$seeded`")));
        let seeded: HashSet<String> = ["seeded".to_string()].into_iter().collect();
        assert!(
            analyze_flow(&ast, &ops, &seeded).is_ok(),
            "a session-seeded symbol satisfies the reference"
        );
    }

    // ---- L-15: required-param presence (F6) ----

    /// An object-literal (or lone `obj`-template) call whose static keys miss a `required_params`
    /// entry is a diagnostic; a call with all required keys present passes. Keys are static even
    /// when values are dynamic — the check needs no jsonschema.
    #[test]
    fn object_call_missing_required_param_is_a_diagnostic() {
        let ops = TypedCatalog;
        let call = |args: Vec<Node>| DraftAst {
            body: vec![Node::Call {
                op: "write".into(),
                args,
            }],
            ..Default::default()
        };

        // `write` requires `path` + `content`; the lit object names only `path`.
        let err = lower(
            &call(vec![Node::Lit {
                value: serde_json::json!({"path": "p"}),
            }]),
            &ops,
            &HashSet::new(),
        )
        .unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("missing required parameter")
                    && d.message.contains("`content`")),
            "expected a missing-required-parameter diagnostic for `content`, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        // Same rule for a lone `obj` template — its keys are static even with dynamic values.
        let err = lower(
            &call(vec![Node::Obj {
                fields: [(
                    "content".to_string(),
                    Box::new(Node::Lit {
                        value: serde_json::json!("c"),
                    }),
                )]
                .into_iter()
                .collect(),
            }]),
            &ops,
            &HashSet::new(),
        )
        .unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("missing required parameter")
                    && d.message.contains("`path`")),
            "expected a missing-required-parameter diagnostic for `path`, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        // All required keys present → clean.
        assert!(lower(
            &call(vec![Node::Lit {
                value: serde_json::json!({"path": "p", "content": "c"}),
            }]),
            &ops,
            &HashSet::new(),
        )
        .is_ok());
    }

    /// F-002: the missing-required-parameter diagnostic names the param's expected TYPE and the op's
    /// full accepted shape (not just "add a key"), so a repairing model can fix the call instead of
    /// re-emitting the same broken node. Uses the typed `dbl(n: Number)` catalog so a type is present.
    #[test]
    fn missing_param_diag_names_the_expected_type_and_shape() {
        let ops = TypeCat;
        // `dbl` requires `n: Number`; a lone empty object omits `n`.
        let err = lower(
            &DraftAst {
                body: vec![Node::Call {
                    op: "dbl".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!({}),
                    }],
                }],
                ..Default::default()
            },
            &ops,
            &HashSet::new(),
        )
        .unwrap_err();
        let msg = err
            .iter()
            .map(|d| d.message.clone())
            .find(|m| m.contains("missing required parameter"))
            .expect("a missing-required-parameter diagnostic");
        assert!(msg.contains("`n`"), "names the missing param: {msg}");
        assert!(
            msg.contains("expected Number"),
            "names the expected type: {msg}"
        );
        assert!(
            msg.contains("accepts") && msg.contains("n (Number, required)"),
            "lists the accepted parameter shape: {msg}"
        );
    }

    // ---- L-16: expression positions the runtime rejects (F7) ----

    /// A `call` in argument position is a diagnostic with a bind-it-first hint — the runtime's
    /// `eval_arg` resolves arguments without dispatch and accepts only `lit`/`var`/`obj`/`list`.
    #[test]
    fn call_in_argument_position_is_a_diagnostic() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![Node::Call {
                op: "write".into(),
                args: vec![Node::Call {
                    op: "read".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!("f"),
                    }],
                }],
            }],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("not a valid call argument")
                    && d.message.contains("bind it to a symbol first")),
            "expected an invalid-argument-position diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    // ---- L-21: the remaining `eval_arg` positions (each source / jq input / parse value) ----

    /// A `call` as an `each` source is a diagnostic — the runtime resolves the source through
    /// `eval_arg` (no dispatch), so it would fail at runtime with "unsupported call argument".
    #[test]
    fn call_in_each_source_is_a_diagnostic() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![Node::Each {
                source: Box::new(Node::Call {
                    op: "read".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!("f"),
                    }],
                }),
                item: "x".into(),
                body: vec![Node::Call {
                    op: "read".into(),
                    args: vec![Node::Var { name: "x".into() }],
                }],
                collect: None,
                flat: false,
            }],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("not a valid `each` source")
                    && d.message.contains("bind it to a symbol first")
                    && d.message.contains("body[0].in")),
            "expected an invalid each-source diagnostic with its node path, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// A `call` as a `jq` input is a diagnostic (same `eval_arg` rule).
    #[test]
    fn call_in_jq_input_is_a_diagnostic() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![Node::Bind {
                name: "k".into(),
                value: Box::new(Node::Jq {
                    path: ".kind".into(),
                    optional: false,
                    input: Box::new(Node::Call {
                        op: "read".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!("f"),
                        }],
                    }),
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("not a valid `jq` input")),
            "expected an invalid jq-input diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// A `call` as a `parse` value is a diagnostic (same `eval_arg` rule).
    #[test]
    fn call_in_parse_value_is_a_diagnostic() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![Node::Bind {
                name: "n".into(),
                value: Box::new(Node::Parse {
                    value: Box::new(Node::Call {
                        op: "read".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!("f"),
                        }],
                    }),
                    as_type: "i64".into(),
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("not a valid `parse` value")),
            "expected an invalid parse-value diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// No false positives: the node kinds the runtime's `eval_arg` DOES accept in those positions —
    /// `var`, `lit`, and the pure `obj`/`list` templates — analyze clean.
    #[test]
    fn valid_non_call_expressions_in_eval_arg_positions_pass() {
        let ops = catalog();
        let bind_lit = |name: &str, v: serde_json::Value| Node::Bind {
            name: name.into(),
            value: Box::new(Node::Lit { value: v }),
            ty: None,
            effect: None,
        };
        let ast = DraftAst {
            body: vec![
                bind_lit("items", serde_json::json!(["a", "b"])),
                bind_lit("raw", serde_json::json!({"kind": "chat"})),
                // each over a bound symbol and over a list template
                Node::Each {
                    source: Box::new(Node::Var {
                        name: "items".into(),
                    }),
                    item: "x".into(),
                    body: vec![Node::Call {
                        op: "read".into(),
                        args: vec![Node::Var { name: "x".into() }],
                    }],
                    collect: None,
                    flat: false,
                },
                Node::Each {
                    source: Box::new(Node::List {
                        items: vec![Node::Var {
                            name: "items".into(),
                        }],
                    }),
                    item: "y".into(),
                    body: vec![Node::Call {
                        op: "read".into(),
                        args: vec![Node::Var { name: "y".into() }],
                    }],
                    collect: None,
                    flat: false,
                },
                // jq over a var, parse over a lit
                Node::Bind {
                    name: "k".into(),
                    value: Box::new(Node::Jq {
                        path: ".kind".into(),
                        optional: false,
                        input: Box::new(Node::Var { name: "raw".into() }),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Bind {
                    name: "n".into(),
                    value: Box::new(Node::Parse {
                        value: Box::new(Node::Lit {
                            value: serde_json::json!("42"),
                        }),
                        as_type: "i64".into(),
                    }),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        assert!(
            analyze_flow(&ast, &ops, &HashSet::new()).is_ok(),
            "valid eval_arg-position expressions must not be flagged: {:?}",
            analyze_flow(&ast, &ops, &HashSet::new())
                .err()
                .map(|ds| ds.iter().map(|d| d.message.clone()).collect::<Vec<_>>())
        );
    }

    /// L-27: a non-`call` `route` selector is resolved via `eval_arg` (lit/var/obj/list only) at
    /// runtime, so a `jq`/`fmt`/… selector must be rejected up front with a bind-it-first hint
    /// (mirrors the L-21 each/jq/parse guards).
    #[test]
    fn non_call_route_selector_is_a_diagnostic() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "raw".into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!({"intent": "refund"}),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Route {
                    // `jq` is not a `call`, so the runtime would `eval_arg` it and error.
                    selector: Box::new(Node::Jq {
                        path: ".intent".into(),
                        optional: false,
                        input: Box::new(Node::Var { name: "raw".into() }),
                    }),
                    cases: vec![crate::ast::RouteCase {
                        label: "refund".into(),
                        body: vec![Node::Call {
                            op: "read".into(),
                            args: vec![],
                        }],
                    }],
                    default: vec![],
                },
            ],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("not a valid `route` selector")
                    && d.message.contains("bind it to a symbol first")),
            "expected an invalid route-selector diagnostic with a bind-it-first hint, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// L-27: a `call` `route` selector (the primary `!model` form) is NOT flagged — the runtime
    /// dispatches it. No false positive.
    #[test]
    fn call_route_selector_passes() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![Node::Route {
                selector: Box::new(Node::Call {
                    op: "read".into(),
                    args: vec![],
                }),
                cases: vec![crate::ast::RouteCase {
                    label: "a".into(),
                    body: vec![Node::Call {
                        op: "read".into(),
                        args: vec![],
                    }],
                }],
                default: vec![],
            }],
            ..Default::default()
        };
        assert!(
            analyze_flow(&ast, &ops, &HashSet::new()).is_ok(),
            "a call route selector must not be flagged: {:?}",
            analyze_flow(&ast, &ops, &HashSet::new())
                .err()
                .map(|ds| ds.iter().map(|d| d.message.clone()).collect::<Vec<_>>())
        );
    }

    /// L-27: `verify`'s `expect` is resolved via `eval_arg` (lit/var/obj/list), no dispatch — a
    /// `fmt` (or any other non-eval_arg kind) must be rejected with a bind-it-first hint.
    #[test]
    fn non_eval_arg_verify_expect_is_a_diagnostic() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![Node::Verify {
                // `cmd` may be a `call` (it dispatches) — only `expect` is the eval_arg position.
                cmd: Box::new(Node::Call {
                    op: "read".into(),
                    args: vec![],
                }),
                expect: Box::new(Node::Fmt {
                    template: "{x}".into(),
                }),
                message: None,
            }],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("not a valid `verify` `expect`")
                    && d.message.contains("bind it to a symbol first")),
            "expected an invalid verify-expect diagnostic with a bind-it-first hint, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// L-27: an `expr` `formula` ident absent from its own `vars` map is a diagnostic — the runtime
    /// evaluates the formula against `vars`, an explicit scope, so this is checkable with no false
    /// positives.
    #[test]
    fn expr_formula_ident_absent_from_vars_is_a_diagnostic() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "n".into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!(5),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Bind {
                    name: "ok".into(),
                    value: Box::new(Node::Expr {
                        // `count` is declared; `threshold` is NOT in `vars`.
                        formula: "count > threshold".into(),
                        vars: [(
                            "count".to_string(),
                            Box::new(Node::Var { name: "n".into() }),
                        )]
                        .into_iter()
                        .collect(),
                    }),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("threshold") && d.message.contains("vars")),
            "expected a missing-vars-ident diagnostic naming `threshold`, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// L-27: a structurally malformed `expr` `formula` (a trailing operator) produces a parse
    /// diagnostic even when every referenced ident is declared.
    #[test]
    fn malformed_expr_formula_is_a_parse_diagnostic() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "n".into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!(5),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Bind {
                    name: "ok".into(),
                    value: Box::new(Node::Expr {
                        formula: "count >".into(),
                        vars: [(
                            "count".to_string(),
                            Box::new(Node::Var { name: "n".into() }),
                        )]
                        .into_iter()
                        .collect(),
                    }),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter().any(|d| d.message.contains("malformed")),
            "expected a malformed-formula parse diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    /// L-27: no false positives — a valid `expr` whose formula uses built-in functions, boolean and
    /// string literals, and only declared variables analyzes clean.
    #[test]
    fn valid_expr_formula_passes() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "n".into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!(5),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Bind {
                    name: "s".into(),
                    value: Box::new(Node::Lit {
                        value: serde_json::json!("ok"),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Bind {
                    name: "ok".into(),
                    value: Box::new(Node::Expr {
                        // round(…)/&&/'ok'/true are built-ins/literals, not variables; count/status
                        // are both declared in `vars`.
                        formula: "round(count * 2, 1) > 0 && status == 'ok' && true".into(),
                        vars: [
                            (
                                "count".to_string(),
                                Box::new(Node::Var { name: "n".into() }),
                            ),
                            (
                                "status".to_string(),
                                Box::new(Node::Var { name: "s".into() }),
                            ),
                        ]
                        .into_iter()
                        .collect(),
                    }),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        assert!(
            analyze_flow(&ast, &ops, &HashSet::new()).is_ok(),
            "a valid expr formula must not be flagged: {:?}",
            analyze_flow(&ast, &ops, &HashSet::new())
                .err()
                .map(|ds| ds.iter().map(|d| d.message.clone()).collect::<Vec<_>>())
        );
    }

    /// A condition kind the runtime's `eval_cond` rejects (anything but `call`/`lit`/`var`/`expr`)
    /// is a diagnostic; an `expr` condition passes.
    #[test]
    fn invalid_condition_kind_is_a_diagnostic() {
        let ops = catalog();
        let then_read = || {
            vec![Node::Call {
                op: "read".into(),
                args: vec![],
            }]
        };
        let bad = DraftAst {
            body: vec![Node::When {
                cond: Box::new(Node::Fmt {
                    template: "{x}".into(),
                }),
                then: then_read(),
                otherwise: vec![],
            }],
            ..Default::default()
        };
        let err = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("not a valid `when` condition")),
            "expected an invalid-condition diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        let good = DraftAst {
            body: vec![Node::When {
                cond: Box::new(Node::Expr {
                    formula: "1 == 1".into(),
                    vars: Default::default(),
                }),
                then: then_read(),
                otherwise: vec![],
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&good, &ops, &HashSet::new()).is_ok());
    }

    // ---- L-16: declared-name validity (F8) ----

    /// The confirmed round-trip corruption case: a bind or var named `a.b` (which reparses as
    /// field-access `jq(".b", $a)` through the text surface) is rejected outright.
    #[test]
    fn non_identifier_symbol_names_are_rejected() {
        let ops = catalog();
        let bad_bind = DraftAst {
            body: vec![Node::Bind {
                name: "a.b".into(),
                value: Box::new(Node::Lit {
                    value: serde_json::json!(1),
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        let err = analyze_flow(&bad_bind, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("invalid symbol name `$a.b`")),
            "a dotted declared name is rejected, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        let bad_var = DraftAst {
            body: vec![Node::Call {
                op: "grep".into(),
                args: vec![Node::Var { name: "a.b".into() }],
            }],
            ..Default::default()
        };
        let err = analyze_flow(&bad_var, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("invalid symbol reference `$a.b`")),
            "a dotted var reference is rejected, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        // Whitespace is just as invalid.
        let bad_space = DraftAst {
            body: vec![Node::Bind {
                name: "a b".into(),
                value: Box::new(Node::Lit {
                    value: serde_json::json!(1),
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&bad_space, &ops, &HashSet::new()).is_err());
    }

    // ---- L-16: loop bounds + empty bodies (F10) ----

    #[test]
    fn repeat_each_race_bounds_and_empty_bodies_are_validated() {
        let ops = catalog();
        let read = || Node::Call {
            op: "read".into(),
            args: vec![],
        };
        let wrap = |n: Node| DraftAst {
            body: vec![n],
            ..Default::default()
        };
        let has = |ast: &DraftAst, needle: &str| {
            analyze_flow(ast, &ops, &HashSet::new())
                .err()
                .is_some_and(|ds| ds.iter().any(|d| d.message.contains(needle)))
        };

        // `repeat` max: 0 can never run.
        assert!(has(
            &wrap(Node::Repeat {
                max: 0,
                until: None,
                body: vec![read()],
                collect: None,
            }),
            "`repeat` requires a non-zero `max`"
        ));
        // An absurd max is effectively unbounded.
        assert!(has(
            &wrap(Node::Repeat {
                max: 100_001,
                until: None,
                body: vec![read()],
                collect: None,
            }),
            "exceeds the analyzer bound"
        ));
        // Empty bodies run nothing and bind nothing (mirrors the empty-`parallel`-branch rule).
        assert!(has(
            &wrap(Node::Repeat {
                max: 2,
                until: None,
                body: vec![],
                collect: None,
            }),
            "`repeat` has an empty body"
        ));
        assert!(has(
            &wrap(Node::Each {
                source: Box::new(Node::Lit {
                    value: serde_json::json!([1]),
                }),
                item: "x".into(),
                body: vec![],
                collect: None,
                flat: false,
            }),
            "`each` has an empty body"
        ));
        assert!(has(
            &wrap(Node::Race {
                timeout_ms: 100,
                branches: vec![crate::ast::Branch {
                    name: "b".into(),
                    body: vec![],
                }],
                bind: None,
            }),
            "`race` branch `$b` has an empty body"
        ));

        // The well-formed version analyzes clean.
        assert!(analyze_flow(
            &wrap(Node::Repeat {
                max: 3,
                until: None,
                body: vec![read()],
                collect: None,
            }),
            &ops,
            &HashSet::new()
        )
        .is_ok());
    }

    // ---- L-16: `parallel` cross-branch bind disjointness (F15 analyzer half) ----

    #[test]
    fn parallel_cross_branch_binds_are_rejected() {
        use crate::ast::Branch;
        let ops = catalog();
        let bind_x = |v: i64| Node::Bind {
            name: "x".into(),
            value: Box::new(Node::Lit {
                value: serde_json::json!(v),
            }),
            ty: None,
            effect: None,
        };
        // Two branches each bind `$x` via INNER binds (not their branch names) — a store race.
        let bad = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![
                    Branch {
                        name: "a".into(),
                        body: vec![bind_x(1)],
                    },
                    Branch {
                        name: "b".into(),
                        body: vec![bind_x(2)],
                    },
                ],
            }],
            ..Default::default()
        };
        let err = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter().any(|d| d.message.contains("both bind `$x`")),
            "expected a cross-branch bind diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        // A branch name colliding with another branch's inner bind is the same race.
        let name_vs_inner = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![
                    Branch {
                        name: "a".into(),
                        body: vec![bind_x(1)],
                    },
                    Branch {
                        name: "x".into(),
                        body: vec![Node::Call {
                            op: "read".into(),
                            args: vec![],
                        }],
                    },
                ],
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&name_vs_inner, &ops, &HashSet::new())
            .unwrap_err()
            .iter()
            .any(|d| d.message.contains("both bind `$x`")));

        // Disjoint binds analyze clean.
        let good = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![
                    Branch {
                        name: "a".into(),
                        body: vec![bind_x(1)],
                    },
                    Branch {
                        name: "b".into(),
                        body: vec![Node::Bind {
                            name: "y".into(),
                            value: Box::new(Node::Lit {
                                value: serde_json::json!(2),
                            }),
                            ty: None,
                            effect: None,
                        }],
                    },
                ],
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&good, &ops, &HashSet::new()).is_ok());
    }

    #[test]
    fn race_cross_branch_binds_are_rejected() {
        use crate::ast::Branch;
        let ops = catalog();
        let bind = |name: &str, value: i64| Node::Bind {
            name: name.into(),
            value: Box::new(Node::Lit {
                value: serde_json::json!(value),
            }),
            ty: None,
            effect: None,
        };
        let bad = DraftAst {
            body: vec![Node::Race {
                timeout_ms: 100,
                branches: vec![
                    Branch {
                        name: "first".into(),
                        body: vec![bind("shared", 1)],
                    },
                    Branch {
                        name: "second".into(),
                        body: vec![bind("shared", 2)],
                    },
                ],
                bind: None,
            }],
            ..Default::default()
        };
        let err = analyze_flow(&bad, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter().any(|d| d.message.contains("`race` branches")
                && d.message.contains("both bind `$shared`")),
            "expected a race bind-disjointness diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        let good = DraftAst {
            body: vec![Node::Race {
                timeout_ms: 100,
                branches: vec![
                    Branch {
                        name: "first".into(),
                        body: vec![bind("left", 1)],
                    },
                    Branch {
                        name: "second".into(),
                        body: vec![bind("right", 2)],
                    },
                ],
                bind: Some("winner".into()),
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&good, &ops, &HashSet::new()).is_ok());
    }

    // ---- L-16: diagnostics carry node-path locators (F11) ----

    #[test]
    fn diagnostics_carry_node_paths() {
        let ops = catalog();
        let ast = DraftAst {
            body: vec![
                Node::Call {
                    op: "read".into(),
                    args: vec![],
                },
                Node::When {
                    cond: Box::new(Node::Lit {
                        value: serde_json::json!(true),
                    }),
                    then: vec![Node::Call {
                        op: "nope.op".into(),
                        args: vec![],
                    }],
                    otherwise: vec![],
                },
            ],
            ..Default::default()
        };
        let err = analyze_flow(&ast, &ops, &HashSet::new()).unwrap_err();
        assert!(
            err.iter().any(|d| d.message.contains("unknown operation")
                && d.message.contains("body[1].then[0]")),
            "expected the diagnostic to carry its node path, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    // ---- D-133: annotate_effects — per-node effect/risk annotation ----

    /// A catalog with a plain `read` (low-risk, idempotent) and a `charge_card` op (high-risk,
    /// non-idempotent) — enough to distinguish "the write node" from "the read node" in the
    /// annotate_effects tests below. `charge_card` also declares `Money` directly in its catalog
    /// signature (`semantic_effects`, D-138) — independent of whatever `effect:` tag (if any) a
    /// call site's enclosing `bind`/`memo` authors — so the catalog-declared-semantics test below
    /// doesn't depend on an authored tag at all.
    struct EffectCatalog;
    impl OpCatalog for EffectCatalog {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            match name {
                "read" => Some(OpSignature {
                    name: name.into(),
                    description: String::new(),
                    effects: vec![flux_spec::Effect::Read],
                    risk: Risk::Low,
                    idempotency: Idempotency::Idempotent,
                    required_params: Vec::new(),
                    optional_params: Vec::new(),
                    param_types: Default::default(),
                    semantic_effects: Vec::new(),
                }),
                "charge_card" => Some(OpSignature {
                    name: name.into(),
                    description: String::new(),
                    effects: vec![flux_spec::Effect::Network],
                    risk: Risk::High,
                    idempotency: Idempotency::NonIdempotent,
                    required_params: Vec::new(),
                    optional_params: Vec::new(),
                    param_types: Default::default(),
                    semantic_effects: vec![FlowEffect::Money],
                }),
                _ => None,
            }
        }
    }

    /// Failing-first for D-133: a flow with one `read` call and one `Money`-effect `charge_card`
    /// write (declared via the enclosing `bind`'s `effect: money` tag, the same annotation
    /// `lower_gathers_effects_and_named_args_are_validated` exercises for the flow-level union)
    /// must annotate EXACTLY the write node with `Money` + its `High` risk tier — the read node
    /// must not pick it up.
    #[test]
    fn annotate_effects_attributes_money_to_exactly_the_write_node() {
        let ops = EffectCatalog;
        let ast = DraftAst {
            body: vec![
                Node::Call {
                    op: "read".into(),
                    args: vec![],
                },
                Node::Bind {
                    name: "charge".into(),
                    value: Box::new(Node::Call {
                        op: "charge_card".into(),
                        args: vec![],
                    }),
                    ty: None,
                    effect: Some(FlowEffect::Money),
                },
            ],
            ..Default::default()
        };
        let annotated = annotate_effects(&ast, &ops);

        let read = annotated
            .iter()
            .find(|(path, _)| path == "body[0]")
            .unwrap_or_else(|| panic!("expected an entry for the read node, got: {annotated:?}"));
        let write = annotated
            .iter()
            .find(|(path, _)| path == "body[1].value")
            .unwrap_or_else(|| panic!("expected an entry for the write node, got: {annotated:?}"));

        let read_ann = read.1.as_ref().expect("`read` is a known op");
        let write_ann = write.1.as_ref().expect("`charge_card` is a known op");

        assert!(
            !read_ann.effects.contains(&FlowEffect::Money),
            "the read node must not carry `Money`, got {:?}",
            read_ann.effects
        );
        assert!(
            write_ann.effects.contains(&FlowEffect::Money),
            "the write node must carry `Money`, got {:?}",
            write_ann.effects
        );
        assert_eq!(
            write_ann.risk,
            Risk::High,
            "the write node must carry its op's risk tier"
        );
    }

    /// Failing-first for D-138: an op that declares `Money` directly in its CATALOG signature
    /// (`OpSignature::semantic_effects`) must annotate a PLAIN, untagged call node with `Money` —
    /// with no authored `effect: money` tag on an enclosing `bind`/`memo` at all. This is the
    /// catalog-declared counterpart to `annotate_effects_attributes_money_to_exactly_the_write_node`
    /// above (which relies entirely on an authored tag): before this story, `annotate_effects` only
    /// ever saw `Money` via `enclosing_effect`, so a bare `call(charge_card, {...})` with no bind/tag
    /// silently lost the semantic tier — exactly the erasure D-138 closes.
    #[test]
    fn annotate_effects_folds_catalog_declared_semantics_without_an_authored_tag() {
        let ops = EffectCatalog;
        let ast = DraftAst {
            body: vec![Node::Call {
                op: "charge_card".into(),
                args: vec![],
            }],
            ..Default::default()
        };

        let annotated = annotate_effects(&ast, &ops);
        assert_eq!(annotated.len(), 1);
        let (path, ann) = &annotated[0];
        assert_eq!(path, "body[0]");
        let ann = ann
            .as_ref()
            .expect("`charge_card` is a known op in EffectCatalog");
        assert!(
            ann.effects.contains(&FlowEffect::Money),
            "a plain, untagged call to a Money-declaring op must still annotate `Money`, got {:?}",
            ann.effects
        );
    }

    /// Failing-first for D-133: an unknown op must still get an entry at its node path (`None`),
    /// not be silently skipped — mirroring [`analyze_call`]'s own "unknown operation" diagnostic.
    #[test]
    fn annotate_effects_honestly_flags_unknown_ops_instead_of_skipping() {
        let ops = EffectCatalog;
        let ast = DraftAst {
            body: vec![Node::Call {
                op: "nope.op".into(),
                args: vec![],
            }],
            ..Default::default()
        };

        // The analyzer itself treats this op as unknown...
        assert!(analyze_call("nope.op", &ops).is_err());

        // ...and annotate_effects must agree: an entry is still present, honestly `None`.
        let annotated = annotate_effects(&ast, &ops);
        assert_eq!(
            annotated.len(),
            1,
            "the unknown-op call must still get an entry, not be skipped: {annotated:?}"
        );
        assert_eq!(annotated[0].0, "body[0]");
        assert!(
            annotated[0].1.is_none(),
            "an unknown op annotates as `None` (honest absence), not a guessed signature"
        );
    }
}
