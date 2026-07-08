---
title: Node reference
description: Every Flux-Lang node kind — the JSON wire shape, fields, semantics, and the native text spelling where one exists.
---

# Node reference

This is the precise JSON AST reference for Flux-Lang. Planners emit this shape, sessions store it,
and SDKs pass it around. Text and JSON are semantically identical: every `.flux` construct lowers to
these nodes, and nodes without native text syntax are written through `@json`.

## Top-level shape

A flow is a JSON object:

```json
{
  "name": "optional-name",
  "params": [{"name": "ticket", "ty": {"named": "Ticket"}}],
  "returns": {"named": "Result"},
  "body": []
}
```

`name`, `params`, and `returns` are optional; `body` is the ordered list of statement nodes the
runtime executes top to bottom. A node is tagged by its `"kind"`.

## Node kinds at a glance

<!-- Generated from the same `flux_lang::schema::node_kind_catalog()` source of truth as
     crates/flux-lang/docs/reference.md and the SKILL.md language skills — do not hand-edit the
     table below. Regenerate with: `UPDATE=1 cargo test -p flux-lang --test website_in_sync`. -->

This table is derived from the generated node catalog in the repository's
[language reference](https://github.com/codewandler/flux/blob/main/crates/flux-lang/docs/reference.md),
which is produced from the interpreter's own AST definitions.

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
| `retry` | Retry a body on failure with optional backoff. Fatal errors (policy denial, unknown op) are never retried. `backoff` may be `"none"` \| `"linear"` \| `"exponential"`. |
| `try` | Structured error handling: run `body`; on failure bind the error string to `catch` and run `handler`. If the handler also errors, propagate that error. |
| `confirm` | Explicit human-in-the-loop gate. Calls the existing `Approver` — `--yes` and TUI modal handle it automatically. Body only runs on approval; on denial the node errors. `risk` may be `"low"` \| `"medium"` \| `"high"` \| `"critical"`. |
| `loop` | Time-bounded iteration. `for_ms` is required (the analyzer rejects unbounded loops). `every_ms` is the inter-iteration sleep (0 = tight). `until` is an early-exit condition. |
| `race` | First-wins concurrency: run branches in parallel and return as soon as the first succeeds. `timeout_ms` is required; if no branch succeeds within it the node errors. `bind` names the symbol that receives the winning branch's result. |
| `throttle` | Rate-limit body execution: at most `max` dispatches per `window_ms` sliding window. The token bucket is tracked in the session store keyed by `name`; plan authors declare intent, runtime enforces. `name` must be unique within a session to avoid bucket collisions. |
| `debounce` | Coalesce rapid re-invocations: wait `wait_ms` after the last trigger before running body. In a `loop`/`watch` context the body only executes when things have settled. `name` is used as a stable key so debounce state survives across turns. |
| `unless` | Negated conditional: run `body` only when `cond` is falsey. Sugar for `when !cond`; the body may contain any nodes (reads, writes, sub-plans — anything). |
| `verify` | Run a command and assert its output contains an expected substring; abort the flow with a structured error if it does not. `cmd` is any node that produces a string (typically a `bash` call); `expect` is the substring the output must contain. |
| `return` | End the flow with a value. |
| `peek` | Read the current in-session value of a named symbol without any filesystem IO. Returns the symbol's stored value, or an empty string if the symbol is not yet bound. |
| `var` | Reference a bound symbol. |
| `lit` | A literal value (raw JSON, as written in the AST by the compiler front-end). |
| `thing` | A reference to an external thing. |
| `expr` | Pure inline computation. `formula` is a safe whitelist expression over named variables: arithmetic (`+ - * /`, `round(x,n)`, `abs`, `min(a,b)`, `max(a,b)`), comparison (`== != < <= > >=`), boolean (`&& \|\| !`, `true`/`false`), string functions (`len/lower/upper/trim/replace/repeat/reverse/contains/concat`), and string literals (`'…'`/`"…"`). `+` adds when both sides are numeric and concatenates otherwise. Because it yields a bool, an `expr` is also a valid `when`/`unless`/`until`/`assert` condition. `vars` maps variable names to node expressions (only `Lit` and `Var` are valid). No IO, no approval gate. Examples: `expr("price * 2", {"price": $btc})`, `expr("status == 'ok' && n > 0", …)`. |
| `fmt` | Pure string interpolation. `template` is a string with `{name}` placeholders substituted from already-bound session symbols (same `{name}`/`{{name}}` syntax as `Lit` interpolation). No IO, no approval gate. Example: `fmt("BTC: {price} \| Double: {doubled}")`. |
| `jq` | Pure JSON path extraction. `path` is a dot-path string (e.g. `".bitcoin.usd"` or `"results[0].value"`) applied to the JSON content of `input` (a `Var` or `Lit` node). No IO, no approval gate. Example: `jq(".bitcoin.usd", $raw)`. |
| `parse` | Pure type coercion. Converts the string result of a `jq` or `fmt` node into a typed value. `as_type` is one of `"f64"`, `"i64"`, `"bool"`, `"json"`, `"string"`. No IO, no approval gate. Example: `parse(jq(".price", $raw), as: "f64")`. |
| `ctx` | Build a bounded, budgeted **context pack** from existing symbols. Resolves `include` (minus `exclude`) to its members, then — when `budget` is set — shrinks the pack *at evaluation* by visibility tier then declared order until within the char budget, recording any dropped members in the run trace. Produces a `Ctx` value bound to `name`. Pure: it selects and labels existing values, performing no IO (the load-bearing elevation of PRD §13 explicit context management). |
| `ctx_append` | Accrete more symbols into an existing context pack (the `+=` marker). Immutably rebinds `ctx` to a *new* `Ctx` value (preserving the audit chain `$pack@1 → @2`) with `add` appended, then re-applies the pack's budget. Pure. |
| `match` | Multi-way **exhaustive** branch: evaluate `subject` (a literal or bound symbol), then run the body of the first `case` whose `value` equals it — by JSON equality, so a *string* subject does not equal a *numeric* literal. If none match, run `default`. A deterministic replacement for chains of `when`. To branch on an op's result, bind it first (`$s = call(); match $s {…}`) or use `route`. The analyzer requires at least one case; at runtime an unmatched subject with no `default` is an error — the exhaustiveness guard-rail. |
| `route` | Model-routed branch — the signature *bounded non-determinism* primitive. Run `selector` (typically a `!model` op) to produce a label, then run the `case` whose `label` it names. The cases are fixed and analyzer-validated: the model chooses *which* declared branch runs, never *what*. Falls back to `default` when the label matches no case (an error if `default` is empty). |
| `fallback` | Ordered "first that succeeds wins" selector: run each branch in `branches` in turn; the first that completes without error and yields a non-empty result wins and becomes the node's result. On a branch error (or empty result) the next is tried — so a *side-effecting* branch that returns empty will still fall through and the next branch also runs (attempts stream live, as in `try`/`retry`). If every branch errors, the last error propagates. Lighter than `try` for graceful degradation (cheap path → else expensive path). `bind` names the winning result. |
| `timeout` | Bound the wall-clock of a sub-flow: run `body` with a `ms` deadline. If it does not finish in time the node errors (an enclosing `try`/`retry` may catch it). A general reliability guard-rail you can wrap around anything. `bind` names the body's result. |
| `budget` | Cap the cost of a scope: run `body` but allow at most `limit` op dispatches within it (checked at statement boundaries; a nested statement can consume more than one dispatch before the next check). A first-class cost guard-rail; v1 counts dispatches (token/money budgets are a later refinement). `bind` names the body's result. |
| `cap_scope` | **Capability scope**: run `body`, but restrict op dispatch to the tool names in `tools` — a call to anything outside that allowlist fails closed at the runtime's dispatch gate, even when the outer session policy would allow it. Capabilities only ever narrow on descent: a nested `with_tools` is intersected with the scope it's nested in, so an inner block can never re-grant a tool an outer one removed. This is the runtime-enforced counterpart of an advisory tool restriction — the analyzer also flags a literal-op `call` here that provably names a tool absent from `tools` (a static echo of the same rule dispatch enforces dynamically). `bind` names the body's result. Native text: `with_tools ["read", "grep"]` + an indented body. |
| `scope` | RAII-style **acquire → use → release** with guaranteed cleanup. Optionally run `acquire` first (binding its result to `bind`, so `body` and `finally` can name the resource), then run `body`; `finally` **always** runs afterward — on normal completion, an early `return`, or an error — so a lock is freed / a handle closed / a temp removed no matter how the body exits. The body's result, `return`, or error then propagates; a `finally` failure surfaces only when the body itself succeeded (it never masks the body's own error). If `acquire` errors the resource was never taken, so `finally` does not run. The deterministic resource-lifecycle guard-rail (RAII for flows). |
| `saga` | Saga / **compensating transaction**: run each `step` in order; after a step's `body` succeeds, its `undo` is registered. If a *later* step fails, the runtime unwinds by running the registered `undo` bodies in **reverse** order (best-effort — an `undo` failure is recorded but does not stop the unwind), then propagates the original error. The strongest guard-rail for non-transactional external side effects (charge→refund, create→delete, reserve→release): partial work is rolled back rather than left dangling. A `return` inside a step is a successful early exit and does not compensate (use `scope` for guaranteed cleanup on every exit). |
| `once` | **At-most-once side effect** across re-runs — an effect-level `memo`. `label` is an explicit idempotency key: the first time the body runs to success in a session its result is recorded durably; later re-runs in the same session skip the body and reuse the stored result. A failed body records nothing and is retried. `bind` optionally names the body's result. Safety under re-execution (`send_email`/`charge` never fire twice). With no durable store wired (a throwaway interpreter) it degrades to running every time. Requires a non-empty literal label. |
| `checkpoint` | **Durable resume point** for long-running / resumable flows. A **top-level-only** marker (like `await`): the first time a run reaches it, the position is recorded durably; a later re-run of the *same* flow in the *same* session fast-forwards past the already-completed prefix (its symbols are still durably bound and its side effects are not repeated) and continues from here. `label` is a human-readable name for the phase it closes. Pairs with `once` for finer-grained idempotency; a no-op when no durable store is wired. Requires a non-empty literal label. |
| `obj` | Build an **object value** from sub-expressions — the record constructor `{ k: expr, … }`. Each field value is itself a node, so a record can mix literals and variables: `{ ok: true, n: $count, intent: $extract.intent }`. Pure: it assembles a value, performing no IO and no op dispatch. Leaves must be pure value nodes (`var`/`lit`/`jq`/`expr`/`fmt`/`obj`/ `list`); a call or control-flow leaf is rejected by the analyzer so templates stay side-effect free. This is what lets `return { … }` assemble a result from computed symbols. |
| `list` | Build a **list value** from sub-expressions — the list constructor `[ expr, … ]`. Each item is itself a node (`[ $a, $b, 3 ]`). Pure, same leaf rules as [`Node::Obj`]; the array twin of the record constructor. |
<!-- END generated:node-kinds -->

---

## Primitive and expression nodes

These produce a value without side effects and appear in argument position, conditions, and
`return` expressions — not as standalone statements.

### `lit`

A literal JSON value. String literals support `{symbol}` interpolation at evaluation time
(unbound tokens stay verbatim; interpolation recurses into strings inside arrays/objects).

```json
{"kind": "lit", "value": {"key": "val"}}
```

| field | type | required | description |
|---|---|---|---|
| `value` | any JSON | yes | the literal value |

### `var`

A reference to a bound symbol, resolved to its stored value. Text form: `$name`.

```json
{"kind": "var", "name": "draft"}
```

| field | type | required | description |
|---|---|---|---|
| `name` | string | yes | the symbol name (no leading `$`) |

An unbound symbol is a hard error at evaluation time.

### `thing`

A reference to an external object, resolved before execution begins. `@json`-only in text.

```json
{"kind": "thing", "thing": {"kind": "person", "selector": {"name": "John"}}}
```

| field | type | required | description |
|---|---|---|---|
| `thing.kind` | ThingKind | yes | `context` / `file` / `person` / `ticket` / `email` / `repo` / `dataset` / `calendar_event` / `url` / `secret` / custom |
| `thing.selector` | Selector | yes | `id` / `name` / `path` / `query` / `key` |

---

## Core statement nodes

### `call`

Invoke a registered operation. Arguments are named: a multi-param op takes a single object
argument; a sole-required-param op accepts one bare value. Text form: `op(args)` or
`do op args`.

```json
{"kind": "call", "op": "write", "args": [
  {"kind": "lit", "value": {"path": "out.txt", "content": "hi"}}
]}
```

| field | type | required | description |
|---|---|---|---|
| `op` | string | yes | the registered op name |
| `args` | Node[] | no | empty, one bare value, or one object naming each parameter (2+ bare values is rejected) |

Every `call` goes through the dispatch envelope; a standalone call's result is discarded from
the symbol table but still traced.

### `bind`

Store a call's result as a symbol. Text form: `$name = …` (optionally `$name: Type = …`,
optionally preceded by `@effect(tag)`).

```json
{"kind": "bind", "name": "draft",
 "value": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "README.md"}]}}
```

| field | type | required | description |
|---|---|---|---|
| `name` | string | yes | symbol to bind |
| `value` | Node | yes | the expression to evaluate |
| `ty` | TypeRef | no | type hint stored alongside the symbol |
| `effect` | FlowEffect | no | declared semantic effect (drives risk + approval) |

An errored evaluation aborts the flow — nothing is bound on error.

### `return`

End the flow immediately with a value, unwinding all enclosing blocks. Text form:
`return [expr]`.

```json
{"kind": "return", "value": {"kind": "var", "name": "draft"}}
```

| field | type | required | description |
|---|---|---|---|
| `value` | Node | yes | the flow's return value |

Rejected inside `parallel` branches.

---

## Control flow

### `when`

Conditional branch on truthiness. Text form: `when <cond>` / `else`.

```json
{"kind": "when", "cond": {"kind": "var", "name": "ok"},
 "then": [], "otherwise": []}
```

| field | type | required | description |
|---|---|---|---|
| `cond` | Node | yes | the condition |
| `then` | Node[] | no | body when truthy |
| `otherwise` | Node[] | no | body when falsey |

### `unless`

Negated conditional — sugar for `when !cond`, no else branch. Text form: `unless <cond>`.

| field | type | required | description |
|---|---|---|---|
| `cond` | Node | yes | body runs when this is falsey |
| `body` | Node[] | no | any nodes |

### `assert`

Abort with an error if the condition is falsey. Text form: `assert <cond>[, "message"]`.

```json
{"kind": "assert", "cond": {"kind": "var", "name": "hits"},
 "message": "grep returned no results"}
```

| field | type | required | description |
|---|---|---|---|
| `cond` | Node | yes | the guard condition |
| `message` | string | no | error detail shown on failure |

### `match`

Deterministic multi-way branch by JSON equality. Text form: `match $x` + `case <value>` /
`default` arms.

```json
{"kind": "match", "subject": {"kind": "var", "name": "status"},
 "cases": [{"value": {"kind": "lit", "value": "ok"}, "body": []}],
 "default": []}
```

| field | type | required | description |
|---|---|---|---|
| `subject` | Node | yes | literal or symbol reference |
| `cases` | MatchCase[] | yes | at least one `{value, body}` |
| `default` | Node[] | no | runs when no case matches (else: error) |

### `route`

Bounded model routing: a selector produces a label; the matching `case` runs. Text form:
`route <selector-call>` + `case "label"` / `default` arms.

```json
{"kind": "route",
 "selector": {"kind": "call", "op": "classify", "args": [{"kind": "var", "name": "ticket"}]},
 "cases": [{"label": "bug", "body": []}, {"label": "billing", "body": []}],
 "default": []}
```

| field | type | required | description |
|---|---|---|---|
| `selector` | Node | yes | node producing a label |
| `cases` | RouteCase[] | yes | unique, non-empty labels with bodies |
| `default` | Node[] | no | runs on an unknown label (else: error) |

### `fallback`

Ordered first-useful-success selector. Text form: `fallback -> $bind` + bare `branch` arms.

| field | type | required | description |
|---|---|---|---|
| `branches` | FallbackBranch[] | yes | ordered `{body}` branches |
| `bind` | string | no | symbol for the winning result |

A branch error or empty result falls through; all-empty keeps the first empty success;
all-errored propagates the last error.

---

## Iteration

### `repeat`

Bounded counter loop. Text form: `repeat N` with optional `until` as the first body line.

```json
{"kind": "repeat", "max": 5, "until": {"kind": "var", "name": "done"}, "body": []}
```

| field | type | required | description |
|---|---|---|---|
| `max` | u32 | yes | maximum iterations |
| `until` | Node | no | stop-when-true guard, checked after each iteration |
| `body` | Node[] | no | loop body |
| `collect` | string | no | symbol bound to the list of per-iteration results |

### `each`

List-driven loop. Text form: `each $x in $list [-> $collect | -> flat $collect]`.

```json
{"kind": "each", "in": {"kind": "var", "name": "files"}, "as": "f",
 "body": [], "collect": "contents"}
```

| field | type | required | description |
|---|---|---|---|
| `in` | Node | yes | expression yielding a list (anything else errors) |
| `as` | string | yes | element symbol per iteration |
| `body` | Node[] | no | per-element body |
| `collect` | string | no | symbol bound to the per-iteration results (empty list ⇒ `[]`) |

### `loop`

Time-bounded iteration. Text form: `loop for <ms> every <ms> [-> $bind]` with optional
`until` first body line.

```json
{"kind": "loop", "for_ms": 10000, "every_ms": 1000,
 "until": {"kind": "var", "name": "done"}, "bind": "last", "body": []}
```

| field | type | required | description |
|---|---|---|---|
| `for_ms` | u64 | yes | wall-clock deadline (ms) |
| `every_ms` | u64 | no | inter-iteration sleep (default 0 = tight) |
| `until` | Node | no | stop-when-true guard after each iteration |
| `body` | Node[] | no | loop body |
| `bind` | string | no | symbol for the last iteration's result |

A body error ends the loop immediately — use `retry` inside the body for per-iteration
resilience.

---

## Sequencing

### `seq`

Sequential block, optionally binding its final result. Text form: `seq [-> $bind]`.

| field | type | required | description |
|---|---|---|---|
| `body` | Node[] | no | statements to run in order |
| `bind` | string | no | symbol for the block's final result |

### `pipe`

Chain calls, each step's output fed as the next step's first argument. `@json`-only in text.

```json
{"kind": "pipe", "bind": "hits", "steps": [
  {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "log.txt"}]},
  {"kind": "call", "op": "grep", "args": [{"kind": "lit", "value": "ERROR"}]}
]}
```

| field | type | required | description |
|---|---|---|---|
| `steps` | Node[] (call) | no | pipeline steps |
| `bind` | string | no | symbol for the final step's result |

### `memo`

Like `bind`, but compute-once-per-session keyed on `(session, name)`. `@json`-only in text.

```json
{"kind": "memo", "name": "survey",
 "value": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "big.log"}]}}
```

| field | type | required | description |
|---|---|---|---|
| `name` | string | yes | symbol / cache key |
| `value` | Node (call) | yes | the call to run on a cache miss |
| `ty` | TypeRef | no | type hint |
| `effect` | FlowEffect | no | declared effect |

---

## Concurrency

### `parallel`

Concurrent fan-out; each branch's result binds to its name; results merge in declaration
order. Text form: `parallel` + `branch $name` arms. See [Concurrency](./concurrency.md).

| field | type | required | description |
|---|---|---|---|
| `branches` | Branch[] | yes | `{name, body}` — names unique; `return` and cross-branch binds rejected |

### `race`

First-success concurrency under a required deadline. `@json`-only in text. See
[Concurrency](./concurrency.md).

| field | type | required | description |
|---|---|---|---|
| `timeout_ms` | u64 | yes | wall-clock deadline |
| `branches` | Branch[] | yes | `{name, body}` run concurrently |
| `bind` | string | no | symbol for the winning result |

---

## Error handling and guards

### `try`

Run `body`; on failure bind the error string to `catch` and run `handler`. `@json`-only in
text. See [Reliability & guard rails](./reliability.md).

| field | type | required | description |
|---|---|---|---|
| `body` | Node[] | no | the guarded body |
| `catch` | string | no | symbol bound to the error string |
| `handler` | Node[] | no | runs only on failure; its own error propagates |

### `retry`

Retry a body on transient failure. Text form:
`retry <max> [backoff <strategy>] [delay <ms>] [-> $bind]`.

| field | type | required | description |
|---|---|---|---|
| `max` | u32 | yes | maximum attempts including the first |
| `backoff` | string | no | `"none"` (default) / `"linear"` / `"exponential"` |
| `delay_ms` | u64 | no | base delay in ms |
| `body` | Node[] | no | body to retry |
| `bind` | string | no | symbol for the successful result |

Fatal errors (policy denial, unknown op, type errors) and denied `confirm`s are never
retried.

### `verify`

Run a command node and assert its output contains a substring. `@json`-only in text.

| field | type | required | description |
|---|---|---|---|
| `cmd` | Node | yes | node producing a string |
| `expect` | Node | yes | substring the output must contain |
| `message` | string | no | error text on failure |

### `confirm`

Explicit human approval gate. `@json`-only in text. See
[Reliability & guard rails](./reliability.md).

| field | type | required | description |
|---|---|---|---|
| `message` | string | yes | what will happen |
| `risk` | string | no | `"low"` / `"medium"` (default) / `"high"` / `"critical"` |
| `body` | Node[] | no | runs only on approval; empty body = pure gate |

---

## Cost, rate, and capability control

### `timeout`

Wall-clock deadline on a body. Text form: `timeout <ms> [-> $bind]`.

| field | type | required | description |
|---|---|---|---|
| `ms` | u64 | yes | non-zero deadline |
| `body` | Node[] | no | body under the deadline |
| `bind` | string | no | symbol for the body's result |

### `budget`

Dispatch cap on a body, checked at statement boundaries. Text form: `budget <n> [-> $bind]`.

| field | type | required | description |
|---|---|---|---|
| `limit` | u32 | yes | non-zero max dispatch count |
| `body` | Node[] | no | body under the cap |
| `bind` | string | no | symbol for the body's result |

### `cap_scope`

Runtime-enforced tool allowlist for a body; nested scopes intersect. Text form:
`with_tools ["a", "b"] [-> $bind]`.

| field | type | required | description |
|---|---|---|---|
| `tools` | string[] | yes | allowed tool names |
| `body` | Node[] | no | body under the capability scope |
| `bind` | string | no | symbol for the body's result |

### `throttle`

At most `max` dispatches per sliding window, keyed per session by `name`; errors instead of
blocking. `@json`-only in text.

| field | type | required | description |
|---|---|---|---|
| `name` | string | yes | bucket key (survives across turns) |
| `max` | u32 | yes | max dispatches in the window |
| `window_ms` | u64 | yes | sliding window size |
| `body` | Node[] | no | the rate-limited body |

### `debounce`

Cross-turn coalescing: the body runs only after `wait_ms` of quiet for the key. `@json`-only
in text.

| field | type | required | description |
|---|---|---|---|
| `name` | string | yes | stable key per `(session, name)` |
| `wait_ms` | u64 | yes | settling window |
| `body` | Node[] | no | body to run once settled |

---

## Cross-turn and durability

### `peek`

Read a symbol's in-session value without IO (empty if unbound). `@json`-only in text.

| field | type | required | description |
|---|---|---|---|
| `name` | string | yes | symbol to look up (no leading `$`) |

### `await`

Suspend until an external event; resume binds the received value. Top-level only.
`@json`-only in text. See [Durability & cross-turn state](./durability.md).

| field | type | required | description |
|---|---|---|---|
| `source` | string | yes | event source identifier |
| `binding` | string | no | symbol for the received value |
| `as_type` | TypeRef | no | lenient coercion for the received value |

### `scope` / `saga` / `once` / `checkpoint`

The durability quartet — guaranteed cleanup, compensation, at-most-once effects, and durable
resume. All `@json`-only in text; semantics, examples, and field-by-field behavior are on
[Durability & cross-turn state](./durability.md). Field summary:

| kind | fields |
|---|---|
| `scope` | `acquire` (Node, optional), `bind` (string, optional), `body` (Node[]), `finally` (Node[]) |
| `saga` | `steps` (SagaStep[]) — each `{body, undo?}` |
| `once` | `label` (non-empty literal string), `body` (Node[]), `bind` (string, optional) |
| `checkpoint` | `label` (non-empty literal string) |

---

## Pure computation

### `expr` / `fmt` / `jq` / `parse`

The pure computation nodes — full semantics, whitelists, and examples on
[Pure data shaping](./pure-data.md). Field summary:

| kind | fields |
|---|---|
| `expr` | `formula` (string), `vars` (map name → `lit`/`var` node) — every formula variable must be declared |
| `fmt` | `template` (string with `{name}` placeholders) — text form `fmt("…")` |
| `jq` | `path` (dot-path string), `input` (`var`/`lit` node) — text sugar `$var.path` for dotted paths |
| `parse` | `value` (Node), `as` (`"f64"`/`"i64"`/`"bool"`/`"json"`/`"string"`) |

### `obj` / `list`

Pure value constructors — the record/list templates. Text form: `{ k: expr }` / `[ expr ]`
when not plain JSON.

```json
{"kind": "obj", "fields": {
  "ok": {"kind": "lit", "value": true},
  "count": {"kind": "var", "name": "n"}
}}
```

| kind | fields |
|---|---|
| `obj` | `fields` — map of field name → pure value node |
| `list` | `items` — ordered pure value nodes |

Leaves may only be `var`/`lit`/`jq`/`expr`/`fmt`/`obj`/`list`; effectful leaves are rejected.
As bare top-level statements they error — a value by itself is not executable.

### `ctx` / `ctx_append`

The context-pack nodes — full budget semantics on [Context packs](./context-packs.md). Text
forms: the `ctx $name` block and `$pack += $more`.

| kind | fields |
|---|---|
| `ctx` | `name` (string), `purpose` (string, optional), `include`/`exclude` (string[], optional), `budget` (u64, optional; `0` rejected) |
| `ctx_append` | `ctx` (string — the pack to extend), `add` (string[], optional) |

---

## Key invariants

- **Every op goes through the dispatch envelope** — policy, approval, and redaction are
  non-bypassable regardless of which node kind triggers the call.
- **`return` inside `parallel` is rejected** by the analyzer — bind inside the branch, read
  the symbol after the join.
- **`memo` is session-scoped** — the cache key is `(session, symbol)`; a new session always
  recomputes.
- **`retry` does not retry fatal errors** — policy denial, unknown op, and type errors
  propagate immediately.
- **`throttle` errors instead of blocking** — the plan stays responsive; wrap with `try` or
  `retry` to wait.
- **`debounce` coalesces per `name` across turns** — the settling window lives in the session
  store.
- **`race` picks the first *success*** within its deadline; all-failed is a joined error
  distinct from a timeout, and losing branches' dispatched steps remain counted and traced.
- **`await` and `checkpoint` are top-level only** — they need stable resume cursors.
- **`obj`/`list` are pure templates** — they cannot contain `call` or control-flow leaves.

## Related docs

- [Operations](./ops.md) — operation names a `call` node can target.
- [Types & effects](./types-and-effects.md) — annotations, effects, and prelude artifact types.
- [Execution model](./execution-model.md) — lifecycle and runtime behavior.
