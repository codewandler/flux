//! The optimizer: lower a validated [`HirFlow`] into a [`PhysicalPlan`] — a schedule over the flow's
//! top-level body. The scheduler (L-53) builds a **whole-node symbol dependency graph**: every
//! statement is summarized by walking its entire subtree — nested blocks, `when` conditions,
//! object/list templates, call arguments — into (reads, writes, class). Whole-node **read-only**
//! statements (every reachable op registered with only `Read` effects, no approval/durability
//! construct) are placed into dependency **levels** between fences; each multi-node level becomes a
//! [`Stage::Parallel`]. An order floor keeps the emitted stage sequence in exact program order, so
//! the replayed trace is identical to sequential execution while independent reads still overlap.
//!
//! **Fences.** A write/network/process effect, an **unknown op** (unknown effects are treated as
//! the most dangerous effects), or an approval/durability construct (`confirm`, `await`,
//! `checkpoint`, `once`, `saga`, `thing`) anywhere in a statement's subtree makes the whole
//! statement a [`Stage::ApprovalFence`]: nothing is scheduled across it in either direction, so
//! approval ordering and policy behavior match sequential execution exactly. A statement carrying
//! a nested `return` stays [`Stage::Sequential`] ([`crate::runtime::execute_plan`] forbids `return`
//! inside a parallel stage). [`NodeId`] is the index into the top-level `body`;
//! [`crate::runtime::execute_plan`] runs the result.
//!
//! Two eliminations run alongside the scheduler:
//! - **Dead-step elimination** drops a read-only `bind` whose symbol is read nowhere in the flow (it
//!   has no observable effect), except the final result statement.
//! - **Common-subexpression elimination (CSE)** dedupes an identical read-only, *deterministic*
//!   (`Idempotent`) call: the second `$b = op(args)` is dispatched once as `$a`'s value and reused via a
//!   [`Stage::Alias`] — provided no intervening node rebinds a symbol the call reads and no side effect
//!   runs between them. Non-idempotent reads (a clock/random) are never deduped.
//!
//! **Soundness:** a node enters a level only when its whole-subtree read/write sets (gathered by
//! the analyzer's exhaustive visitor, so no node kind can hide a read or a binder) have no
//! RAW/WAW/WAR hazard against any co-scheduled level, and the order floor forbids placing a node
//! at a level below its predecessor's — so the emitted stage sequence is always a refinement of
//! program order and no hazard can cross a stage. Over-approximated read/write sets only
//! *suppress* parallelism, never wrongly permit it. CSE reuses a value only when the op is
//! deterministic and the inputs are provably unchanged; a CSE source is kept live so dead-step
//! never removes it out from under an alias.

use std::collections::{BTreeMap, BTreeSet};

use flux_spec::{Effect, Idempotency};

use crate::ast::{HirFlow, Node, NodeId, PhysicalPlan, Stage, SymbolName};
use crate::opspec::OpCatalog;

/// Lower a [`HirFlow`] to a [`PhysicalPlan`] (see the module docs for the scheduling rules).
pub fn optimize(hir: &HirFlow, ops: &dyn OpCatalog) -> PhysicalPlan {
    let mut stages: Vec<Stage> = Vec::new();
    let mut batch = Window::default();

    // Common-subexpression elimination: a read-only, deterministic op called twice with the same args
    // (no intervening invalidation) is dispatched once; the duplicate becomes a `Stage::Alias` that
    // copies the earlier result. Computed up front over the whole body, applied per node below.
    let aliases = cse_aliases(&hir.body, ops);

    // Dead-step elimination: a read-only `bind` whose symbol is read nowhere in the flow has no
    // observable effect, so drop it from the schedule. The flow's final top-level statement is never
    // dropped — its value is the flow result (`execute_plan` returns the last stage's text). Single
    // pass (not iterated to a fixpoint): dropping a step may free a *prior* step, which a later pass
    // would catch; keeping it is sound, just less optimal.
    let mut live = BTreeSet::new();
    collect_reads_deep(&hir.body, &mut live);
    // A CSE *source* is read by its alias, so it must survive dead-step elimination even if no real
    // node reads it.
    for (_, source) in aliases.values() {
        live.insert(source.0.clone());
    }
    let last = hir.body.len().saturating_sub(1);
    let is_dead = |i: usize, node: &Node| i != last && is_dead_readonly_bind(node, ops, &live);

    for (i, node) in hir.body.iter().enumerate() {
        if is_dead(i, node) {
            continue;
        }
        if let Some((target, source)) = aliases.get(&i) {
            // The `source` is an earlier node, so its stage is already emitted; flush the window so
            // the alias runs after it.
            batch.flush(&mut stages);
            stages.push(Stage::Alias {
                target: target.clone(),
                source: source.clone(),
            });
            continue;
        }
        let summary = summarize(node, ops);
        match summary.class {
            // A hard fence: emit everything scheduled so far, then the fence in program order.
            // Nothing is ever scheduled across it, in either direction.
            NodeClass::Fenced => {
                batch.flush(&mut stages);
                stages.push(Stage::ApprovalFence(NodeId(i as u32)));
            }
            // Value-safe but not parallelizable (a nested `return`): run alone in program order.
            NodeClass::Barrier => {
                batch.flush(&mut stages);
                stages.push(Stage::Sequential(NodeId(i as u32)));
            }
            // Whole-node read-only work: place at the earliest hazard-free level at or after the
            // previous node's level (the order floor keeps the emitted schedule in program order).
            NodeClass::ReadOnly => batch.place(i, &summary),
        }
    }
    batch.flush(&mut stages);
    PhysicalPlan { stages }
}

/// How a whole node — including every nested body, template, condition, and call argument — may
/// be scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeClass {
    /// Every op reachable in the subtree is registered and read-only, and no approval or
    /// durability construct appears: safe to run concurrently with other `ReadOnly` nodes.
    ReadOnly,
    /// Value-safe but not parallelizable — the subtree carries a `return`, which
    /// [`crate::runtime::execute_plan`] forbids inside a parallel stage. Runs alone, in order.
    Barrier,
    /// A hard fence: a write/network/process effect, an **unknown** op (unknown effects are
    /// treated as the most dangerous effects), or an approval/durability construct (`confirm`,
    /// `await`, `checkpoint`, `once`, `saga`, `thing`). Nothing is scheduled across a fence in
    /// either direction, so approval ordering and policy behavior match sequential execution.
    Fenced,
}

/// The whole-node scheduling summary: every symbol the subtree reads, every symbol it binds, and
/// its [`NodeClass`]. Over-approximating reads/writes is sound — it only suppresses parallelism.
struct NodeSummary {
    reads: BTreeSet<String>,
    writes: BTreeSet<String>,
    class: NodeClass,
}

/// Summarize a node for the scheduler by walking its whole subtree with the analyzer's exhaustive
/// visitor (the same one `collect_var_reads` uses, so no node kind can hide a read, a binder, or
/// an effect from the hazard analysis).
fn summarize(node: &Node, ops: &dyn OpCatalog) -> NodeSummary {
    let nodes = std::slice::from_ref(node);
    let mut reads = BTreeSet::new();
    collect_var_reads(nodes, &mut reads);
    let mut writes = BTreeSet::new();
    let mut fenced = false;
    let mut barrier = false;
    crate::analyze::for_each_node(nodes, &mut |n| {
        collect_binder_writes(n, &mut writes);
        match n {
            Node::Call { op, .. } => {
                if !is_readonly_op(op, ops) {
                    fenced = true;
                }
            }
            Node::Confirm { .. }
            | Node::Await { .. }
            | Node::Checkpoint { .. }
            | Node::Once { .. }
            | Node::Saga { .. }
            | Node::Thing { .. } => fenced = true,
            Node::Return { .. } => barrier = true,
            _ => {}
        }
    });
    let class = if fenced {
        NodeClass::Fenced
    } else if barrier {
        NodeClass::Barrier
    } else {
        NodeClass::ReadOnly
    };
    NodeSummary {
        reads,
        writes,
        class,
    }
}

/// Record the symbol(s) a single node BINDS — every binder position in the grammar. Paired with
/// the exhaustive visitor this yields the write set of a whole subtree.
fn collect_binder_writes(n: &Node, acc: &mut BTreeSet<String>) {
    let mut w = |s: &SymbolName| {
        acc.insert(s.0.clone());
    };
    match n {
        Node::Bind { name, .. } | Node::Memo { name, .. } | Node::Ctx { name, .. } => w(name),
        Node::CtxAppend { ctx, .. } => w(ctx),
        Node::Each { item, collect, .. } => {
            w(item);
            if let Some(c) = collect {
                w(c);
            }
        }
        Node::Repeat {
            collect: Some(c), ..
        } => w(c),
        Node::Await {
            binding: Some(b), ..
        } => w(b),
        Node::Scope { bind, .. }
        | Node::Timeout { bind, .. }
        | Node::Budget { bind, .. }
        | Node::CapScope { bind, .. }
        | Node::Retry { bind, .. }
        | Node::Seq { bind, .. }
        | Node::Once { bind, .. }
        | Node::Race { bind, .. }
        | Node::Fallback { bind, .. }
        | Node::Loop { bind, .. }
        | Node::Pipe { bind, .. } => {
            if let Some(b) = bind {
                w(b);
            }
        }
        Node::Try { catch: Some(c), .. } => w(c),
        Node::Parallel { branches } => {
            for b in branches {
                w(&b.name);
            }
        }
        _ => {}
    }
}

/// The window scheduler: dependency levels between two fences. A `ReadOnly` node is placed at the
/// earliest level with no RAW/WAW/WAR hazard against any existing level, **but never before the
/// level of the previously placed node** (the order floor). That floor keeps the emitted stage
/// sequence — and therefore the replayed trace — in exact program order, while still letting a
/// later independent read run concurrently with an earlier dependent one (they share a level).
#[derive(Default)]
struct Window {
    levels: Vec<Level>,
    floor: usize,
}

#[derive(Default)]
struct Level {
    ids: Vec<usize>,
    reads: BTreeSet<String>,
    writes: BTreeSet<String>,
}

impl Window {
    fn place(&mut self, i: usize, s: &NodeSummary) {
        let mut lvl = self.floor;
        for (li, level) in self.levels.iter().enumerate() {
            let raw = !s.reads.is_disjoint(&level.writes);
            let waw = !s.writes.is_disjoint(&level.writes);
            let war = !s.writes.is_disjoint(&level.reads);
            if raw || waw || war {
                lvl = lvl.max(li + 1);
            }
        }
        if lvl >= self.levels.len() {
            self.levels.resize_with(lvl + 1, Level::default);
        }
        let level = &mut self.levels[lvl];
        level.ids.push(i);
        level.reads.extend(s.reads.iter().cloned());
        level.writes.extend(s.writes.iter().cloned());
        self.floor = lvl;
    }

    fn flush(&mut self, stages: &mut Vec<Stage>) {
        for level in self.levels.drain(..) {
            match level.ids.len() {
                0 => {}
                1 => stages.push(Stage::Sequential(NodeId(level.ids[0] as u32))),
                _ => stages.push(Stage::Parallel(
                    level.ids.iter().map(|&i| NodeId(i as u32)).collect(),
                )),
            }
        }
        self.floor = 0;
    }
}

/// A known op all of whose effects are `Read` (or that declares none) — safe to run speculatively /
/// in parallel. An unknown op is conservatively treated as *not* read-only.
fn is_readonly_op(op: &str, ops: &dyn OpCatalog) -> bool {
    match ops.lookup(op) {
        Some(sig) => sig.effects.iter().all(|e| matches!(e, Effect::Read)),
        None => false,
    }
}

/// Collect the symbols read anywhere in `nodes` — the explicit `Var` names, the `{name}`/`{{name}}`
/// interpolation tokens inside `lit`/`fmt` strings, and the members a `ctx`/`ctx_append` pack pulls
/// in — recursing through EVERY nested sub-expression, including the `obj`/`list`/`expr` templates a
/// named-arg call carries (the canonical `grep({path:$dir})` form). Routed through the analyzer's
/// exhaustive [`for_each_node`] visitor (as [`collect_reads_deep`] is) so a new node kind can never
/// silently hide a read site — the earlier hand-rolled match dropped `obj`/`list`/`fmt`/`expr` under
/// a `_ => {}`, making a reader invisible to the batch/CSE hazard check (L-26). This drives both
/// `Batch::independent` (parallelization) and `cse_aliases` (invalidation); over-approximating is
/// sound — extra reads only *suppress* batching/aliasing, never wrongly permit them. `pub(crate)`:
/// also the whole-flow (unnarrowed) read source for [`crate::context_slice::required_symbols_in_flow`]
/// (KF4/L-56), which needs the identical soundness guarantee but has no interest in narrowing.
pub(crate) fn collect_var_reads(nodes: &[Node], acc: &mut BTreeSet<String>) {
    crate::analyze::for_each_node(nodes, &mut |n| collect_leaf_read(n, acc));
}

/// Record the symbol at a single leaf read site the exhaustive [`for_each_node`] visitor reaches — a
/// `var`/`peek` reference, the `{name}` tokens in a `lit` string or `fmt` template, and the members
/// a `ctx`/`ctx_append` pack names. The shared leaf logic of [`collect_var_reads`] (call-arg reads)
/// and [`collect_reads_deep`] (whole-flow liveness); every other node's reads are reached as the
/// nested leaves the visitor descends into.
fn collect_leaf_read(n: &Node, acc: &mut BTreeSet<String>) {
    match n {
        Node::Var { name } | Node::Peek { name } => {
            acc.insert(name.0.clone());
        }
        Node::Lit { value } => collect_interp_reads(value, acc),
        Node::Fmt { template } => collect_interp_reads_str(template, acc),
        Node::Ctx {
            include, exclude, ..
        } => {
            for s in include.iter().chain(exclude.iter()) {
                acc.insert(s.0.clone());
            }
        }
        Node::CtxAppend { ctx, add } => {
            acc.insert(ctx.0.clone());
            for s in add {
                acc.insert(s.0.clone());
            }
        }
        _ => {}
    }
}

/// Collect interpolation tokens (`{name}` / `{{name}}`) from a literal value, recursing into arrays
/// and objects (the interpolator recurses the same way). Mirrors `runtime::interpolate_str`'s scan so
/// no interpolated read is missed.
pub(crate) fn collect_interp_reads(value: &serde_json::Value, acc: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(s) => collect_interp_reads_str(s, acc),
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_interp_reads(x, acc)),
        serde_json::Value::Object(m) => m.values().for_each(|x| collect_interp_reads(x, acc)),
        _ => {}
    }
}

/// Collect interpolation tokens (`{name}` / `{{name}}`) from a single string (a `lit` string or an
/// inline `fmt` template).
pub(crate) fn collect_interp_reads_str(s: &str, acc: &mut BTreeSet<String>) {
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        let at = &rest[open..];
        let (o, c): (&str, &str) = if at.starts_with("{{") {
            ("{{", "}}")
        } else {
            ("{", "}")
        };
        let inner = &at[o.len()..];
        let Some(close) = inner.find(c) else { break };
        let name = inner[..close].trim();
        if !name.is_empty() {
            acc.insert(name.to_string());
        }
        rest = &inner[close + c.len()..];
    }
}

/// The **global liveness read-set**: every symbol read anywhere in `body`, recursing through all
/// sub-expressions and every nested statement body (via the analyzer's exhaustive [`for_each_node`]
/// visitor, so a new node kind can't silently hide a read site). Collects the leaf read sites — a
/// `var`/`peek` reference, the `{name}` tokens inside a `lit` string or `fmt` template, and the
/// members a `ctx`/`ctx_append` pack pulls in. Powers dead-step elimination: a read-only bind whose
/// symbol is absent here is provably unused.
fn collect_reads_deep(body: &[Node], acc: &mut BTreeSet<String>) {
    crate::analyze::for_each_node(body, &mut |n| collect_leaf_read(n, acc));
}

/// Whether `node` is a read-only `bind`-of-`call` whose bound symbol is read nowhere in the flow — a
/// dead step the optimizer drops. Restricted to plain `bind` (a `memo` may be read in a later turn,
/// which a single flow's body cannot see) and to read-only ops (dropping must remove no side effect).
fn is_dead_readonly_bind(node: &Node, ops: &dyn OpCatalog, live: &BTreeSet<String>) -> bool {
    let Node::Bind { name, value, .. } = node else {
        return false;
    };
    let Node::Call { op, .. } = value.as_ref() else {
        return false;
    };
    is_readonly_op(op, ops) && !live.contains(&name.0)
}

/// A read-only op whose result is a deterministic function of its inputs (`Idempotent`) — safe for CSE
/// to **reuse** a prior result. Stronger than [`is_readonly_op`]: a read-only but *non*-idempotent op
/// (a clock/random read) must NOT be deduplicated, because its two calls can legitimately differ.
fn is_deterministic_readonly(op: &str, ops: &dyn OpCatalog) -> bool {
    match ops.lookup(op) {
        Some(sig) => {
            sig.effects.iter().all(|e| matches!(e, Effect::Read))
                && matches!(sig.idempotency, Idempotency::Idempotent)
        }
        None => false,
    }
}

/// Common-subexpression elimination over the top-level body: return, for each top-level node index that
/// duplicates an earlier identical read-only deterministic call, the pair `(target, source)` — its own
/// bound symbol and the earlier symbol whose already-computed value it can reuse.
///
/// **Soundness.** Two `$a = op(args)` / `$b = op(args)` may share a value only when `op` is read-only +
/// deterministic and the inputs are unchanged between them. Conservatively: any node that is not a
/// deterministic read-only `bind`-of-`call` *clears* the table (a write / side-effecting op / control
/// flow could change shared state or a read symbol); and a cached call is dropped as soon as a later
/// node rebinds a symbol it reads (its input changed). Keys are the canonical JSON of the `call`
/// (`Node: Serialize`), so identical op+args collide and differing args do not.
fn cse_aliases(body: &[Node], ops: &dyn OpCatalog) -> BTreeMap<usize, (SymbolName, SymbolName)> {
    let mut aliases = BTreeMap::new();
    // canonical `call` JSON -> (first symbol bound to that call, the symbols the call reads)
    let mut seen: BTreeMap<String, (SymbolName, BTreeSet<String>)> = BTreeMap::new();
    for (i, node) in body.iter().enumerate() {
        let Node::Bind { name, value, .. } = node else {
            seen.clear();
            continue;
        };
        let Node::Call { op, args } = value.as_ref() else {
            // A pure non-call bind (expr/fmt/jq/…) still rebinds `name`; reset conservatively.
            seen.clear();
            continue;
        };
        if !is_deterministic_readonly(op, ops) {
            // Side-effecting or non-deterministic: its result can't be reused, and a side effect may
            // invalidate other cached reads — reset.
            seen.clear();
            continue;
        }
        let key = serde_json::to_string(value.as_ref()).unwrap_or_default();
        if let Some((source, _)) = seen.get(&key) {
            aliases.insert(i, (name.clone(), source.clone()));
        } else {
            let mut reads = BTreeSet::new();
            collect_var_reads(args, &mut reads);
            seen.insert(key, (name.clone(), reads));
        }
        // This node (re)binds `name`, so any cached call that reads `name` is now stale for later nodes.
        seen.retain(|_, (_, reads)| !reads.contains(&name.0));
    }
    aliases
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opspec::OpSignature;

    /// `read` is read-only + deterministic; `pure` has no effects; `write` mutates; `now` is read-only
    /// but **non-deterministic** (`NonIdempotent`) — read-only enough to batch, but NOT safe to CSE.
    struct Cat;
    impl OpCatalog for Cat {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            let mk = |effects: Vec<Effect>, idempotency: Idempotency| OpSignature {
                name: name.into(),
                description: String::new(),
                effects,
                risk: flux_spec::Risk::Low,
                idempotency,
                required_params: vec!["x".into()],
                optional_params: Vec::new(),
                param_types: Default::default(),
            };
            match name {
                "read" => Some(mk(vec![Effect::Read], Idempotency::Idempotent)),
                "pure" => Some(mk(Vec::new(), Idempotency::Idempotent)),
                "write" => Some(mk(vec![Effect::Write], Idempotency::NonIdempotent)),
                "now" => Some(mk(vec![Effect::Read], Idempotency::NonIdempotent)),
                _ => None,
            }
        }
    }

    fn bind(name: &str, op: &str, args: Vec<Node>) -> Node {
        Node::Bind {
            name: name.into(),
            value: Box::new(Node::Call {
                op: op.into(),
                args,
            }),
            ty: None,
            effect: None,
        }
    }
    fn var(n: &str) -> Node {
        Node::Var { name: n.into() }
    }
    fn lit(s: &str) -> Node {
        Node::Lit {
            value: serde_json::json!(s),
        }
    }

    fn plan(body: Vec<Node>) -> Vec<Stage> {
        let hir = HirFlow {
            body,
            ..Default::default()
        };
        optimize(&hir, &Cat).stages
    }

    // ---- L-53: whole-flow dependency scheduler --------------------------------------------

    #[test]
    fn nested_readonly_work_batches_into_parallel_stages() {
        // Read-only calls hidden inside a `when` (condition + child block), an object template,
        // and a plain bind are all independent → ONE parallel stage, not three sequential nodes.
        let template = Node::Bind {
            name: "t".into(),
            value: Box::new(Node::Obj {
                fields: [
                    (
                        "x".to_string(),
                        Box::new(Node::Call {
                            op: "read".into(),
                            args: vec![lit("f1")],
                        }),
                    ),
                    (
                        "y".to_string(),
                        Box::new(Node::Call {
                            op: "read".into(),
                            args: vec![lit("f2")],
                        }),
                    ),
                ]
                .into_iter()
                .collect(),
            }),
            ty: None,
            effect: None,
        };
        let when = Node::When {
            cond: Box::new(Node::Call {
                op: "read".into(),
                args: vec![lit("flag")],
            }),
            then: vec![bind("w", "read", vec![lit("f3")])],
            otherwise: vec![],
        };
        let stages = plan(vec![
            template,
            when,
            bind("c", "read", vec![lit("f4")]),
            bind("r", "read", vec![lit("{{t}}{{w}}{{c}}")]),
        ]);
        assert_eq!(
            stages,
            vec![
                Stage::Parallel(vec![NodeId(0), NodeId(1), NodeId(2)]),
                Stage::Sequential(NodeId(3)),
            ]
        );
    }

    #[test]
    fn pipelined_levels_preserve_program_order() {
        // $a = read f; $c = read $a; $b = read g — b is independent of the a→c chain, but program
        // order must be preserved in the emitted schedule, so b joins c's LEVEL (they run
        // concurrently) rather than jumping ahead of it.
        let stages = plan(vec![
            bind("a", "read", vec![lit("f")]),
            bind("c", "read", vec![var("a")]),
            bind("b", "read", vec![lit("g")]),
            bind("r", "read", vec![lit("{{c}}{{b}}")]),
        ]);
        assert_eq!(
            stages,
            vec![
                Stage::Sequential(NodeId(0)),
                Stage::Parallel(vec![NodeId(1), NodeId(2)]),
                Stage::Sequential(NodeId(3)),
            ]
        );
    }

    #[test]
    fn unknown_op_is_a_hard_fence() {
        // `mystery` is not in the catalog: unknown effects → hard fence, no speculation across.
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            Node::Call {
                op: "mystery".into(),
                args: vec![],
            },
            bind("b", "read", vec![lit("{{a}}")]),
        ]);
        assert_eq!(
            stages,
            vec![
                Stage::Sequential(NodeId(0)),
                Stage::ApprovalFence(NodeId(1)),
                Stage::Sequential(NodeId(2)),
            ]
        );
    }

    #[test]
    fn a_nested_write_fences_the_whole_node() {
        // A write buried inside a `when` body makes the WHOLE node a fence — the reads on either
        // side never batch across it.
        let when_with_write = Node::When {
            cond: Box::new(lit("true")),
            then: vec![Node::Call {
                op: "write".into(),
                args: vec![lit("out")],
            }],
            otherwise: vec![],
        };
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            when_with_write,
            bind("b", "read", vec![lit("{{a}}")]),
        ]);
        assert_eq!(
            stages,
            vec![
                Stage::Sequential(NodeId(0)),
                Stage::ApprovalFence(NodeId(1)),
                Stage::Sequential(NodeId(2)),
            ]
        );
    }

    #[test]
    fn approval_construct_is_a_hard_fence() {
        // `confirm` is the approval gate itself — always a fence, even with a read-only body.
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            Node::Confirm {
                message: "ok?".into(),
                risk: None,
                body: vec![bind("k", "read", vec![lit("y")])],
            },
            bind("b", "read", vec![lit("{{a}}{{k}}")]),
        ]);
        assert_eq!(
            stages,
            vec![
                Stage::Sequential(NodeId(0)),
                Stage::ApprovalFence(NodeId(1)),
                Stage::Sequential(NodeId(2)),
            ]
        );
    }

    #[test]
    fn a_nested_return_never_enters_a_parallel_stage() {
        // `execute_plan` hard-errors on `return` inside a parallel stage; a when-with-return is
        // scheduled sequentially even though it is read-only.
        let when_with_return = Node::When {
            cond: Box::new(Node::Call {
                op: "read".into(),
                args: vec![lit("flag")],
            }),
            then: vec![Node::Return {
                value: Box::new(lit("early")),
            }],
            otherwise: vec![],
        };
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            when_with_return,
            bind("r", "read", vec![lit("{{a}}")]),
        ]);
        assert!(
            !stages
                .iter()
                .any(|s| matches!(s, Stage::Parallel(ids) if ids.contains(&NodeId(1)))),
            "a return-carrying node must not be parallelized: {stages:?}"
        );
    }

    #[test]
    fn independent_reads_batch_into_one_parallel_stage() {
        // $a = read "x"; $b = read "y" — independent reads → one Parallel stage. `$r` consumes both
        // (so they are live) and is the flow result.
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            bind("b", "read", vec![lit("y")]),
            bind("r", "read", vec![lit("{{a}}{{b}}")]),
        ]);
        assert_eq!(
            stages,
            vec![
                Stage::Parallel(vec![NodeId(0), NodeId(1)]),
                Stage::Sequential(NodeId(2)),
            ]
        );
    }

    #[test]
    fn a_dependency_splits_the_batch() {
        // $a = read "x"; $b = read $a  — b reads a's write → sequential after a.
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            bind("b", "read", vec![var("a")]),
        ]);
        assert_eq!(
            stages,
            vec![Stage::Sequential(NodeId(0)), Stage::Sequential(NodeId(1))]
        );
    }

    #[test]
    fn a_write_fences_and_breaks_the_batch() {
        // $a = read "x"; $b = write "y"; $c = read "{{a}}"  → [seq a] [fence b] [seq c]. `$c` reads
        // `$a` (keeping it live) and is the result.
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            bind("b", "write", vec![lit("y")]),
            bind("c", "read", vec![lit("{{a}}")]),
        ]);
        assert_eq!(
            stages,
            vec![
                Stage::Sequential(NodeId(0)),
                Stage::ApprovalFence(NodeId(1)),
                Stage::Sequential(NodeId(2)),
            ]
        );
    }

    #[test]
    fn interpolation_reads_in_a_lit_arg_break_the_batch() {
        // $a = read "config"; $b = read "{{a}}/sub" — b reads `a` via interpolation, so the two must
        // NOT parallelize (the soundness bug: missing the implicit interpolation read).
        let stages = plan(vec![
            bind("a", "read", vec![lit("config")]),
            bind(
                "b",
                "read",
                vec![Node::Lit {
                    value: serde_json::json!("{{a}}/sub"),
                }],
            ),
        ]);
        assert_eq!(
            stages,
            vec![Stage::Sequential(NodeId(0)), Stage::Sequential(NodeId(1))]
        );
    }

    #[test]
    fn write_after_write_to_the_same_symbol_is_not_parallelized() {
        // two pure binds to the SAME symbol must not parallelize (WAW hazard). The second reads `$a`
        // (keeping the first live) and is the result.
        let stages = plan(vec![
            bind("a", "pure", vec![lit("x")]),
            bind("a", "pure", vec![lit("{{a}}")]),
        ]);
        assert_eq!(
            stages,
            vec![Stage::Sequential(NodeId(0)), Stage::Sequential(NodeId(1))]
        );
    }

    #[test]
    fn a_dead_read_bind_is_dropped() {
        // $dead = read "x" (never used); $used = read "y"; $r = read $used (the result).
        // The dead read is eliminated; the live nodes keep their original indices.
        let stages = plan(vec![
            bind("dead", "read", vec![lit("x")]),
            bind("used", "read", vec![lit("y")]),
            bind("r", "read", vec![var("used")]),
        ]);
        assert_eq!(
            stages,
            vec![Stage::Sequential(NodeId(1)), Stage::Sequential(NodeId(2))],
            "node 0 (dead) is gone; nodes 1 and 2 (live, dependent) stay sequential"
        );
    }

    #[test]
    fn a_read_used_only_by_interpolation_is_kept() {
        // $cfg = read "x"; $b = read "{{cfg}}/p" — cfg is read via interpolation, so it is NOT dead.
        let stages = plan(vec![
            bind("cfg", "read", vec![lit("x")]),
            bind(
                "b",
                "read",
                vec![Node::Lit {
                    value: serde_json::json!("{{cfg}}/p"),
                }],
            ),
        ]);
        assert_eq!(
            stages,
            vec![Stage::Sequential(NodeId(0)), Stage::Sequential(NodeId(1))],
            "cfg is live via interpolation and is not eliminated"
        );
    }

    #[test]
    fn an_unused_write_is_never_dropped() {
        // $w = write "x" (unused); $r = read "y" (result). A write is a side effect, never eliminated.
        let stages = plan(vec![
            bind("w", "write", vec![lit("x")]),
            bind("r", "read", vec![lit("y")]),
        ]);
        assert_eq!(
            stages,
            vec![
                Stage::ApprovalFence(NodeId(0)),
                Stage::Sequential(NodeId(1))
            ],
            "only read-only binds are eligible for elimination; the write stays (fenced)"
        );
    }

    #[test]
    fn the_final_statement_is_never_dropped_even_if_unread() {
        // a single unread read is the flow's result, so it must survive.
        let stages = plan(vec![bind("a", "read", vec![lit("x")])]);
        assert_eq!(stages, vec![Stage::Sequential(NodeId(0))]);
    }

    fn has_alias(stages: &[Stage]) -> bool {
        stages.iter().any(|s| matches!(s, Stage::Alias { .. }))
    }

    #[test]
    fn duplicate_read_only_call_is_aliased_and_dispatched_once() {
        // $a = read "x"; $b = read "x" (identical, read-only, deterministic); $r consumes both.
        // The second read collapses into an Alias of the first — one dispatch, $b reuses $a's value.
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            bind("b", "read", vec![lit("x")]),
            bind("r", "read", vec![lit("{{a}}{{b}}")]),
        ]);
        assert_eq!(
            stages,
            vec![
                Stage::Sequential(NodeId(0)),
                Stage::Alias {
                    target: SymbolName("b".into()),
                    source: SymbolName("a".into()),
                },
                Stage::Sequential(NodeId(2)),
            ],
        );
    }

    #[test]
    fn a_nondeterministic_read_is_never_aliased() {
        // `now` is read-only but NonIdempotent — two calls may differ, so CSE must NOT dedupe them.
        let stages = plan(vec![
            bind("a", "now", vec![lit("x")]),
            bind("b", "now", vec![lit("x")]),
            bind("r", "read", vec![lit("{{a}}{{b}}")]),
        ]);
        assert!(
            !has_alias(&stages),
            "non-idempotent reads are not CSE'd: {stages:?}"
        );
    }

    #[test]
    fn an_intervening_rebind_of_a_read_symbol_blocks_cse() {
        // $a = read "{{cfg}}"; $cfg = read "c2" (rebinds cfg); $b = read "{{cfg}}".
        // $a and $b are textually identical calls, but $b reads a DIFFERENT cfg, so no alias.
        let stages = plan(vec![
            bind("cfg", "read", vec![lit("c")]),
            bind("a", "read", vec![lit("{{cfg}}")]),
            bind("cfg", "read", vec![lit("c2")]),
            bind("b", "read", vec![lit("{{cfg}}")]),
            bind("r", "read", vec![lit("{{a}}{{b}}")]),
        ]);
        assert!(
            !has_alias(&stages),
            "an intervening rebind of a read symbol blocks CSE: {stages:?}"
        );
    }

    #[test]
    fn a_side_effecting_op_between_clears_cse() {
        // $a = read "x"; $w = write "y" (side effect); $b = read "x". The write could change what the
        // read observes, so the cached value is dropped — no alias.
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            bind("w", "write", vec![lit("y")]),
            bind("b", "read", vec![lit("x")]),
            bind("r", "read", vec![lit("{{a}}{{b}}")]),
        ]);
        assert!(
            !has_alias(&stages),
            "a side-effecting op between identical reads blocks CSE: {stages:?}"
        );
    }

    #[test]
    fn distinct_args_are_not_aliased() {
        // read "x" and read "y" are different calls — never deduped.
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            bind("b", "read", vec![lit("y")]),
            bind("r", "read", vec![lit("{{a}}{{b}}")]),
        ]);
        assert!(
            !has_alias(&stages),
            "distinct args are not CSE'd: {stages:?}"
        );
    }

    /// Build a single-object call arg `{ k: v, … }` from `(key, node)` pairs — the canonical
    /// named-arg form (L-09), the shape the old hand-rolled collector was blind to.
    fn obj(fields: Vec<(&str, Node)>) -> Node {
        Node::Obj {
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), Box::new(v)))
                .collect(),
        }
    }

    #[test]
    fn batch_split_sees_object_arg_reads() {
        // $dir = read "x"; $hits = read({pattern:"TODO", path:$dir}) — the reader's `$dir` lives
        // inside a named-arg OBJECT. The optimizer must see that read (RAW hazard on `$dir`) and
        // NOT place both binds in one `Stage::Parallel`, or `$hits` would resolve `$dir` unbound.
        let stages = plan(vec![
            bind("dir", "read", vec![lit("x")]),
            bind(
                "hits",
                "read",
                vec![obj(vec![("pattern", lit("TODO")), ("path", var("dir"))])],
            ),
        ]);
        assert_eq!(
            stages,
            vec![Stage::Sequential(NodeId(0)), Stage::Sequential(NodeId(1))],
            "an object-arg read of $dir must split the batch, not parallelize with its writer: {stages:?}"
        );
    }

    #[test]
    fn cse_invalidated_by_object_arg_rebind() {
        // $dir = read "c"; $a = read({path:$dir}); $dir = read "c2" (rebinds dir);
        // $b = read({path:$dir}); $r consumes both. $a and $b are textually identical calls, but
        // the intervening rebind means $b reads a DIFFERENT $dir — the object-arg read must
        // invalidate the cache so $b is NOT aliased to $a's stale value.
        let stages = plan(vec![
            bind("dir", "read", vec![lit("c")]),
            bind("a", "read", vec![obj(vec![("path", var("dir"))])]),
            bind("dir", "read", vec![lit("c2")]),
            bind("b", "read", vec![obj(vec![("path", var("dir"))])]),
            bind("r", "read", vec![lit("{{a}}{{b}}")]),
        ]);
        assert!(
            !has_alias(&stages),
            "an intervening rebind of a symbol read inside an object arg must block CSE: {stages:?}"
        );
    }

    #[test]
    fn collect_var_reads_sees_every_var_in_nested_args() {
        // Read-set soundness invariant: EVERY `Var` name reachable anywhere in a call's args —
        // however deeply nested through obj/list/jq/expr/parse/fmt/call — is collected. A
        // deterministic property check over pseudo-randomly generated nested arg trees: build a
        // tree seeded with a known set of `$xN` reads at random depths, then assert the collector
        // returns a superset of them. If any descent arm is dropped, a planted name goes missing.
        //
        // Simple reproducible LCG — no external rand crate in this lib's dev-deps.
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        // Build a node that embeds `planted` (a var read) somewhere, wrapping it in a random pure
        // container up to `depth` levels deep. Returns the wrapped node.
        fn wrap(planted: Node, depth: u32, next: &mut impl FnMut() -> u64) -> Node {
            if depth == 0 {
                return planted;
            }
            let inner = wrap(planted, depth - 1, next);
            match next() % 6 {
                0 => Node::Obj {
                    fields: [("f".to_string(), Box::new(inner))].into_iter().collect(),
                },
                1 => Node::List { items: vec![inner] },
                2 => Node::Jq {
                    path: ".p".into(),
                    optional: false,
                    input: Box::new(inner),
                },
                3 => Node::Parse {
                    value: Box::new(inner),
                    as_type: "json".into(),
                },
                4 => Node::Expr {
                    formula: "k + 1".into(),
                    vars: [("k".to_string(), Box::new(inner))].into_iter().collect(),
                },
                _ => Node::Call {
                    op: "read".into(),
                    args: vec![inner],
                },
            }
        }

        for trial in 0..200 {
            let names: Vec<String> = (0..(1 + next() % 5))
                .map(|i| format!("x{trial}_{i}"))
                .collect();
            let args: Vec<Node> = names
                .iter()
                .map(|n| {
                    let depth = (next() % 5) as u32;
                    wrap(var(n), depth, &mut next)
                })
                .collect();
            let mut reads = BTreeSet::new();
            collect_var_reads(&args, &mut reads);
            for n in &names {
                assert!(
                    reads.contains(n),
                    "planted read `${n}` was dropped from the collected set {reads:?} (args: {args:?})"
                );
            }
        }
    }
}
