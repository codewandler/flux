//! Routing recipes — the `route` primitive (bounded non-determinism).
//!
//! One model-cost classification step picks **which** declared branch runs; everything downstream is
//! deterministic. The classifier never decides **what** a branch does, only which label applies.

use crate::dsl::*;

/// Classify `input` with `classify_op`, then dispatch to the handler op for the matching intent label.
///
/// Builds:
///
/// ```text
/// route( classify_op(input) ) {
///   case <label> -> <handler>(input)   // one arm per (label, handler) in `arms`
///   …
///   default      -> default_handler(input)
/// }
/// ```
///
/// The classifier must return a **bare label string** (not JSON-quoted): `route` trims the selector's
/// output to a string and matches it by equality against the arm labels, falling to `default` on no
/// match. Each handler op receives the original `input` and its output is the flow's result.
///
/// `input` is cloned into the selector and every arm (it is consumed once per branch).
pub fn route_intent(
    classify_op: &str,
    input: Node,
    arms: &[(&str, &str)],
    default_handler: &str,
) -> DraftAst {
    Flow::named("route_intent")
        .body(|b| {
            b.route(call(classify_op, [input.clone()]), |r| {
                for &(label, handler) in arms {
                    r.case(label, |bb| {
                        bb.call(handler, [input.clone()]);
                    });
                }
                r.default(|bb| {
                    bb.call(default_handler, [input.clone()]);
                });
            });
        })
        .build()
}
