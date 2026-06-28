package dev.flux.intellij.editor

/**
 * The editor-facing Flux-Lang vocabulary used by completion and documentation. Keywords/types come from
 * the language itself (crates/flux-lang/docs/syntax.md). Ops are *host-provided* — `fluxlang` doesn't
 * know them — so this is a curated list of the built-in file/agent ops from docs/usage.md.
 */
object FluxVocabulary {
    val KEYWORDS = listOf(
        "flow", "type", "when", "unless", "else", "repeat", "until", "each", "in", "flat",
        "loop", "timeout", "budget", "match", "case", "default", "route", "fallback", "branch",
        "block", "pipe", "parallel", "race", "try", "catch", "retry", "confirm", "throttle",
        "debounce", "assert", "verify", "memo", "await", "as", "peek", "return", "do",
    )

    val TYPES = listOf("String", "Number", "Bool", "Any", "List")

    /** Built-in ops advertised by the host runtime (curated from docs/usage.md). */
    val OPS = listOf(
        "read", "read_many", "write", "append", "edit", "patch", "glob", "grep", "bash",
        "web_fetch", "task", "fmt", "expr", "jq", "parse",
    )

    val ANNOTATIONS = listOf(
        "@effect", "@person", "@file", "@ticket", "@url", "@secret", "@repo", "@dataset",
        "@email", "@calendar_event",
    )

    /** Short hover docs, keyed by keyword/node. Sourced from crates/flux-lang/docs/syntax.md. */
    val DOCS: Map<String, String> = mapOf(
        "flow" to "Declares a flow: <code>flow name(params) -&gt; ReturnType</code>. The unit of execution.",
        "type" to "A structural type declaration (record fields, or <code>| variant</code> unions).",
        "when" to "Conditional branch. Runs the body when the condition is truthy; optional <code>else</code>.",
        "unless" to "Guard clause — sugar for <code>when !cond</code>. No <code>else</code> branch.",
        "else" to "The alternative branch of a <code>when</code>, at the same indent as its <code>when</code>.",
        "repeat" to "Counter-driven bounded loop: <code>repeat N</code>. Optional <code>until</code> first body line.",
        "until" to "Early-exit condition; written as the first line of a <code>repeat</code>/<code>loop</code> body.",
        "each" to "List-driven loop: <code>each \$item in \$list</code>; optional <code>-&gt; \$collect</code>.",
        "flat" to "In an <code>each ... -&gt; flat \$x</code>, concatenate per-iteration lists.",
        "loop" to "Time-bounded loop: <code>loop for &lt;ms&gt; every &lt;ms&gt;</code>; optional <code>-&gt; \$bind</code>.",
        "timeout" to "Bounds its body by wall-clock milliseconds: <code>timeout &lt;ms&gt;</code>.",
        "budget" to "Caps the number of dispatches in its body: <code>budget N</code>.",
        "match" to "Exhaustive multi-way branch on a bound value via <code>case</code>/<code>default</code>.",
        "case" to "An arm of a <code>match</code>/<code>route</code> block.",
        "default" to "The fallback arm of a <code>match</code>/<code>route</code>.",
        "route" to "Like <code>match</code>, but a model-backed selector picks which labeled branch runs.",
        "fallback" to "Ordered “first branch that succeeds wins”; <code>-&gt; \$bind</code> names the winner.",
        "branch" to "A bare body arm inside <code>fallback</code>.",
        "block" to "A sequential block that optionally binds its final result with <code>-&gt; \$x</code>.",
        "pipe" to "Each step's output feeds the next step's first argument.",
        "parallel" to "Run independent <code>\$name:</code> branches concurrently; each name is bound after.",
        "race" to "Run branches concurrently; first success wins. <code>timeout</code> is required.",
        "try" to "Run the body, suppressing or handling errors with an optional <code>catch \$err</code>.",
        "catch" to "Handles a <code>try</code> failure, binding the error message to <code>\$err</code>.",
        "retry" to "Retry the body up to N times: <code>retry N, backoff:, delay:</code>.",
        "confirm" to "Human-in-the-loop approval gate: <code>confirm \"msg\", risk: low|medium|high|critical</code>.",
        "throttle" to "At most <code>max</code> executions per sliding <code>window</code> ms.",
        "debounce" to "Fire the body only after <code>wait</code> ms of quiet.",
        "assert" to "Abort the flow if the condition is falsey; optional message argument.",
        "verify" to "Run a command and assert its output contains a pattern: <code>verify P in cmd</code>.",
        "memo" to "Cross-turn cache: <code>memo \$x = expr</code> computes once per session.",
        "await" to "Suspend until an external event arrives (top-level only).",
        "peek" to "Read a symbol's current value by name without IO: <code>peek(\"name\")</code>.",
        "return" to "Unconditional early exit from the flow with a value (or null).",
        "do" to "Bare effectful call whose result is discarded: <code>do op args</code>.",
        "ctx" to "Declare a bounded, budgeted context pack (<code>purpose</code>/<code>budget</code>/<code>include</code>).",
    )
}
