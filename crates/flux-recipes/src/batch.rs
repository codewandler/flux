//! Batch / loop recipes — the loop family: `each`, `repeat`, `loop`, `race`.

use crate::dsl::*;

/// Map `op` over every element of `source`, collecting the per-item results into `collect`.
///
/// Builds:
///
/// ```text
/// each $item in source -> $collect:
///   op($item)
/// return $collect
/// ```
///
/// `source` must evaluate to a list. Each iteration binds the element to `$item`, calls `op($item)`, and
/// the per-iteration result is gathered (in order) into the `$collect` list, which is the flow's result.
pub fn map_each(item: &str, source: Node, op: &str, collect: &str) -> DraftAst {
    Flow::named("map_each")
        .body(|b| {
            b.each(item, source, |e| {
                e.collect(collect);
                e.body(|bb| {
                    bb.call(op, [var(item)]);
                });
            });
            b.ret(var(collect));
        })
        .build()
}

/// Call `op(input)` up to `max` times, stopping early once `until` holds; bind each result to `$bind`
/// and return the last one.
///
/// Builds:
///
/// ```text
/// repeat max:
///   $bind: op(input)
/// until until            // stop-when-true, evaluated after each iteration
/// return $bind
/// ```
///
/// `until` is a *stop-when-true* guard checked after every iteration. It must be a condition the runtime
/// can evaluate — a `call`, or a `var`/`lit` (truthy check); commonly `var(bind)` to stop as soon as a
/// non-empty result lands. `input` is cloned into the (single) body.
pub fn repeat_until(max: u32, op: &str, input: Node, bind: &str, until: Node) -> DraftAst {
    Flow::named("repeat_until")
        .body(|b| {
            b.repeat(max, |r| {
                r.body(|bb| {
                    bb.bind(bind, call(op, [input.clone()]));
                });
                r.until(until);
            });
            b.ret(var(bind));
        })
        .build()
}

/// Poll `op(input)` repeatedly for up to `for_ms` milliseconds, sleeping `every_ms` between attempts.
///
/// Builds:
///
/// ```text
/// loop for_ms every every_ms:
///   op(input)
/// ```
///
/// The loop runs the body at least once, then stops at the time budget. The flow's result is the last
/// attempt's output. `input` is cloned into the body.
pub fn poll_for(for_ms: u64, every_ms: u64, op: &str, input: Node) -> DraftAst {
    Flow::named("poll_for")
        .body(|b| {
            b.loop_for(for_ms, |l| {
                l.every_ms(every_ms);
                l.body(|bb| {
                    bb.call(op, [input.clone()]);
                });
            });
        })
        .build()
}

/// Run `ops` concurrently against `input`; the first to finish wins and binds to `$bind`.
///
/// Builds:
///
/// ```text
/// race timeout_ms {
///   branch: op_a(input)
///   branch: op_b(input)
///   …
/// } -> $bind
/// return $bind
/// ```
///
/// All branches start together; losers are cancelled once one succeeds. If none finishes within
/// `timeout_ms`, the flow errors. `input` is cloned into each branch.
pub fn race_first(timeout_ms: u64, ops: &[&str], input: Node, bind: &str) -> DraftAst {
    Flow::named("race_first")
        .body(|b| {
            b.race(timeout_ms, |r| {
                for (i, &op) in ops.iter().enumerate() {
                    r.branch(format!("branch{i}"), |bb| {
                        bb.call(op, [input.clone()]);
                    });
                }
                r.bind(bind);
            });
            b.ret(var(bind));
        })
        .build()
}
