//! The Flux-Lang **language skill**: a self-contained reference for authoring durable Flux-Lang
//! programs. It is generated — the node-kind table comes from [`crate::schema::node_kind_catalog`] (the
//! `Node` doc-comments), so it can never drift from the types. Unlike the engine's skill it carries
//! **no registered-ops table**: operations are provided by the host runtime, not the language.
//!
//! [`render`] returns the full markdown; the committed artifact `crates/flux-lang/skill/SKILL.md` is
//! its cached output, verified by `tests/skill_in_sync.rs`.

/// Render the complete Flux-Lang language skill as markdown.
pub fn render() -> String {
    let mut s = String::new();
    s.push_str(HEAD);
    s.push_str("<!-- BEGIN generated:node-kinds -->\n");
    s.push_str(&crate::schema::node_kind_catalog());
    s.push_str("<!-- END generated:node-kinds -->\n");
    s.push_str(BODY);
    s.push_str(PRELUDE);
    s.push_str("<!-- BEGIN generated:prelude-types -->\n");
    s.push_str(&crate::prelude::prelude_type_catalog());
    s.push_str("<!-- END generated:prelude-types -->\n");
    s
}

const HEAD: &str = r##"---
description: How to author Flux-Lang — typed, deterministic agent control flow with explicit operations, branches, and durable state.
triggers: [flux-lang, fluxlang, flux-flow, ast, flow, journey, dag]
---

# Flux-Lang — the language

Flux-Lang is the typed language for the parts of an agent that must be reliable. Developers and
coding agents author a readable `.flux` program; the analyzer lowers it to this JSON execution-graph
representation and a deterministic runtime executes it. A conversational model does **not** generate
the executable graph during a turn: model-backed stages return typed values or native operation
calls inside control flow the application already owns. Iteration, error handling, and data shaping
are explicit nodes. Results are stored as **symbols** and resolved to **values**.

The **operations** a `call` node targets (file reads, shell, sub-agents, …) are advertised by the host
runtime — they are not part of the language. Prefer Flux text for checked-in programs; this reference
also documents the canonical AST used by analyzers, SDKs, and durable execution.

## Top-level shape

```json
{"name": "optional-name", "params": [{"name": "x", "ty": "string"}], "returns": {"named": "Result"}, "body": [Node, ...]}
```

`name`, `params`, and `returns` are optional; `body` is the ordered list of nodes the runtime runs
top-to-bottom. A node is tagged by its `"kind"`.

## Node kinds

"##;

const BODY: &str = r##"
## Writing rules

- **Express control flow as nodes**, never inside an op's arguments. Loops are `repeat`/`each`/`loop`;
  branches are `when`/`unless`; error handling is `try`/`retry`. Never put `for`/`if`/`&&` inside a
  `call` argument.
- **Reference results by symbol.** `bind` a result to `$name`, then read it back with a `var` node —
  do not re-fetch the same thing or paste raw output into a later argument.
- **Inline a symbol into a string** with `{name}` (e.g. a `fmt` template or a message argument); pass a
  whole value as an argument with a `var` node.
- **Shape data with pure nodes** — `expr` (arithmetic), `fmt` (interpolation), `jq` (path extraction),
  `parse` (coercion). They do no IO and need no approval.
- **Give each flow one durable responsibility.** Compose flows and journeys explicitly instead of
  asking a turn-time model to invent control flow.
- **Bounded iteration only.** `repeat` needs `max`; `loop` needs `for_ms`; the analyzer rejects
  unbounded loops.

## Examples

The op names below (`read`, `grep`, …) are illustrative — your host advertises the real catalog.

**Bind and reference:**
```json
{"body": [
  {"kind": "bind", "name": "src",
   "value": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "README.md"}]}},
  {"kind": "bind", "name": "hits",
   "value": {"kind": "call", "op": "grep",
     "args": [{"kind": "lit", "value": {"pattern": "TODO"}}]}}
]}
```

**Bounded loop (repeat):**
```json
{"body": [
  {"kind": "repeat", "max": 3, "body": [
    {"kind": "call", "op": "notify", "args": [{"kind": "lit", "value": "tick"}]}
  ]}
]}
```

**Branch (when):**
```json
{"body": [
  {"kind": "bind", "name": "out", "value": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "x"}]}},
  {"kind": "when", "cond": {"kind": "var", "name": "out"},
   "then":      [{"kind": "call", "op": "use", "args": [{"kind": "var", "name": "out"}]}],
   "otherwise": [{"kind": "call", "op": "fallback", "args": []}]}
]}
```

**Iterate a list (each), collecting results:**
```json
{"body": [
  {"kind": "each", "in": {"kind": "lit", "value": ["a.rs", "b.rs", "c.rs"]}, "as": "f",
   "body": [{"kind": "bind", "name": "t", "value": {"kind": "call", "op": "read", "args": [{"kind": "var", "name": "f"}]}}],
   "collect": "all"}
]}
```
Prefer `each` over `repeat` when iterating a known list.

**Concurrency (parallel):**
```json
{"body": [
  {"kind": "parallel", "branches": [
    {"name": "readme", "body": [{"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "README.md"}]}]},
    {"name": "todos",  "body": [{"kind": "call", "op": "grep", "args": [{"kind": "lit", "value": "TODO"}]}]}
  ]}
]}
```
Each branch binds its result to its `$name`; use distinct names and do not `return` inside a branch.

**Chain + guard (pipe / assert):**
```json
{"body": [
  {"kind": "pipe", "bind": "hits", "steps": [
    {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "log.txt"}]},
    {"kind": "call", "op": "grep", "args": [{"kind": "lit", "value": "ERROR"}]}
  ]},
  {"kind": "assert", "cond": {"kind": "var", "name": "hits"}, "message": "no errors found"}
]}
```
In a `pipe`, each step's output becomes the next step's first argument.

**Pure data shaping (jq / parse / fmt):**
```json
{"body": [
  {"kind": "bind", "name": "raw", "value": {"kind": "call", "op": "fetch", "args": [{"kind": "lit", "value": "https://api/price"}]}},
  {"kind": "bind", "name": "usd", "value": {"kind": "parse",
     "value": {"kind": "jq", "path": ".bitcoin.usd", "input": {"kind": "var", "name": "raw"}}, "as": "f64"}},
  {"kind": "return", "value": {"kind": "fmt", "template": "BTC: {usd}"}}
]}
```

**Context pack (ctx / ctx_append):**
```json
{"body": [
  {"kind": "ctx", "name": "debug", "purpose": "smallest likely bug",
   "include": ["src", "failures", "claims"], "exclude": ["generated"], "budget": 9000},
  {"kind": "ctx_append", "ctx": "debug", "add": ["more_src"]}
]}
```
A `ctx` selects existing symbols (`include` minus `exclude`) into a budgeted pack — shrunk by
visibility then declared order to `budget` chars, with any drops recorded in the trace. Packing is
drop-and-continue: an oversized member is skipped, not a hard stop, so smaller members after it still
survive. `ctx_append` accretes more symbols into it. Both are pure (no IO).
"##;

const PRELUDE: &str = r##"
## Artifact types (prelude)

An opt-in stdlib of `Named` type schemas an agent task manipulates — claims, evidence, needs, context
packs, patches, and structured returns. They are ordinary `Struct` values whose `Named` type names one
of these schemas; ops declare their inputs/outputs in these terms.

"##;

#[cfg(test)]
mod tests {
    use super::*;

    /// F27: every hand-written JSON example in the skill body must stay a valid [`DraftAst`] —
    /// the same drift guard the engine prompt's grammar examples already have
    /// (flux-flow `compile.rs::grammar_examples_parse_and_use_parallel_for_independent_reads`).
    #[test]
    fn body_examples_parse_as_draft_asts() {
        use crate::ast::DraftAst;
        let mut checked = 0usize;
        for chunk in BODY.split("```json").skip(1) {
            let json = chunk
                .split("```")
                .next()
                .expect("fenced example closes")
                .trim();
            let ast: DraftAst = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("skill example must parse as a DraftAst ({e}): {json}"));
            assert!(!ast.body.is_empty(), "example body is non-empty: {json}");
            checked += 1;
        }
        assert!(
            checked >= 8,
            "expected the 8 worked body examples, got {checked}"
        );
    }

    #[test]
    fn skill_embeds_the_generated_node_kinds() {
        let skill = render();
        assert!(skill.contains("<!-- BEGIN generated:node-kinds -->"));
        assert!(skill.contains("<!-- END generated:node-kinds -->"));
        // The table is the schema-derived catalog, verbatim.
        assert!(
            skill.contains("| `call` | Invoke a registered operation with argument expressions. |")
        );
        // Frontmatter + language framing, but no engine ops table.
        assert!(skill.starts_with("---\n"));
        assert!(!skill.contains("## Registered ops"));
    }
}
