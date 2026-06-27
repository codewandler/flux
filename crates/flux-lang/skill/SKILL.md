---
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

<!-- BEGIN generated:node-kinds -->
| kind | description |
|---|---|
| `call` | Invoke a registered operation with argument expressions. |
| `bind` | Bind the result of an expression to a symbol. |
| `when` | Conditional control flow. |
| `repeat` | A bounded loop (`max` is required; the analyzer rejects unbounded loops). |
| `each` | Map a list value through a body (list-driven loop; `repeat` stays counter-driven). Each element is bound to `as`; an optional `collect` symbol gathers the per-iteration results into a list. |
| `assert` | A boolean guard: aborts the flow with an error if the condition is falsey. |
| `pipe` | A chain of calls where each step's output is fed as the first argument of the next. |
| `seq` | A sequential block; runs its body in order. Optionally binds the block's final result. |
| `memo` | Like `bind`, but pinned across turns: if the symbol is already resolved for this session, skip execution and reuse the cached value (compute-once-per-session, keyed on symbol name). |
| `parallel` | Concurrent fan-out: run independent branches, binding each branch's result to its name. |
| `await` | Pause until an external event/input arrives. |
| `retry` | Retry a body on failure with optional backoff. Fatal errors (policy denial, unknown op) are never retried. `backoff` may be `"none"` | `"linear"` | `"exponential"`. |
| `try` | Structured error handling: run `body`; on failure bind the error string to `catch` and run `handler`. If the handler also errors, propagate that error. |
| `confirm` | Explicit human-in-the-loop gate. Calls the existing `Approver` — `--yes` and TUI modal handle it automatically. Body only runs on approval; on denial the node errors. `risk` may be `"low"` | `"medium"` | `"high"` | `"critical"`. |
| `loop` | Time-bounded iteration. `for_ms` is required (the analyzer rejects unbounded loops). `every_ms` is the inter-iteration sleep (0 = tight). `until` is an early-exit condition. |
| `race` | First-wins concurrency: run branches in parallel and return as soon as the first succeeds. `timeout_ms` is required; if no branch succeeds within it the node errors. `bind` names the symbol that receives the winning branch's result. |
| `throttle` | Rate-limit body execution: at most `max` dispatches per `window_ms` sliding window. The token bucket is tracked in the session store keyed by `name`; plan authors declare intent, runtime enforces. `name` must be unique within a session to avoid bucket collisions. |
| `debounce` | Coalesce rapid re-invocations: wait `wait_ms` after the last trigger before running body. In a `loop`/`watch` context the body only executes when things have settled. `name` is used as a stable key so debounce state survives across turns. |
| `unless` | Negated conditional: run `body` only when `cond` is falsey. Sugar for `when !cond`; the body may contain any nodes (reads, writes, sub-plans — anything). |
| `verify` | Run a command and assert its output matches an expected pattern; abort the flow with a structured error if it does not. `cmd` is any node that produces a string (typically a `bash` call); `expect` is a substring or regex the output must contain. |
| `return` | End the flow with a value. |
| `peek` | Read the current in-session value of a named symbol without any filesystem IO. Returns the symbol's stored value, or null if the symbol is not yet bound. |
| `var` | Reference a bound symbol. |
| `lit` | A literal value (raw JSON, as written in the AST by the compiler front-end). |
| `thing` | A reference to an external thing. |
| `expr` | Pure inline arithmetic. `formula` is a safe whitelist expression (`+`, `-`, `*`, `/`, `round(x,n)`, `abs`, `min(a,b)`, `max(a,b)`) over named variables. `vars` maps variable names to node expressions (only `Lit` and `Var` are valid). No IO, no approval gate. Example: `expr("price * 2", {"price": $btc})`. |
| `fmt` | Pure string interpolation. `template` is a string with `{name}` placeholders substituted from already-bound session symbols (same `{name}`/`{{name}}` syntax as `Lit` interpolation). No IO, no approval gate. Example: `fmt("BTC: {price} | Double: {doubled}")`. |
| `jq` | Pure JSON path extraction. `path` is a dot-path string (e.g. `".bitcoin.usd"` or `"results[0].value"`) applied to the JSON content of `input` (a `Var` or `Lit` node). No IO, no approval gate. Example: `jq(".bitcoin.usd", $raw)`. |
| `parse` | Pure type coercion. Converts the string result of a `jq` or `fmt` node into a typed value. `as_type` is one of `"f64"`, `"i64"`, `"bool"`, `"json"`, `"string"`. No IO, no approval gate. Example: `parse(jq(".price", $raw), as: "f64")`. |
<!-- END generated:node-kinds -->

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
