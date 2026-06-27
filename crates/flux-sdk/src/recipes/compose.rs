//! Composed recipes — primitives nested into one resilient flow.

use crate::dsl::*;

/// A resilient call: retry a time-boxed primary→backup fallback, binding the eventual result to `$bind`.
///
/// Composes three primitives:
///
/// ```text
/// retry max backoff <backoff> delay <delay_ms>ms:
///   timeout timeout_ms:
///     fallback:
///       branch: primary(input)   // first non-empty success wins
///       branch: backup(input)
/// -> $bind
/// return $bind
/// ```
///
/// On each attempt, `primary` is tried and `backup` is the fallback (the first non-empty success wins),
/// the whole attempt is bounded by `timeout_ms`, and a failed/timed-out attempt is retried up to `max`
/// times with backoff. `timeout`/`fallback` propagate their value outward, so only the outer `retry`
/// binds `$bind`. `input` is cloned into each branch.
// The arity mirrors the three composed primitives (retry × timeout × fallback); a config struct would
// only obscure that and break the positional style the rest of the cookbook uses.
#[allow(clippy::too_many_arguments)]
pub fn resilient_call(
    max: u32,
    backoff: &str,
    delay_ms: u64,
    timeout_ms: u64,
    primary: &str,
    backup: &str,
    input: Node,
    bind: &str,
) -> DraftAst {
    Flow::named("resilient_call")
        .body(|b| {
            b.retry(max, |r| {
                r.backoff(backoff);
                r.delay_ms(delay_ms);
                r.bind(bind);
                r.body(|bb| {
                    bb.timeout(timeout_ms, |w| {
                        w.body(|bbb| {
                            bbb.fallback(|f| {
                                f.branch(|x| {
                                    x.call(primary, [input.clone()]);
                                });
                                f.branch(|x| {
                                    x.call(backup, [input.clone()]);
                                });
                            });
                        });
                    });
                });
            });
            b.ret(var(bind));
        })
        .build()
}
