//! Lookup recipes — graceful degradation with `fallback`, wrapped into the typed prelude `Answer`.

use crate::dsl::*;

/// Look `question` up with `primary_op`; on a miss, degrade to `escalate_op`; then wrap whatever was
/// retrieved into a typed answer with `synth_op`.
///
/// Builds:
///
/// ```text
/// fallback {                       // first NON-EMPTY success wins
///   branch: primary_op(question)   // e.g. a KB hit
///   branch: escalate_op(question)  // miss → a canned escalation
/// } -> $kb
/// $answer = synth_op($kb)          // an `Answer`-shaped JSON string
/// return $answer
/// ```
///
/// `fallback` passes over a branch whose output is the empty string, so `primary_op` should return `""`
/// on a miss to fall through to `escalate_op`. `synth_op` is expected to return a JSON string matching
/// the prelude `Answer` (so `ExecutionResult::answer()` round-trips it) — `answered` on a hit,
/// `unanswered` (with a gap) when handed the escalation sentinel.
///
/// `question` is cloned into both retrieval branches.
pub fn answer_with_fallback(
    primary_op: &str,
    escalate_op: &str,
    synth_op: &str,
    question: Node,
) -> DraftAst {
    Flow::named("answer_with_fallback")
        .body(|b| {
            b.fallback(|f| {
                f.bind("kb");
                f.branch(|bb| {
                    bb.call(primary_op, [question.clone()]);
                });
                f.branch(|bb| {
                    bb.call(escalate_op, [question.clone()]);
                });
            });
            b.bind("answer", call(synth_op, [var("kb")]));
            b.ret(var("answer"));
        })
        .build()
}
