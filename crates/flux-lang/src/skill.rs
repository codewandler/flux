//! The Flux-Lang **language skill**: a self-contained reference an LLM reads to author Flux-Lang
//! ASTs. It is generated — the node-kind table comes from [`crate::schema::node_kind_catalog`] (the
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
    s
}

const HEAD: &str = r##"---
description: How to author Flux-Lang — the typed execution-graph language an LLM emits (node kinds, control flow, pure expressions). Operations are host-provided.
triggers: [flux-lang, fluxlang, flux-flow, emit_plan, ast, plan, flow, dag]
---

# Flux-Lang — the language

Flux-Lang is a small language **built for LLMs**. You express a task as a typed JSON **execution graph**
(an AST) and a deterministic runtime runs it — instead of acting tool-by-tool, you emit one readable
plan. Control flow, iteration, error handling, and pure data shaping are all **nodes** in the graph,
never hidden inside an op's arguments. The runtime stores results as **symbols** and resolves them to
**values**, so raw outputs are referenced by name, not re-sent every step.

The **operations** a `call` node targets (file reads, shell, sub-agents, …) are advertised by the host
runtime — they are not part of the language. This reference covers the language itself.

## Top-level shape

```json
{"name": "optional-name", "params": [{"name": "x", "ty": "String"}], "returns": "Result", "body": [Node, ...]}
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
- **Keep one task in one plan.** Put gathering, work, and verification in a single graph rather than
  many tiny plans.
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
     "args": [{"kind": "var", "name": "src"}, {"kind": "lit", "value": "TODO"}]}}
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
"##;

#[cfg(test)]
mod tests {
    use super::*;

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
