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
| `verify` | Run a command and assert its output contains an expected substring; abort the flow with a structured error if it does not. `cmd` is any node that produces a string (typically a `bash` call); `expect` is the substring the output must contain. |
| `return` | End the flow with a value. |
| `peek` | Read the current in-session value of a named symbol without any filesystem IO. Returns the symbol's stored value, or null if the symbol is not yet bound. |
| `var` | Reference a bound symbol. |
| `lit` | A literal value (raw JSON, as written in the AST by the compiler front-end). |
| `thing` | A reference to an external thing. |
| `expr` | Pure inline computation. `formula` is a safe whitelist expression over named variables: arithmetic (`+ - * /`, `round(x,n)`, `abs`, `min(a,b)`, `max(a,b)`), comparison (`== != < <= > >=`), boolean (`&& || !`, `true`/`false`), string functions (`len/lower/upper/trim/replace/repeat/reverse/contains/concat`), and string literals (`'…'`/`"…"`). `+` adds when both sides are numeric and concatenates otherwise. Because it yields a bool, an `expr` is also a valid `when`/`unless`/`until`/`assert` condition. `vars` maps variable names to node expressions (only `Lit` and `Var` are valid). No IO, no approval gate. Examples: `expr("price * 2", {"price": $btc})`, `expr("status == 'ok' && n > 0", …)`. |
| `fmt` | Pure string interpolation. `template` is a string with `{name}` placeholders substituted from already-bound session symbols (same `{name}`/`{{name}}` syntax as `Lit` interpolation). No IO, no approval gate. Example: `fmt("BTC: {price} | Double: {doubled}")`. |
| `jq` | Pure JSON path extraction. `path` is a dot-path string (e.g. `".bitcoin.usd"` or `"results[0].value"`) applied to the JSON content of `input` (a `Var` or `Lit` node). No IO, no approval gate. Example: `jq(".bitcoin.usd", $raw)`. |
| `parse` | Pure type coercion. Converts the string result of a `jq` or `fmt` node into a typed value. `as_type` is one of `"f64"`, `"i64"`, `"bool"`, `"json"`, `"string"`. No IO, no approval gate. Example: `parse(jq(".price", $raw), as: "f64")`. |
| `ctx` | Build a bounded, budgeted **context pack** from existing symbols. Resolves `include` (minus `exclude`) to its members, then — when `budget` is set — shrinks the pack *at evaluation* by visibility tier then declared order until within the char budget, recording any dropped members in the run trace. Produces a `Ctx` value bound to `name`. Pure: it selects and labels existing values, performing no IO (the load-bearing elevation of PRD §13 explicit context management). |
| `ctx_append` | Accrete more symbols into an existing context pack (the `+=` marker). Immutably rebinds `ctx` to a *new* `Ctx` value (preserving the audit chain `$pack@1 → @2`) with `add` appended, then re-applies the pack's budget. Pure. |
| `match` | Multi-way **exhaustive** branch: evaluate `subject` (a literal or bound symbol), then run the body of the first `case` whose `value` equals it — by JSON equality, so a *string* subject does not equal a *numeric* literal. If none match, run `default`. A deterministic replacement for chains of `when`. To branch on an op's result, bind it first (`$s = call(); match $s {…}`) or use `route`. The analyzer requires at least one case; at runtime an unmatched subject with no `default` is an error — the exhaustiveness guard-rail. |
| `route` | Model-routed branch — the signature *bounded non-determinism* primitive. Run `selector` (typically a `!model` op) to produce a label, then run the `case` whose `label` it names. The cases are fixed and analyzer-validated: the model chooses *which* declared branch runs, never *what*. Falls back to `default` when the label matches no case (an error if `default` is empty). |
| `fallback` | Ordered "first that succeeds wins" selector: run each branch in `branches` in turn; the first that completes without error and yields a non-empty result wins and becomes the node's result. On a branch error (or empty result) the next is tried — so a *side-effecting* branch that returns empty will still fall through and the next branch also runs (attempts stream live, as in `try`/`retry`). If every branch errors, the last error propagates. Lighter than `try` for graceful degradation (cheap path → else expensive path). `bind` names the winning result. |
| `timeout` | Bound the wall-clock of a sub-flow: run `body` with a `ms` deadline. If it does not finish in time the node errors (an enclosing `try`/`retry` may catch it). A general reliability guard-rail you can wrap around anything. `bind` names the body's result. |
| `budget` | Cap the cost of a scope: run `body` but allow at most `limit` op dispatches within it (checked at statement boundaries; a nested statement can consume more than one dispatch before the next check). A first-class cost guard-rail; v1 counts dispatches (token/money budgets are a later refinement). `bind` names the body's result. |
| `scope` | RAII-style **acquire → use → release** with guaranteed cleanup. Optionally run `acquire` first (binding its result to `bind`, so `body` and `finally` can name the resource), then run `body`; `finally` **always** runs afterward — on normal completion, an early `return`, or an error — so a lock is freed / a handle closed / a temp removed no matter how the body exits. The body's result, `return`, or error then propagates; a `finally` failure surfaces only when the body itself succeeded (it never masks the body's own error). If `acquire` errors the resource was never taken, so `finally` does not run. The deterministic resource-lifecycle guard-rail (RAII for flows). |
| `saga` | Saga / **compensating transaction**: run each `step` in order; after a step's `body` succeeds, its `undo` is registered. If a *later* step fails, the runtime unwinds by running the registered `undo` bodies in **reverse** order (best-effort — an `undo` failure is recorded but does not stop the unwind), then propagates the original error. The strongest guard-rail for non-transactional external side effects (charge→refund, create→delete, reserve→release): partial work is rolled back rather than left dangling. A `return` inside a step is a successful early exit and does not compensate (use `scope` for guaranteed cleanup on every exit). |
| `once` | **At-most-once side effect** across re-runs — an effect-level `memo`. `label` is an explicit idempotency key: the first time the body runs to success in a session its result is recorded durably; later re-runs in the same session skip the body and reuse the stored result. A failed body records nothing and is retried. `bind` optionally names the body's result. Safety under re-execution (`send_email`/`charge` never fire twice). With no durable store wired (a throwaway interpreter) it degrades to running every time. Requires a non-empty literal label. |
| `checkpoint` | **Durable resume point** for long-running / resumable flows. A **top-level-only** marker (like `await`): the first time a run reaches it, the position is recorded durably; a later re-run of the *same* flow in the *same* session fast-forwards past the already-completed prefix (its symbols are still durably bound and its side effects are not repeated) and continues from here. `label` is a human-readable name for the phase it closes. Pairs with `once` for finer-grained idempotency; a no-op when no durable store is wired. Requires a non-empty literal label. |
| `obj` | Build an **object value** from sub-expressions — the record constructor `{ k: expr, … }`. Each field value is itself a node, so a record can mix literals and variables: `{ ok: true, n: $count, intent: $extract.intent }`. Pure: it assembles a value, performing no IO and no op dispatch. Leaves must be pure value nodes (`var`/`lit`/`jq`/`expr`/`fmt`/`obj`/ `list`); a call or control-flow leaf is rejected by the analyzer so templates stay side-effect free. This is what lets `return { … }` assemble a result from computed symbols. |
| `list` | Build a **list value** from sub-expressions — the list constructor `[ expr, … ]`. Each item is itself a node (`[ $a, $b, 3 ]`). Pure, same leaf rules as [`Node::Obj`]; the array twin of the record constructor. |
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

**Context pack (ctx / ctx_append):**
```json
{"body": [
  {"kind": "ctx", "name": "debug", "purpose": "smallest likely bug",
   "include": ["src", "failures", "claims"], "exclude": ["generated"], "budget": 9000},
  {"kind": "ctx_append", "ctx": "debug", "add": ["more_src"]}
]}
```
A `ctx` selects existing symbols (`include` minus `exclude`) into a budgeted pack — shrunk by
visibility then declared order to `budget` chars, with any drops recorded in the trace. `ctx_append`
accretes more symbols into it. Both are pure (no IO).

## Artifact types (prelude)

An opt-in stdlib of `Named` type schemas an agent task manipulates — claims, evidence, needs, context
packs, patches, and structured returns. They are ordinary `Struct` values whose `Named` type names one
of these schemas; ops declare their inputs/outputs in these terms.

<!-- BEGIN generated:prelude-types -->
| type | description |
|---|---|
| `Span` | A cited region inside a source document — the proof pointer a `Claim` or `Evidence` points at. |
| `Claim` | A factual assertion extracted from a source, carrying its provenance span and a confidence score. |
| `Evidence` | A claim together with the supporting spans that ground it — the audited unit of support. |
| `Need` | An explicit statement of missing information: what to ask, which fields are required to satisfy it, and the condition under which it is considered met. Produced by the pure `need` op; its complement `gaps` reports the still-unmet `require` fields. |
| `Ctx` | A bounded, intentionally-budgeted bundle of context — the value produced by the `ctx`/`ctx_append` nodes. `members` are the symbol references selected into the pack; `budget` is the char/token cap the runtime shrinks the pack to at node evaluation. |
| `Query` | A structured retrieval request over one or more datasources — the input to the `query`/`Search.run` ops. |
| `Answer` | A structured, evidence-bearing **successful** return from an agent task. |
| `Blocked` | A structured return signalling the task **could not** be completed, with the open gaps that blocked it. Same shape as [`Answer`] but a distinct type so callers can branch on success vs. blockage. |
| `Patch` | A proposed code change — a concrete unified diff plus the path it applies to. |
| `TestResult` | The outcome of running a test command. |
| `Verdict` | A judge step's structured decision: the chosen outcome, the reasons behind it, and the evidence it weighed. Consumed by the `ai.judge` cognition op. |
<!-- END generated:prelude-types -->
