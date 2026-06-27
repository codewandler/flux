//! The optimizer: lower a validated [`HirFlow`] into a [`PhysicalPlan`] — a schedule over the flow's
//! top-level body. v1 schedules the **top level**: a maximal run of consecutive, mutually-independent
//! **read-only** `bind`s (disjoint symbol reads/writes; their op carries no effect beyond `Read`)
//! batches into a [`Stage::Parallel`]; a side-effecting node becomes an [`Stage::ApprovalFence`]
//! ("don't speculate past a write"); every other node stays [`Stage::Sequential`] (the interpreter
//! runs its nested bodies). [`NodeId`] is the index into the top-level `body`;
//! [`crate::runtime::execute_plan`] runs the result.
//!
//! **Soundness:** only consecutive *simple read-only binds* whose reads are the explicit `Var` names
//! in their call args are reordered into a batch, and program order is preserved across every batch
//! boundary — so no read-after-write / write-after-write hazard can cross a stage. A node whose reads
//! can't be determined precisely (anything but a read-only `bind`-of-`call`) is never batched.

use std::collections::BTreeSet;

use flux_spec::Effect;

use crate::ast::{HirFlow, Node, NodeId, PhysicalPlan, Stage};
use crate::opspec::OpCatalog;

/// Lower a [`HirFlow`] to a [`PhysicalPlan`] (see the module docs for the scheduling rules).
pub fn optimize(hir: &HirFlow, ops: &dyn OpCatalog) -> PhysicalPlan {
    let mut stages: Vec<Stage> = Vec::new();
    let mut batch = Batch::default();

    for (i, node) in hir.body.iter().enumerate() {
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

/// Collect the `Var` names read anywhere in `nodes` (call args, nested calls, jq/parse inputs).
fn collect_var_reads(nodes: &[Node], acc: &mut BTreeSet<String>) {
    for n in nodes {
        match n {
            Node::Var { name } => {
                acc.insert(name.0.clone());
            }
            Node::Call { args, .. } => collect_var_reads(args, acc),
            Node::Jq { input, .. } => collect_var_reads(std::slice::from_ref(input), acc),
            Node::Parse { value, .. } => collect_var_reads(std::slice::from_ref(value), acc),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opspec::OpSignature;

    /// `read` is read-only (effect `Read`); `write` mutates (effect `Write`); `pure` has no effects.
    struct Cat;
    impl OpCatalog for Cat {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            let mk = |effects: Vec<Effect>| OpSignature {
                name: name.into(),
                description: String::new(),
                effects,
                risk: flux_spec::Risk::Low,
                idempotency: flux_spec::Idempotency::Idempotent,
                required_params: vec!["x".into()],
                optional_params: Vec::new(),
                param_types: Default::default(),
            };
            match name {
                "read" => Some(mk(vec![Effect::Read])),
                "pure" => Some(mk(Vec::new())),
                "write" => Some(mk(vec![Effect::Write])),
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
        // $a = read "x"; $b = read "y"  — independent reads → one Parallel stage.
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            bind("b", "read", vec![lit("y")]),
        ]);
        assert_eq!(stages, vec![Stage::Parallel(vec![NodeId(0), NodeId(1)])]);
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
        // $a = read "x"; $b = write "y"; $c = read "z"  → [seq a] [fence b] [seq c].
        let stages = plan(vec![
            bind("a", "read", vec![lit("x")]),
            bind("b", "write", vec![lit("y")]),
            bind("c", "read", vec![lit("z")]),
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
    fn write_after_write_to_the_same_symbol_is_not_parallelized() {
        // two pure binds to the SAME symbol must not parallelize (WAW hazard).
        let stages = plan(vec![
            bind("a", "pure", vec![lit("x")]),
            bind("a", "pure", vec![lit("y")]),
        ]);
        assert_eq!(
            stages,
            vec![Stage::Sequential(NodeId(0)), Stage::Sequential(NodeId(1))]
        );
    }
}
