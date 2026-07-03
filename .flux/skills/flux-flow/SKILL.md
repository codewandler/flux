---
description: How to write Flux-Lang flow ASTs (the planning language used by flux-flow)
triggers: [flux-flow, flux-lang, emit_plan, ast, plan, flow, dag]
---

# Flux-Lang — writing flow ASTs

Flux-Lang is the planning language for this project. The LLM has no directly-callable tools; its only action is `emit_plan`, which submits a JSON AST that the runtime executes. Everything — reads, writes, bash, grep — must be a node in that graph.

## Top-level shape

```json
{"name": "optional-name", "body": [Node, ...]}
```

## Node kinds

<!-- Generated from flux-lang's `Node` doc-comments. Do not edit by hand; regenerate with `UPDATE=1 cargo test -p flux-flow --test skill_docs_in_sync`. -->
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
| `cap_scope` | **Capability scope**: run `body`, but restrict op dispatch to the tool names in `tools` — a call to anything outside that allowlist fails closed at the runtime's dispatch gate, even when the outer session policy would allow it. Capabilities only ever narrow on descent: a nested `with_tools` is intersected with the scope it's nested in, so an inner block can never re-grant a tool an outer one removed. This is the runtime-enforced counterpart of an advisory tool restriction — the analyzer also flags a literal-op `call` here that provably names a tool absent from `tools` (a static echo of the same rule dispatch enforces dynamically). `bind` names the body's result. Native text: `with_tools ["read", "grep"]` + an indented body. |
| `scope` | RAII-style **acquire → use → release** with guaranteed cleanup. Optionally run `acquire` first (binding its result to `bind`, so `body` and `finally` can name the resource), then run `body`; `finally` **always** runs afterward — on normal completion, an early `return`, or an error — so a lock is freed / a handle closed / a temp removed no matter how the body exits. The body's result, `return`, or error then propagates; a `finally` failure surfaces only when the body itself succeeded (it never masks the body's own error). If `acquire` errors the resource was never taken, so `finally` does not run. The deterministic resource-lifecycle guard-rail (RAII for flows). |
| `saga` | Saga / **compensating transaction**: run each `step` in order; after a step's `body` succeeds, its `undo` is registered. If a *later* step fails, the runtime unwinds by running the registered `undo` bodies in **reverse** order (best-effort — an `undo` failure is recorded but does not stop the unwind), then propagates the original error. The strongest guard-rail for non-transactional external side effects (charge→refund, create→delete, reserve→release): partial work is rolled back rather than left dangling. A `return` inside a step is a successful early exit and does not compensate (use `scope` for guaranteed cleanup on every exit). |
| `once` | **At-most-once side effect** across re-runs — an effect-level `memo`. `label` is an explicit idempotency key: the first time the body runs to success in a session its result is recorded durably; later re-runs in the same session skip the body and reuse the stored result. A failed body records nothing and is retried. `bind` optionally names the body's result. Safety under re-execution (`send_email`/`charge` never fire twice). With no durable store wired (a throwaway interpreter) it degrades to running every time. Requires a non-empty literal label. |
| `checkpoint` | **Durable resume point** for long-running / resumable flows. A **top-level-only** marker (like `await`): the first time a run reaches it, the position is recorded durably; a later re-run of the *same* flow in the *same* session fast-forwards past the already-completed prefix (its symbols are still durably bound and its side effects are not repeated) and continues from here. `label` is a human-readable name for the phase it closes. Pairs with `once` for finer-grained idempotency; a no-op when no durable store is wired. Requires a non-empty literal label. |
| `obj` | Build an **object value** from sub-expressions — the record constructor `{ k: expr, … }`. Each field value is itself a node, so a record can mix literals and variables: `{ ok: true, n: $count, intent: $extract.intent }`. Pure: it assembles a value, performing no IO and no op dispatch. Leaves must be pure value nodes (`var`/`lit`/`jq`/`expr`/`fmt`/`obj`/ `list`); a call or control-flow leaf is rejected by the analyzer so templates stay side-effect free. This is what lets `return { … }` assemble a result from computed symbols. |
| `list` | Build a **list value** from sub-expressions — the list constructor `[ expr, … ]`. Each item is itself a node (`[ $a, $b, 3 ]`). Pure, same leaf rules as [`Node::Obj`]; the array twin of the record constructor. |
<!-- END generated:node-kinds -->

## Registered ops (positional args in order shown)

| op | signature | risk |
|---|---|---|
| `read` | `path[, limit, offset]` | Low |
| `grep` | `pattern[, glob, max_results, path]` | Low |
| `glob` | `pattern[, path]` | Low |
| `search` | `query[, limit]` | Low |
| `web_fetch` | `url` | Low |
| `write` | `path, content` | Medium |
| `edit` | `path, old_string, new_string[, replace_all]` | Medium |
| `task` | `role, task` | Medium |
| `op.register` | `source, scope[, replace, expose]` | Medium |
| `bash` | `command[, timeout_secs]` | High |
| `proc.run` | `program[, args, timeout_secs]` | High |

`bash` and `proc.run` are generic process escape hatches and are **off by default** — they are only in the catalog when the
`shell` group is opted in (config `enable_shell = true`, `FLUX_ENABLE_BASH=1`, or the `/shell` REPL
toggle). Prefer the dedicated ops: `now`/`cwd`/`sys_info` (date/pwd/uname), `git_*`, the
`cargo_*`/`go_*`/`python_run`/`pytest`/`npm`/`node_run`/`make` toolchains, and the pure
`expr`/`jq`/`fmt` + `len`/`first`/`last`/`filter` ops. `write`, `op.register`, `bash`, and `proc.run`
may pause for user approval (guarded by the safety envelope).

`read` accepts three shapes for `path`:
- a plain string → reads that one file (line-numbered; paging with offset/limit)
- a string with `*` or `?` → auto-expanded as a glob, reads all matched files
- a JSON array of strings → reads each file; sections headed `==> path <==`

Prefer `read` over `read_many` (kept as a backward-compat alias).

## Key rules

- Control flow must be `repeat`/`when` nodes — never `for`/`if`/`&&` inside a shell string.
- Conditions can be a pure `expr` (`x == 2`, `len(s) > 0 && done`) — no `bash` needed for boolean logic.
- `bash` is opt-in and a last resort; when enabled, one `bash` op is ONE discrete command — not a shell script.
- Bound symbols are referenced with `{"kind":"var","name":"sym"}`, not string interpolation inside args.
- Every op runs through `Executor::dispatch` (policy → approval → redaction). Never try to bypass it.
- Re-use session symbols (`$name`) with `var` nodes instead of re-fetching the same file.
- `edit` requires `old_string` to appear EXACTLY ONCE in the file (or set `replace_all`).

## Minimal examples

**Read then grep:**
```json
{"body": [
  {"kind": "bind", "name": "src",
   "value": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "README.md"}]}},
  {"kind": "bind", "name": "hits",
   "value": {"kind": "call", "op": "grep",
     "args": [{"kind": "lit", "value": "TODO"}, {"kind": "lit", "value": "*.rs"}]}}
]}
```

**Loop (repeat) — append a line 3 times:**
```json
{"body": [
  {"kind": "repeat", "max": 3, "body": [
    {"kind": "call", "op": "append", "args": [{"kind": "lit", "value": "log.txt"}, {"kind": "lit", "value": "tick\n"}]}
  ]}
]}
```

**Conditional (when) — branch on file presence, no bash:**
```json
{"body": [
  {"kind": "bind", "name": "exists",
   "value": {"kind": "call", "op": "path_exists", "args": [{"kind": "lit", "value": "Cargo.toml"}]}},
  {"kind": "when",
   "cond": {"kind": "var", "name": "exists"},
   "then":      [{"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "Cargo.toml"}]}],
   "otherwise": [{"kind": "return", "value": {"kind": "lit", "value": "no manifest"}}]}
]}
```

**Delegate to sub-agent:**
```json
{"body": [
  {"kind": "bind", "name": "hits",
   "value": {"kind": "call", "op": "grep", "args": [{"kind": "lit", "value": "TODO"}]}},
  {"kind": "call", "op": "task",
   "args": [
     {"kind": "lit", "value": "worker"},
     {"kind": "lit", "value": "Summarize these TODOs: {{hits}}"}
   ]}
]}
```

**Iterate a list (each) — read several files, collecting results:**
```json
{"body": [
  {"kind": "each",
   "in": {"kind": "lit", "value": ["a.rs", "b.rs", "c.rs"]},
   "as": "f",
   "body": [
     {"kind": "bind", "name": "text",
      "value": {"kind": "call", "op": "read", "args": [{"kind": "var", "name": "f"}]}}
   ],
   "collect": "all"}
]}
```
Prefer `each` over `repeat` when iterating a known list. `repeat` stays for counter-driven loops.

**Run independent work concurrently (parallel):**
```json
{"body": [
  {"kind": "parallel", "branches": [
    {"name": "readme", "body": [
      {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "README.md"}]}]},
    {"name": "todos", "body": [
      {"kind": "call", "op": "grep", "args": [{"kind": "lit", "value": "TODO"}]}]}
  ]}
]}
```
Each branch binds its result to its `$name`. Use distinct names; do not `return` inside a branch.

**Chain (pipe) and guard (assert):**
```json
{"body": [
  {"kind": "pipe", "bind": "hits", "steps": [
    {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "log.txt"}]},
    {"kind": "call", "op": "grep", "args": [{"kind": "lit", "value": "ERROR"}]}
  ]},
  {"kind": "assert", "cond": {"kind": "var", "name": "hits"}, "message": "no errors found"}
]}
```
In a `pipe`, each step's output becomes the next step's first argument automatically.

## What's planned but not yet implemented

- `await` node execution (cross-turn suspend/resume)
- Op packs for L5 capabilities (browser, datasource/RAG)
- `!model` ops (LLM-as-a-node inside a plan)
