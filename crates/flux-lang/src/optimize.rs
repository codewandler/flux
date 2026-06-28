//! The optimizer: lower a validated [`HirFlow`] into a [`PhysicalPlan`] — a schedule over the flow's
//! top-level body. v1 schedules the **top level**: a maximal run of consecutive, mutually-independent
//! **read-only** `bind`s (disjoint symbol reads/writes; their op carries no effect beyond `Read`)
//! batches into a [`Stage::Parallel`]; a side-effecting node becomes an [`Stage::ApprovalFence`]
//! ("don't speculate past a write"); every other node stays [`Stage::Sequential`] (the interpreter
//! runs its nested bodies). [`NodeId`] is the index into the top-level `body`;
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
//! **Soundness:** only consecutive *simple read-only binds* whose reads are the explicit `Var` names
//! in their call args are reordered into a batch, and program order is preserved across every batch
//! boundary — so no read-after-write / write-after-write hazard can cross a stage. A node whose reads
//! can't be determined precisely (anything but a read-only `bind`-of-`call`) is never batched. CSE
//! reuses a value only when the op is deterministic and the inputs are provably unchanged; a CSE source
//! is kept live so dead-step never removes it out from under an alias.

use std::collections::{BTreeMap, BTreeSet};

use flux_spec::{Effect, Idempotency};

use crate::ast::{HirFlow, Node, NodeId, PhysicalPlan, Stage, SymbolName};
use crate::opspec::OpCatalog;

/// Lower a [`HirFlow`] to a [`PhysicalPlan`] (see the module docs for the scheduling rules).
pub fn optimize(hir: &HirFlow, ops: &dyn OpCatalog) -> PhysicalPlan {
    let mut stages: Vec<Stage> = Vec::new();
    let mut batch = Batch::default();

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
            // The `source` is an earlier node, so its stage is already emitted; flush the batch so the
            // alias runs after it.
            batch.flush(&mut stages);
            stages.push(Stage::Alias {
                target: target.clone(),
                source: source.clone(),
            });
            continue;
        }
        match readonly_bind(node, ops) {
            // A read-only bind joins the current batch when independent of it, else starts a fresh one.
            Some((reads, write)) => {
                if !batch.independent(&reads, write.as_deref()) {
                    batch.flush(&mut stages);
                }
                batch.push(i, reads, write);
            }
            // Anything else flushes the batch, then runs in program order — fenced if side-effecting.
            None => {
                batch.flush(&mut stages);
                stages.push(if is_side_effecting(node, ops) {
                    Stage::ApprovalFence(NodeId(i as u32))
                } else {
                    Stage::Sequential(NodeId(i as u32))
                });
            }
        }
    }
    batch.flush(&mut stages);
    PhysicalPlan { stages }
}

/// The accumulating set of consecutive independent read-only binds.
#[derive(Default)]
struct Batch {
    ids: Vec<usize>,
    reads: BTreeSet<String>,
    writes: BTreeSet<String>,
}

impl Batch {
    /// A candidate node is independent of the batch when its written symbol is neither read nor
    /// written by the batch, and none of its reads hit a symbol the batch writes (no RAW/WAR/WAW).
    fn independent(&self, reads: &BTreeSet<String>, write: Option<&str>) -> bool {
        let write_ok = write
            .map(|w| !self.reads.contains(w) && !self.writes.contains(w))
            .unwrap_or(true);
        write_ok && reads.is_disjoint(&self.writes)
    }

    fn push(&mut self, i: usize, reads: BTreeSet<String>, write: Option<String>) {
        self.ids.push(i);
        self.reads.extend(reads);
        if let Some(w) = write {
            self.writes.insert(w);
        }
    }

    fn flush(&mut self, stages: &mut Vec<Stage>) {
        match self.ids.len() {
            0 => {}
            1 => stages.push(Stage::Sequential(NodeId(self.ids[0] as u32))),
            _ => stages.push(Stage::Parallel(
                self.ids.iter().map(|&i| NodeId(i as u32)).collect(),
            )),
        }
        self.ids.clear();
        self.reads.clear();
        self.writes.clear();
    }
}

/// If `node` is a `bind`/`memo` of a **read-only** `call`, return `(reads, written-symbol)` — its
/// reads are the explicit `Var` names in the call args. Only such nodes are eligible to batch.
fn readonly_bind(node: &Node, ops: &dyn OpCatalog) -> Option<(BTreeSet<String>, Option<String>)> {
    let (name, value) = match node {
        Node::Bind { name, value, .. } | Node::Memo { name, value, .. } => (name, value.as_ref()),
        _ => return None,
    };
    let Node::Call { op, args } = value else {
        return None;
    };
    if !is_readonly_op(op, ops) {
        return None;
    }
    let mut reads = BTreeSet::new();
    collect_var_reads(args, &mut reads);
    Some((reads, Some(name.0.clone())))
}

/// A known op all of whose effects are `Read` (or that declares none) — safe to run speculatively /
/// in parallel. An unknown op is conservatively treated as *not* read-only.
fn is_readonly_op(op: &str, ops: &dyn OpCatalog) -> bool {
    match ops.lookup(op) {
        Some(sig) => sig.effects.iter().all(|e| matches!(e, Effect::Read)),
        None => false,
    }
}

/// Whether the node calls an op carrying a non-`Read` (mutating / external) effect.
fn is_side_effecting(node: &Node, ops: &dyn OpCatalog) -> bool {
    let op = match node {
        Node::Bind { value, .. } | Node::Memo { value, .. } => match value.as_ref() {
            Node::Call { op, .. } => Some(op.as_str()),
            _ => None,
        },
        Node::Call { op, .. } => Some(op.as_str()),
        _ => None,
    };
    match op.and_then(|o| ops.lookup(o)) {
        Some(sig) => sig.effects.iter().any(|e| !matches!(e, Effect::Read)),
        None => false,
    }
}

/// Collect the symbols read anywhere in `nodes` — explicit `Var` names AND the `{name}`/`{{name}}`
/// interpolation tokens inside `lit` string args (a `lit` string is interpolated from bound symbols
/// at `eval_arg` time, so it reads them). Over-approximating is sound: extra reads only *suppress*
/// batching, never wrongly permit it.
fn collect_var_reads(nodes: &[Node], acc: &mut BTreeSet<String>) {
    for n in nodes {
        match n {
            Node::Var { name } => {
                acc.insert(name.0.clone());
            }
            Node::Lit { value } => collect_interp_reads(value, acc),
            Node::Call { args, .. } => collect_var_reads(args, acc),
            Node::Jq { input, .. } => collect_var_reads(std::slice::from_ref(input), acc),
            Node::Parse { value, .. } => collect_var_reads(std::slice::from_ref(value), acc),
            _ => {}
        }
    }
}

/// Collect interpolation tokens (`{name}` / `{{name}}`) from a literal value, recursing into arrays
/// and objects (the interpolator recurses the same way). Mirrors `runtime::interpolate_str`'s scan so
/// no interpolated read is missed.
fn collect_interp_reads(value: &serde_json::Value, acc: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(s) => collect_interp_reads_str(s, acc),
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_interp_reads(x, acc)),
        serde_json::Value::Object(m) => m.values().for_each(|x| collect_interp_reads(x, acc)),
        _ => {}
    }
}

/// Collect interpolation tokens (`{name}` / `{{name}}`) from a single string (a `lit` string or an
/// inline `fmt` template).
fn collect_interp_reads_str(s: &str, acc: &mut BTreeSet<String>) {
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
    crate::analyze::for_each_node(body, &mut |n| match n {
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
        // Every other node's reads are reached as nested `var`/`lit`/… nodes the visitor descends into.
        _ => {}
    });
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
}
