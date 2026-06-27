//! Resilience recipes — `retry`, `timeout`, `budget`, and `try`/`catch`.

use crate::dsl::*;

/// Call `op(input)`, retrying on **error** up to `max` attempts with a backoff between tries; bind the
/// successful result to `$bind`.
///
/// Builds:
///
/// ```text
/// retry max backoff <backoff> delay <delay_ms>ms:
///   op(input)
/// -> $bind
/// return $bind
/// ```
///
/// `backoff` is `"none"`, `"linear"`, or `"exponential"` over `delay_ms`. Retry triggers on a failed op
/// (an erroring tool result), **not** on a condition; fatal errors (a denied confirm, a failed assert)
/// are never retried. If every attempt fails, `execute` returns an error. `input` is cloned into the body.
pub fn retry_with_backoff(
    max: u32,
    backoff: &str,
    delay_ms: u64,
    op: &str,
    input: Node,
    bind: &str,
) -> DraftAst {
    Flow::named("retry_with_backoff")
        .body(|b| {
            b.retry(max, |r| {
                r.backoff(backoff);
                r.delay_ms(delay_ms);
                r.bind(bind);
                r.body(|bb| {
                    bb.call(op, [input.clone()]);
                });
            });
            b.ret(var(bind));
        })
        .build()
}

/// Run `op(input)` under a wall-clock deadline of `ms` milliseconds; bind the result to `$bind`.
///
/// Builds:
///
/// ```text
/// timeout ms:
///   op(input)
/// -> $bind
/// return $bind
/// ```
///
/// If the body does not finish within `ms` (`ms` must be > 0), `execute` returns a timeout error.
pub fn with_timeout(ms: u64, op: &str, input: Node, bind: &str) -> DraftAst {
    Flow::named("with_timeout")
        .body(|b| {
            b.timeout(ms, |w| {
                w.bind(bind);
                w.body(|bb| {
                    bb.call(op, [input.clone()]);
                });
            });
            b.ret(var(bind));
        })
        .build()
}

/// Run `op(input)` under a cost cap of at most `limit` op dispatches in scope; bind the result to `$bind`.
///
/// Builds:
///
/// ```text
/// budget limit:
///   op(input)
/// -> $bind
/// return $bind
/// ```
///
/// The cap is checked at each statement boundary; exceeding it makes `execute` return a budget error.
pub fn with_budget(limit: u32, op: &str, input: Node, bind: &str) -> DraftAst {
    Flow::named("with_budget")
        .body(|b| {
            b.budget(limit, |w| {
                w.bind(bind);
                w.body(|bb| {
                    bb.call(op, [input.clone()]);
                });
            });
            b.ret(var(bind));
        })
        .build()
}

/// Try `op(input)`; if it fails, bind the error string to `$catch` and run `handler($catch)` instead.
///
/// Builds:
///
/// ```text
/// try:
///   op(input)
/// catch $catch:
///   handler($catch)
/// ```
///
/// The flow's result is the body's output on success, or the handler's output if the body errored and was
/// caught. `input` is cloned into the body.
pub fn try_catch(op: &str, input: Node, catch: &str, handler: &str) -> DraftAst {
    Flow::named("try_catch")
        .body(|b| {
            b.try_(|t| {
                t.body(|bb| {
                    bb.call(op, [input.clone()]);
                });
                t.catch(catch);
                t.handler(|bb| {
                    bb.call(handler, [var(catch)]);
                });
            });
        })
        .build()
}
