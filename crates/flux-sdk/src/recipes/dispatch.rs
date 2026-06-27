//! Dispatch recipes — the `match` primitive (value dispatch).

use crate::dsl::*;

/// Compute a subject with `subject_op(input)`, then dispatch to a handler whose case value JSON-equals it.
///
/// Builds:
///
/// ```text
/// $subject = subject_op(input)
/// match $subject {
///   case <value> -> <handler>(input)   // one arm per (value, handler) in `arms`
///   …
///   default      -> default_handler(input)
/// }
/// ```
///
/// Unlike [`routing::route_intent`](super::routing::route_intent) (which trims a model selector to a
/// label), `match` compares a **pre-bound** value by JSON equality — so the subject is bound first, then
/// matched. `subject_op` should return exactly the case string it is meant to match. With no matching case
/// and no `default` the engine errors; this recipe always supplies a `default_handler`. `input` is cloned
/// into the subject step and every arm.
pub fn match_value(
    subject_op: &str,
    input: Node,
    arms: &[(&str, &str)],
    default_handler: &str,
) -> DraftAst {
    Flow::named("match_value")
        .body(|b| {
            b.bind("subject", call(subject_op, [input.clone()]));
            b.match_(var("subject"), |m| {
                for &(value, handler) in arms {
                    m.case(lit(value), |bb| {
                        bb.call(handler, [input.clone()]);
                    });
                }
                m.default(|bb| {
                    bb.call(default_handler, [input.clone()]);
                });
            });
        })
        .build()
}
