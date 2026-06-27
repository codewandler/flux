//! Fan-out recipes — the `parallel` primitive.

use crate::dsl::*;

/// Run every op in `ops` concurrently against `input`; each branch's result binds to `$b{i}`.
///
/// Builds:
///
/// ```text
/// parallel:
///   branch b0: ops[0](input)
///   branch b1: ops[1](input)
///   …
/// ```
///
/// Branches run concurrently but their output is replayed in branch order, so results are deterministic;
/// the flow's result is the last branch's output. Downstream statements can reference each `$b{i}`. A
/// `return` inside a branch is rejected by the engine. `input` is cloned into each branch.
pub fn parallel_all(ops: &[&str], input: Node) -> DraftAst {
    Flow::named("parallel_all")
        .body(|b| {
            b.parallel(|p| {
                for (i, &op) in ops.iter().enumerate() {
                    p.branch(format!("b{i}"), |bb| {
                        bb.call(op, [input.clone()]);
                    });
                }
            });
        })
        .build()
}
