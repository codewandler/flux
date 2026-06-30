//! The analyzer. M1 validates the single-`call` grammar: the operation must be registered. Later
//! milestones add full name / type / effect / bounded-loop checking over the whole AST, lowering a
//! [`DraftAst`](crate::ast::DraftAst) into a typed [`HirFlow`](crate::ast::HirFlow).

use std::collections::{HashMap, HashSet};

use crate::ast::{DraftAst, FlowEffect, HirFlow, Node, TypeRef};
use crate::opspec::OpCatalog;

/// A single analyzer diagnostic, suitable for UI display or feeding back into the compile/repair
/// loop.
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

/// Validate that `op` names a registered operation (the M1 single-call grammar). Returns the
/// collected diagnostics on failure.
pub fn analyze_call(op: &str, ops: &dyn OpCatalog) -> Result<(), Vec<Diagnostic>> {
    if ops.lookup(op).is_some() {
        Ok(())
    } else {
        Err(vec![Diagnostic::new(format!("unknown operation: `{op}`"))])
    }
}

/// Validate every operation referenced anywhere in a flow against the catalog (the M2 whole-flow
/// check; richer type/effect checking comes later). Returns aggregated diagnostics on failure.
pub fn analyze_flow(ast: &DraftAst, ops: &dyn OpCatalog) -> Result<(), Vec<Diagnostic>> {
    let mut diags = Vec::new();
    for node in &ast.body {
        check_node(node, ops, &mut diags);
    }
    check_await_position(&ast.body, &mut diags);
    check_checkpoint_position(&ast.body, &mut diags);
    if diags.is_empty() {
        Ok(())
    } else {
        Err(diags)
    }
}

/// `await` may only appear as a **top-level** flow statement: it suspends the whole flow for cross-turn
/// resume (the interpreter records the top-level index and continues from the next statement on resume).
/// Nesting one inside a `when`/`repeat`/`each`/`parallel`/… body has no well-defined resume point in v1,
/// so it is rejected here (a clear analysis error rather than a runtime failure deep in `exec_body`).
fn check_await_position(body: &[Node], diags: &mut Vec<Diagnostic>) {
    for node in body {
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
            diags.push(Diagnostic::new(
                "`await` must be a top-level flow statement — it suspends the whole flow and cannot be nested (v1)",
            ));
        }
    }
}

/// `checkpoint` may only appear as a **top-level** flow statement: it is a durable resume cursor keyed
/// on a top-level index, so a `checkpoint` nested inside a `when`/`repeat`/`scope`/… body has no stable
/// resume point. Rejected here (mirrors [`check_await_position`]).
fn check_checkpoint_position(body: &[Node], diags: &mut Vec<Diagnostic>) {
    for node in body {
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
            diags.push(Diagnostic::new(
                "`checkpoint` must be a top-level flow statement — it is a durable resume cursor and cannot be nested (v1)",
            ));
        }
    }
}

/// Lower a `DraftAst` to a typed [`HirFlow`]: run the whole-flow analysis (op resolution, grammar,
/// bounded loops, call arity) and gather the flow's semantic effect set. Full type inference over
/// expressions is a later milestone; today the HIR carries the validated body plus the gathered
/// effects an authorizer/optimizer reasons over.
pub fn lower(ast: &DraftAst, ops: &dyn OpCatalog) -> Result<HirFlow, Vec<Diagnostic>> {
    analyze_flow(ast, ops)?;
    // Type-check call arguments against the ops' declared param types, tracking symbol types from
    // `param` decls + `bind` annotations. Lenient: only hard scalar/list mismatches are rejected.
    let mut scope: HashMap<String, TypeRef> = ast
        .params
        .iter()
        .map(|p| (p.name.0.clone(), p.ty.clone()))
        .collect();
    let mut diags = Vec::new();
    type_check_body(&ast.body, ops, &mut scope, &mut diags);
    if !diags.is_empty() {
        return Err(diags);
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
        Node::Lit { value } => match value {
            serde_json::Value::String(_) => TypeRef::String,
            serde_json::Value::Number(_) => TypeRef::Number,
            serde_json::Value::Bool(_) => TypeRef::Bool,
            serde_json::Value::Array(_) => TypeRef::List(Box::new(TypeRef::Any)),
            _ => TypeRef::Any,
        },
        Node::Var { name } => scope.get(&name.0).cloned().unwrap_or(TypeRef::Any),
        Node::Fmt { .. } => TypeRef::String,
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

/// Type-check a call's positional args against the op's declared param types (`required ++ optional`
/// order). A lone object literal is the whole named input — skipped. Only hard mismatches are reported.
fn check_call_types(
    op: &str,
    args: &[Node],
    ops: &dyn OpCatalog,
    scope: &HashMap<String, TypeRef>,
    diags: &mut Vec<Diagnostic>,
) {
    let Some(sig) = ops.lookup(op) else {
        return;
    };
    if matches!(args, [Node::Lit { value }] if value.is_object()) {
        return;
    }
    let order: Vec<&String> = sig
        .required_params
        .iter()
        .chain(sig.optional_params.iter())
        .collect();
    for (i, arg) in args.iter().enumerate() {
        if let Some(pname) = order.get(i) {
            if let Some(ptype) = sig.param_types.get(*pname) {
                let atype = infer_type(arg, scope);
                if types_conflict(&atype, ptype) {
                    diags.push(Diagnostic::new(format!(
                        "op `{op}` parameter `{pname}` expects {}, got {}",
                        ptype.label(),
                        atype.label()
                    )));
                }
            }
        }
        if let Node::Call {
            op: inner,
            args: iargs,
        } = arg
        {
            check_call_types(inner, iargs, ops, scope, diags);
        }
    }
}

/// Ordered type-check walk: track each symbol's type (a `bind`/`memo`'s `ty` annotation, else `Any`)
/// and check every `call`'s args. Control bodies are checked with a cloned scope (a branch-local bind
/// doesn't leak out — conservative).
fn type_check_body(
    body: &[Node],
    ops: &dyn OpCatalog,
    scope: &mut HashMap<String, TypeRef>,
    diags: &mut Vec<Diagnostic>,
) {
    for node in body {
        match node {
            Node::Bind {
                name, value, ty, ..
            }
            | Node::Memo {
                name, value, ty, ..
            } => {
                if let Node::Call { op, args } = value.as_ref() {
                    check_call_types(op, args, ops, scope, diags);
                }
                scope.insert(name.0.clone(), ty.clone().unwrap_or(TypeRef::Any));
            }
            Node::Call { op, args } => check_call_types(op, args, ops, scope, diags),
            Node::Return { value } => {
                if let Node::Call { op, args } = value.as_ref() {
                    check_call_types(op, args, ops, scope, diags);
                }
            }
            Node::Pipe { steps, .. } => {
                for s in steps {
                    if let Node::Call { op, args } = s {
                        check_call_types(op, args, ops, scope, diags);
                    }
                }
            }
            Node::When {
                then, otherwise, ..
            } => {
                type_check_body(then, ops, &mut scope.clone(), diags);
                type_check_body(otherwise, ops, &mut scope.clone(), diags);
            }
            Node::Unless { body, .. } => type_check_body(body, ops, &mut scope.clone(), diags),
            Node::Each { item, body, .. } => {
                let mut s = scope.clone();
                s.insert(item.0.clone(), TypeRef::Any);
                type_check_body(body, ops, &mut s, diags);
            }
            Node::Repeat { body, .. }
            | Node::Seq { body, .. }
            | Node::Retry { body, .. }
            | Node::Confirm { body, .. }
            | Node::Loop { body, .. }
            | Node::Throttle { body, .. }
            | Node::Debounce { body, .. } => type_check_body(body, ops, &mut scope.clone(), diags),
            Node::Try { body, handler, .. } => {
                type_check_body(body, ops, &mut scope.clone(), diags);
                type_check_body(handler, ops, &mut scope.clone(), diags);
            }
            Node::Parallel { branches } | Node::Race { branches, .. } => {
                for b in branches {
                    type_check_body(&b.body, ops, &mut scope.clone(), diags);
                }
            }
            Node::Timeout { body, .. } | Node::Budget { body, .. } => {
                type_check_body(body, ops, &mut scope.clone(), diags)
            }
            Node::Scope { body, finally, .. } => {
                type_check_body(body, ops, &mut scope.clone(), diags);
                type_check_body(finally, ops, &mut scope.clone(), diags);
            }
            Node::Saga { steps } => {
                for step in steps {
                    type_check_body(&step.body, ops, &mut scope.clone(), diags);
                    type_check_body(&step.undo, ops, &mut scope.clone(), diags);
                }
            }
            Node::Once { body, .. } => type_check_body(body, ops, &mut scope.clone(), diags),
            Node::Fallback { branches, .. } => {
                for b in branches {
                    type_check_body(&b.body, ops, &mut scope.clone(), diags);
                }
            }
            Node::Match { cases, default, .. } => {
                // The subject is a literal/bound symbol (enforced by `check_node`), so there's no call
                // to type-check here — only the case + default bodies.
                for c in cases {
                    type_check_body(&c.body, ops, &mut scope.clone(), diags);
                }
                type_check_body(default, ops, &mut scope.clone(), diags);
            }
            Node::Route {
                selector,
                cases,
                default,
            } => {
                if let Node::Call { op, args } = selector.as_ref() {
                    check_call_types(op, args, ops, scope, diags);
                }
                for c in cases {
                    type_check_body(&c.body, ops, &mut scope.clone(), diags);
                }
                type_check_body(default, ops, &mut scope.clone(), diags);
            }
            _ => {}
        }
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
            Node::Timeout { body, .. } | Node::Budget { body, .. } => for_each_node(body, f),
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

/// Recursively validate the operations in a node and its children.
fn check_node(node: &Node, ops: &dyn OpCatalog, diags: &mut Vec<Diagnostic>) {
    match node {
        Node::Call { op, args } => {
            match ops.lookup(op) {
                None => diags.push(Diagnostic::new(format!("unknown operation: `{op}`"))),
                Some(sig) => {
                    // Arity: positional args bind to `required ++ optional`; a lone object argument
                    // is the whole named input, so it is exempt (matches `runtime::map_args_to_input`).
                    let lone_object =
                        matches!(args.as_slice(), [Node::Lit { value }] if value.is_object());
                    let max = sig.required_params.len() + sig.optional_params.len();
                    // `max == 0` ops are skipped: a single arg may *resolve* to the whole-input object
                    // at runtime (exempt there), and the runtime still rejects a true 0-param overflow.
                    if !lone_object && max > 0 && args.len() > max {
                        diags.push(Diagnostic::new(format!(
                            "op `{op}` accepts at most {max} argument(s) but {} were supplied",
                            args.len()
                        )));
                    }
                    // Too few: a call with NO args can never bind a required param (zero args cannot
                    // be the lone whole-input object). Surface it at compile time so the planner
                    // re-plans, instead of failing at runtime mid-execution after side effects.
                    if args.is_empty() && !sig.required_params.is_empty() {
                        diags.push(Diagnostic::new(format!(
                            "op `{op}` requires argument(s) {} but none were supplied",
                            sig.required_params
                                .iter()
                                .map(|p| format!("`{p}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )));
                    }
                }
            }
            for a in args {
                check_node(a, ops, diags);
            }
        }
        Node::Bind { value, .. } => check_node(value, ops, diags),
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            check_node(cond, ops, diags);
            for n in then.iter().chain(otherwise) {
                check_node(n, ops, diags);
            }
        }
        Node::Repeat { until, body, .. } => {
            if let Some(u) = until {
                check_node(u, ops, diags);
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Each { source, body, .. } => {
            check_node(source, ops, diags);
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Assert { cond, .. } => check_node(cond, ops, diags),
        Node::Pipe { steps, .. } => {
            for s in steps {
                if !matches!(s, Node::Call { .. }) {
                    diags.push(Diagnostic::new("`pipe` steps must be `call` nodes"));
                }
                check_node(s, ops, diags);
            }
        }
        Node::Seq { body, .. } => {
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Memo { value, .. } => check_node(value, ops, diags),
        Node::Parallel { branches } => {
            let mut seen: HashSet<&str> = HashSet::new();
            for b in branches {
                if !seen.insert(b.name.0.as_str()) {
                    diags.push(Diagnostic::new(format!(
                        "duplicate `parallel` branch name `${}`",
                        b.name.0
                    )));
                }
                if body_contains_return(&b.body) {
                    diags.push(Diagnostic::new(
                        "`return` is not allowed inside a `parallel` branch",
                    ));
                }
                for n in &b.body {
                    check_node(n, ops, diags);
                }
            }
        }
        Node::Return { value } => check_node(value, ops, diags),
        Node::Retry { body, .. } => {
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Try { body, handler, .. } => {
            for n in body.iter().chain(handler) {
                check_node(n, ops, diags);
            }
        }
        Node::Confirm { body, .. } => {
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Race { branches, .. } => {
            let mut seen: HashSet<&str> = HashSet::new();
            for b in branches {
                if !seen.insert(b.name.0.as_str()) {
                    diags.push(Diagnostic::new(format!(
                        "duplicate `race` branch name `${}`",
                        b.name.0
                    )));
                }
                for n in &b.body {
                    check_node(n, ops, diags);
                }
            }
        }
        Node::Throttle {
            max, name, body, ..
        } => {
            if *max == 0 {
                diags.push(Diagnostic::new("`throttle` requires a non-zero `max`"));
            }
            if name.is_empty() {
                diags.push(Diagnostic::new("`throttle` requires a non-empty `name`"));
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Debounce { name, body, .. } => {
            if name.is_empty() {
                diags.push(Diagnostic::new("`debounce` requires a non-empty `name`"));
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Loop {
            until,
            body,
            for_ms,
            ..
        } => {
            if *for_ms == 0 {
                diags.push(Diagnostic::new(
                    "`loop` requires a non-zero `for_ms` (unbounded loops are rejected)",
                ));
            }
            if let Some(u) = until {
                check_node(u, ops, diags);
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Unless { cond, body } => {
            check_node(cond, ops, diags);
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Verify { cmd, expect, .. } => {
            check_node(cmd, ops, diags);
            check_node(expect, ops, diags);
        }
        Node::Expr { vars, .. } => {
            for v in vars.values() {
                check_node(v, ops, diags);
            }
        }
        Node::Fmt { .. } => {}
        Node::Jq { input, .. } => check_node(input, ops, diags),
        Node::Parse { value, as_type } => {
            const VALID: &[&str] = &["f64", "i64", "bool", "json", "string"];
            if !VALID.contains(&as_type.as_str()) {
                diags.push(Diagnostic::new(format!(
                    "`parse` as_type must be one of f64/i64/bool/json/string, got `{as_type}`"
                )));
            }
            check_node(value, ops, diags);
        }
        Node::Ctx { budget, .. } => {
            if matches!(budget, Some(0)) {
                diags.push(Diagnostic::new(
                    "`ctx` budget must be non-zero (a 0-char budget drops every member)",
                ));
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
                diags.push(Diagnostic::new(
                    "`match` subject must be a literal or a bound symbol (`$x`); bind a call result first, or use `route` to branch on an op",
                ));
            }
            check_node(subject, ops, diags);
            if cases.is_empty() {
                diags.push(Diagnostic::new("`match` requires at least one case"));
            }
            for c in cases {
                if !matches!(c.value, Node::Lit { .. } | Node::Var { .. }) {
                    diags.push(Diagnostic::new(
                        "`match` case values must be literals or bound symbols",
                    ));
                }
                check_node(&c.value, ops, diags);
                for n in &c.body {
                    check_node(n, ops, diags);
                }
            }
            for n in default {
                check_node(n, ops, diags);
            }
        }
        Node::Route {
            selector,
            cases,
            default,
        } => {
            check_node(selector, ops, diags);
            if cases.is_empty() {
                diags.push(Diagnostic::new("`route` requires at least one case"));
            }
            let mut seen: HashSet<&str> = HashSet::new();
            for c in cases {
                if c.label.is_empty() {
                    diags.push(Diagnostic::new("`route` case labels must be non-empty"));
                }
                if !seen.insert(c.label.as_str()) {
                    diags.push(Diagnostic::new(format!(
                        "duplicate `route` case label `{}`",
                        c.label
                    )));
                }
                for n in &c.body {
                    check_node(n, ops, diags);
                }
            }
            for n in default {
                check_node(n, ops, diags);
            }
        }
        Node::Fallback { branches, .. } => {
            if branches.is_empty() {
                diags.push(Diagnostic::new("`fallback` requires at least one branch"));
            }
            for b in branches {
                for n in &b.body {
                    check_node(n, ops, diags);
                }
            }
        }
        Node::Timeout { ms, body, .. } => {
            if *ms == 0 {
                diags.push(Diagnostic::new("`timeout` requires a non-zero `ms`"));
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Budget { limit, body, .. } => {
            if *limit == 0 {
                diags.push(Diagnostic::new("`budget` requires a non-zero `limit`"));
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Scope {
            acquire,
            bind,
            body,
            finally,
        } => {
            // `bind` names the *acquired resource*, so it only makes sense with an `acquire`.
            if bind.is_some() && acquire.is_none() {
                diags.push(Diagnostic::new(
                    "`scope` binds the acquired resource — `-> $name` requires an `acquire`",
                ));
            }
            if let Some(acq) = acquire {
                check_node(acq, ops, diags);
            }
            for n in body {
                check_node(n, ops, diags);
            }
            for n in finally {
                check_node(n, ops, diags);
            }
        }
        Node::Saga { steps } => {
            if steps.is_empty() {
                diags.push(Diagnostic::new("`saga` requires at least one step"));
            }
            for step in steps {
                for n in &step.body {
                    check_node(n, ops, diags);
                }
                for n in &step.undo {
                    check_node(n, ops, diags);
                }
            }
        }
        Node::Once { label, body, .. } => {
            // The label is the durable idempotency key, so it must be a fixed, auditable string.
            if label.trim().is_empty() {
                diags.push(Diagnostic::new(
                    "`once` requires a non-empty label (its durable idempotency key)",
                ));
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Checkpoint { label } => {
            if label.trim().is_empty() {
                diags.push(Diagnostic::new(
                    "`checkpoint` requires a non-empty label (its durable resume key)",
                ));
            }
        }
        Node::Obj { fields } => {
            for v in fields.values() {
                check_template_leaf(v, ops, diags);
            }
        }
        Node::List { items } => {
            for it in items {
                check_template_leaf(it, ops, diags);
            }
        }
        Node::Await { .. }
        | Node::Peek { .. }
        | Node::Var { .. }
        | Node::Lit { .. }
        | Node::Thing { .. }
        | Node::CtxAppend { .. } => {}
    }
}

/// A value template (`obj`/`list`) assembles a value with no dispatch, so each leaf must be a **pure
/// value node** (`var`/`lit`/`jq`/`expr`/`fmt`/`obj`/`list`). A `call` or control-flow leaf would
/// smuggle side effects into a notionally-pure template, so it is rejected — bind it to a symbol
/// first, then reference `$name`. Recurses so nested templates are checked too.
fn check_template_leaf(node: &Node, ops: &dyn OpCatalog, diags: &mut Vec<Diagnostic>) {
    if !matches!(
        node,
        Node::Var { .. }
            | Node::Lit { .. }
            | Node::Jq { .. }
            | Node::Expr { .. }
            | Node::Fmt { .. }
            | Node::Obj { .. }
            | Node::List { .. }
    ) {
        diags.push(Diagnostic::new(
            "a value template (`obj`/`list`) may only contain pure value leaves \
             (`var`/`lit`/`jq`/`expr`/`fmt`/`obj`/`list`); bind a call or control-flow result to a \
             symbol first, then reference it as `$name`",
        ));
    }
    // Recurse regardless, so a nested issue (e.g. an unknown op inside the offending call) also surfaces.
    check_node(node, ops, diags);
}

/// Whether any statement in `body` is (or reaches, through nested control flow) a `return`. Used to
/// reject `return` inside a `parallel` branch, where which branch's return should win is ambiguous.
/// A nested `parallel`'s own branches are validated separately, so their returns don't count here.
fn body_contains_return(body: &[Node]) -> bool {
    body.iter().any(node_contains_return)
}

fn node_contains_return(node: &Node) -> bool {
    match node {
        Node::Return { .. } => true,
        Node::When {
            then, otherwise, ..
        } => body_contains_return(then) || body_contains_return(otherwise),
        Node::Repeat { body, .. } => body_contains_return(body),
        Node::Each { body, .. } => body_contains_return(body),
        Node::Seq { body, .. } => body_contains_return(body),
        Node::Retry { body, .. } => body_contains_return(body),
        Node::Try { body, handler, .. } => {
            body_contains_return(body) || body_contains_return(handler)
        }
        Node::Confirm { body, .. } => body_contains_return(body),
        Node::Loop { body, .. } => body_contains_return(body),
        Node::Race { branches, .. } => branches.iter().any(|b| body_contains_return(&b.body)),
        Node::Throttle { body, .. } => body_contains_return(body),
        Node::Debounce { body, .. } => body_contains_return(body),
        Node::Unless { body, .. } => body_contains_return(body),
        Node::Match { cases, default, .. } => {
            cases.iter().any(|c| body_contains_return(&c.body)) || body_contains_return(default)
        }
        Node::Route { cases, default, .. } => {
            cases.iter().any(|c| body_contains_return(&c.body)) || body_contains_return(default)
        }
        Node::Fallback { branches, .. } => branches.iter().any(|b| body_contains_return(&b.body)),
        Node::Timeout { body, .. } | Node::Budget { body, .. } => body_contains_return(body),
        Node::Scope { body, finally, .. } => {
            body_contains_return(body) || body_contains_return(finally)
        }
        Node::Saga { steps } => steps
            .iter()
            .any(|s| body_contains_return(&s.body) || body_contains_return(&s.undo)),
        Node::Once { body, .. } => body_contains_return(body),
        Node::Expr { .. } | Node::Fmt { .. } | Node::Jq { .. } => false,
        _ => false,
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
    fn lower_gathers_effects_and_arity_is_checked() {
        use crate::ast::{Node, TypeRef};
        let ops = TypedCatalog;

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
                    args: vec![
                        Node::Lit {
                            value: serde_json::json!("p"),
                        },
                        Node::Lit {
                            value: serde_json::json!("c"),
                        },
                    ],
                },
            ],
            ..Default::default()
        };
        let hir: HirFlow = lower(&ast, &ops).unwrap();
        // Read (from `read`) + WriteFile (from `write`) + Model (declared) — deduped.
        assert!(hir.effects.contains(&FlowEffect::Read));
        assert!(hir.effects.contains(&FlowEffect::WriteFile));
        assert!(hir.effects.contains(&FlowEffect::Model));
        let _ = TypeRef::Any;

        // Too many positional args for `read` (1 param) is rejected at analysis.
        let over = DraftAst {
            body: vec![Node::Call {
                op: "read".into(),
                args: vec![
                    Node::Lit {
                        value: serde_json::json!("a"),
                    },
                    Node::Lit {
                        value: serde_json::json!("b"),
                    },
                ],
            }],
            ..Default::default()
        };
        let err = lower(&over, &ops).unwrap_err();
        assert!(err.iter().any(|d| d.message.contains("at most 1 argument")));
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
        let err = lower(&empty, &ops).unwrap_err();
        assert!(
            err.iter()
                .any(|d| d.message.contains("requires argument(s)") && d.message.contains("`path`")),
            "expected a missing-required-arg diagnostic, got: {:?}",
            err.iter().map(|d| &d.message).collect::<Vec<_>>()
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
            lower(ast, &ops)
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
        assert!(lower(&ok, &ops).is_ok());
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
        let err = analyze_flow(&nested, &ops).unwrap_err();
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
        assert!(analyze_flow(&top, &ops).is_ok());
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
        let err = analyze_flow(&nested, &ops).unwrap_err();
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
        assert!(analyze_flow(&top, &ops).is_ok());

        let empty = DraftAst {
            body: vec![Node::Checkpoint { label: "".into() }],
            ..Default::default()
        };
        assert!(analyze_flow(&empty, &ops).is_err());
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
            })
        }
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
        let err = lower(&bad, &TypeCat).unwrap_err();
        assert!(
            err.iter().any(|d| d.message.contains("expects Number")),
            "expected a Number-mismatch diagnostic, got {err:?}"
        );

        // A number literal passes.
        let good = call_dbl(Node::Lit {
            value: serde_json::json!(5),
        });
        assert!(lower(&good, &TypeCat).is_ok());

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
            lower(&lenient, &TypeCat).is_ok(),
            "an Any-typed var argument must pass leniently"
        );

        // A param declared `Number` is tracked: passing it where a Number is wanted is fine; a
        // String-typed param would conflict.
        let _ = TypeRef::Number;
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
        assert!(analyze_flow(&good, &ops).is_ok());

        let bad = DraftAst {
            body: vec![Node::Return {
                value: Box::new(Node::Call {
                    op: "nope.op".into(),
                    args: vec![],
                }),
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&bad, &ops).is_err());
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
        let diags = analyze_flow(&bad, &ops).unwrap_err();
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
        let diags = analyze_flow(&bad, &ops).unwrap_err();
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
        let diags = analyze_flow(&bad_ast, &ops).unwrap_err();
        assert!(
            diags.iter().any(|d| d.message.contains("value template")),
            "expected a template-leaf diagnostic, got: {diags:?}"
        );

        // The pure version (field-access + literal + nested list) analyzes clean.
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
            body: vec![Node::Bind {
                name: "r".into(),
                value: Box::new(good),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&good_ast, &ops).is_ok());
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
        let diags = analyze_flow(&bad, &ops).unwrap_err();
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
        let diags = analyze_flow(&bad, &ops).unwrap_err();
        assert!(
            diags.iter().any(|d| d.message.contains("return")),
            "a return inside a parallel branch is rejected"
        );
        assert!(
            diags.iter().any(|d| d.message.contains("duplicate")),
            "a duplicate branch name is rejected"
        );
    }
}
