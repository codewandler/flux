# Flux-Flow — Node Reference

This document covers every node kind in the Flux-Lang AST, from the primitive expression
nodes up through control-flow, concurrency, error-handling, and the timing/rate
primitives. Nodes are grouped from innermost (values / expressions) to outermost (full
statements and flow-level constructs).

---

## Top-level shape

A flow is a JSON object:

```json
{
  "name": "optional-name",
  "params": [{"name": "ticket", "ty": "Ticket"}],
  "returns": "Result",
  "body": [Node, ...]
}
```

`params` and `returns` are optional; `body` is the ordered list of statement nodes
the runtime executes top-to-bottom.

---

## Primitive / expression nodes

These produce a value without side effects and appear in argument position, conditions,
and `return` expressions. They are **not** executable as standalone statements.

### `lit`

A literal JSON value embedded directly in the AST.

```json
{"kind": "lit", "value": 42}
{"kind": "lit", "value": "hello"}
{"kind": "lit", "value": ["a", "b", "c"]}
{"kind": "lit", "value": {"key": "val"}}
```

String literals support `{{symbol}}` and `{symbol}` interpolation: any token that
matches a bound session symbol is replaced with the symbol's stored text at evaluation
time. Tokens whose name is unbound are left verbatim (no silent data loss).
Interpolation recurses into strings inside arrays and objects.

**Fields**

| field | type | required | description |
|---|---|---|---|
| `value` | any JSON | yes | the literal value |

---

### `var`

A reference to a symbol bound earlier in the same flow (or a prior turn for `memo`
symbols). The symbol is resolved to its stored `Value` and converted to natural JSON
before being passed to the op.

```json
{"kind": "var", "name": "draft"}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `name` | string | yes | the symbol name (no leading `$`) |

An unbound symbol is a hard error at evaluation time.

---

### `thing`

A reference to an external object that must be resolved before execution begins. The
runtime resolves it to a `ResolvedThing` (with `id`, `display`, `confidence`) before
any side-effecting node runs.

```json
{"kind": "thing", "thing": {"kind": "person", "selector": {"name": "John"}}}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `thing.kind` | ThingKind | yes | `context` / `file` / `person` / `ticket` / `email` / `repo` / `dataset` / `calendar_event` / `url` / `secret` / `custom(...)` |
| `thing.selector` | Selector | yes | `id` / `name` / `path` / `query` / `key` — how the thing is addressed |

---

## Core statement nodes

### `call`

Invoke a registered operation with positional argument expressions. Arguments are
mapped to the op's named JSON-Schema parameters in `required ++ optional` order. A
single object argument is passed straight through as the named input.

```json
{"kind": "call", "op": "read", "args": [
  {"kind": "lit", "value": "README.md"}
]}
```

Every `call` goes through `Executor::dispatch` (policy -> approval -> redaction) —
there is no bypass surface.

**Fields**

| field | type | required | description |
|---|---|---|---|
| `op` | string | yes | the registered op name (e.g. `read`, `bash`, `task`) |
| `args` | Node[] | no | positional argument expressions (`lit` / `var` only in statement position) |

A standalone `call` (not inside a `bind`) runs the op for its side effects; the result
is discarded from the symbol table but still appears in the transcript.

---

### `bind`

Run a `call` node and store its result as a session symbol.

```json
{"kind": "bind", "name": "draft",
 "value": {"kind": "call", "op": "echo", "args": [
   {"kind": "lit", "value": "hello"}
 ]}}
```

The symbol is visible to subsequent nodes in the same session via `var` and inside
`{{interpolation}}` in string literals. An errored call aborts the flow — nothing is
bound on error.

**Fields**

| field | type | required | description |
|---|---|---|---|
| `name` | string | yes | symbol to bind |
| `value` | Node (call) | yes | must be a `call` node |
| `ty` | TypeRef | no | optional type hint stored alongside the symbol |
| `effect` | FlowEffect | no | declared semantic effect (drives risk + approval) |

---

### `return`

End the flow immediately and carry a value back to the caller. Unwinds all enclosing
blocks (`when`, `repeat`, `seq`, ...). The expression may be a `var`, `lit`, or `call`.

```json
{"kind": "return", "value": {"kind": "var", "name": "draft"}}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `value` | Node | yes | `var` / `lit` / `call` — the flow's return value |

A `return` inside a `parallel` branch is rejected by the analyzer.

---

## Control-flow nodes

### `when`

Conditional branch. Evaluates `cond`; if truthy, runs `then`; otherwise runs
`otherwise` (which may be empty). Both branches may contain any statement nodes.

```json
{"kind": "when",
 "cond": {"kind": "lit", "value": true},
 "then": [
   {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "echo yes"}]}
 ],
 "otherwise": []}
```

Truthiness rules: `null`/`false`/`0`/`""`/`"false"`/`"0"` are falsey; a non-empty
string is truthy unless it equals `"false"` or `"0"` (so a tool's textual output reads
as expected).

**Fields**

| field | type | required | description |
|---|---|---|---|
| `cond` | Node | yes | `lit` / `var` / `call` — the condition |
| `then` | Node[] | no | body when condition is truthy |
| `otherwise` | Node[] | no | body when condition is falsey |

---

### `assert`

Abort the flow with an error if the condition is falsey. A lightweight guard that avoids
writing `when ... return Err(...)` manually.

```json
{"kind": "assert",
 "cond": {"kind": "var", "name": "hits"},
 "message": "grep returned no results"}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `cond` | Node | yes | `lit` / `var` / `call` |
| `message` | string | no | error detail shown on failure |

---

### `repeat`

Counter-driven bounded loop. `max` is required; the analyzer rejects unbounded loops.
The body runs up to `max` times; an `until` guard (evaluated *after* each iteration)
can break early.

Prefer `each` when iterating a known list; use `repeat` only for counter-driven work.

```json
{"kind": "repeat", "max": 5,
 "until": {"kind": "var", "name": "done"},
 "body": [
   {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "poll.sh"}]}
 ]}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `max` | u32 | yes | maximum iterations |
| `until` | Node | no | stop-when-true guard, checked after each iteration |
| `body` | Node[] | no | loop body |

---

### `each`

List-driven loop. Evaluates `in` to a `Value::List`; binds each element to `as` and
runs `body`. An optional `collect` symbol gathers the per-iteration results into a
`Value::List`.

```json
{"kind": "each",
 "in": {"kind": "lit", "value": ["a.rs", "b.rs"]},
 "as": "f",
 "body": [
   {"kind": "bind", "name": "text",
    "value": {"kind": "call", "op": "read", "args": [{"kind": "var", "name": "f"}]}}
 ],
 "collect": "contents"}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `in` | Node | yes | expression yielding a list |
| `as` | string | yes | element symbol bound on each iteration |
| `body` | Node[] | no | per-element body |
| `collect` | string | no | symbol bound to the list of per-iteration results |

`in` must evaluate to a list; any other type is a runtime error.

---

## Sequencing and grouping nodes

### `seq`

A sequential block. Runs `body` in order; optionally binds the block's final result.
Useful for scoping a sub-flow whose last value you want to name.

```json
{"kind": "seq",
 "body": [
   {"kind": "call", "op": "echo", "args": [{"kind": "lit", "value": "one"}]},
   {"kind": "bind", "name": "two",
    "value": {"kind": "call", "op": "echo", "args": [{"kind": "lit", "value": "two"}]}}
 ],
 "bind": "last"}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `body` | Node[] | no | body to run |
| `bind` | string | no | symbol to bind to the block's final result |

---

### `pipe`

Chain calls: each step's output is fed as the **first argument** of the next step.
Optionally binds the chain's final result. All steps must be `call` nodes.

```json
{"kind": "pipe", "bind": "hits", "steps": [
  {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "log.txt"}]},
  {"kind": "call", "op": "grep", "args": [{"kind": "lit", "value": "ERROR"}]}
]}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `steps` | Node[] (call) | no | pipeline steps; each receives the previous step's output as its first arg |
| `bind` | string | no | symbol to bind to the final step's result |

---

### `memo`

Like `bind`, but pinned across turns: if the symbol is already resolved for this
session, skip execution and reuse the cached value. Use this for expensive
deterministic work (large reads, slow grep, model calls) that should not re-run on
each turn.

```json
{"kind": "memo", "name": "survey",
 "value": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "big.log"}]}}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `name` | string | yes | symbol (cache key — scoped to session) |
| `value` | Node (call) | yes | must be a `call` node |
| `ty` | TypeRef | no | optional type hint |
| `effect` | FlowEffect | no | declared semantic effect |

A different session always recomputes; only the same `(session, name)` pair hits the
cache.

---

## Concurrency nodes

### `parallel`

Concurrent fan-out: run independent branches concurrently; bind each branch's final
result to its `name`. Each branch writes to a buffering sink; after the join, events
are replayed into the real sink in branch order (no interleaving).

```json
{"kind": "parallel", "branches": [
  {"name": "readme", "body": [
    {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "README.md"}]}]},
  {"name": "todos", "body": [
    {"kind": "call", "op": "grep", "args": [{"kind": "lit", "value": "TODO"}]}]}
]}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `branches` | Branch[] | yes | each `{name, body}` — name must be unique |

Constraints: branch names must be distinct (the analyzer rejects duplicates). `return`
inside a branch is rejected. Every op in every branch still goes through
`Executor::dispatch`.

---

### `race`

First-wins fallback: try branches in order, return as soon as one succeeds. If no
branch succeeds before `timeout_ms` elapses the node errors. `bind` names the symbol
that receives the winning branch's result.

```json
{"kind": "race", "timeout_ms": 5000, "bind": "result", "branches": [
  {"name": "fast", "body": [
    {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "fast-check.sh"}]}]},
  {"name": "slow", "body": [
    {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "slow-check.sh"}]}]}
]}
```

**Semantics:** branches are tried sequentially (in order); the first one that completes
without error wins. A failing branch is skipped (its error is swallowed) and the next
branch is tried — as long as the deadline has not passed. Branch names must be
distinct.

**Fields**

| field | type | required | description |
|---|---|---|---|
| `timeout_ms` | u64 | yes | wall-clock deadline in milliseconds |
| `branches` | Branch[] | yes | `{name, body}` tried in order |
| `bind` | string | no | symbol to bind to the winning branch's result |

---

## Error-handling nodes

### `try`

Structured error handling. Runs `body`; if it errors, binds the error string to
`catch` (optional) and runs `handler`. If `handler` also errors, that error
propagates. If `body` succeeds, `handler` is not run.

```json
{"kind": "try",
 "body": [
   {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "might-fail.sh"}]}
 ],
 "catch": "err",
 "handler": [
   {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "echo fallback"}]}
 ]}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `body` | Node[] | no | the guarded body |
| `catch` | string | no | symbol to bind the error string to (in handler scope) |
| `handler` | Node[] | no | error-handling body; runs only on failure |

---

### `retry`

Retry a body on transient failure. Fatal errors (policy denial, unknown op) are not
retried. `backoff` controls the inter-attempt delay strategy.

```json
{"kind": "retry", "max": 3, "backoff": "exponential", "delay_ms": 500, "bind": "out",
 "body": [
   {"kind": "bind", "name": "out",
    "value": {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "flaky.sh"}]}}
 ]}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `max` | u32 | yes | maximum attempts |
| `backoff` | string | no | `"none"` (default) / `"linear"` / `"exponential"` |
| `delay_ms` | u64 | no | base delay in ms (default 500) |
| `body` | Node[] | no | body to retry |
| `bind` | string | no | symbol to bind to the final successful result |

Backoff schedule (attempt index starts at 1 for the second attempt):
- `none` — always `delay_ms`
- `linear` — `delay_ms x attempt`
- `exponential` — `delay_ms x 2^(attempt-1)` (capped at 2^10)

After `max` failed attempts the node errors with the last error message.

---

## Human-in-the-loop node

### `confirm`

Explicit approval gate. Calls the session `Approver` (TUI modal, `--yes` auto-allow,
or the default interactive prompt) with the given message and risk level. The body
only runs if the user approves; on denial the node errors immediately.

```json
{"kind": "confirm",
 "message": "Delete all temporary files?",
 "risk": "high",
 "body": [
   {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "rm -rf tmp/"}]}
 ]}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `message` | string | yes | human-readable description of what will happen |
| `risk` | string | no | `"low"` / `"medium"` (default) / `"high"` / `"critical"` — shown to the approver |
| `body` | Node[] | no | body that runs only on approval |

The risk label is prepended to the message as `[{risk}] {message}` when the approver is
called, so the operator sees it clearly in the TUI or CLI.

---

## Timing and rate-limiting nodes

### `loop`

Time-bounded iteration. `for_ms` is required (the analyzer rejects unbounded loops).
The body runs repeatedly until the deadline expires or the `until` guard fires. An
optional `every_ms` sleep between iterations prevents tight-loop CPU spin.

```json
{"kind": "loop", "for_ms": 10000, "every_ms": 1000,
 "until": {"kind": "var", "name": "done"},
 "bind": "last",
 "body": [
   {"kind": "bind", "name": "done",
    "value": {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "check.sh"}]}}
 ]}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `for_ms` | u64 | yes | wall-clock deadline (milliseconds) |
| `every_ms` | u64 | no | inter-iteration sleep in ms (default 0 = tight) |
| `until` | Node | no | stop-when-true guard, checked after each iteration |
| `body` | Node[] | no | loop body |
| `bind` | string | no | symbol to bind to the last iteration's result |

If the body errors during an iteration the node errors immediately (no silent retry —
use `retry` inside the body for that).

---

### `throttle`

Rate-limit body execution: at most `max` dispatches per `window_ms` sliding window.
The token bucket is tracked in the session store; if the limit is exceeded the node
errors rather than blocking, so the plan stays responsive.

```json
{"kind": "throttle", "max": 5, "window_ms": 60000,
 "body": [
   {"kind": "call", "op": "web_fetch", "args": [{"kind": "var", "name": "url"}]}
 ]}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `max` | u32 | yes | maximum calls in the window |
| `window_ms` | u64 | yes | sliding window size in milliseconds |
| `body` | Node[] | no | the rate-limited body |

The bucket is keyed by `(session, max, window_ms, call-site)` so two `throttle` nodes
with the same parameters in the same session do not share a bucket.

---

### `debounce`

Coalesce rapid re-invocations: wait `wait_ms` after the node is reached before running
body. In a `loop` or watch context this means the body only executes once things have
settled. In a single sequential flow it acts as a fixed delay before the body.

```json
{"kind": "debounce", "wait_ms": 300,
 "body": [
   {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "rebuild.sh"}]}
 ]}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `wait_ms` | u64 | yes | settling delay in milliseconds |
| `body` | Node[] | no | body that runs after the delay |

---

## Guard nodes

### `unless`

Negated conditional: run `body` only when `cond` is falsey. Sugar for `when !cond`;
the body may contain **any** nodes — reads, writes, sub-agents, nested control flow.

```json
{"kind": "unless",
 "cond": {"kind": "var", "name": "already_done"},
 "body": [
   {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "make build"}]}
 ]}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `cond` | Node | yes | any expression — the body runs when this is falsey |
| `body` | Node[] | no | any nodes to run when condition is falsey |

Identical truthiness rules as `when` (see below). Prefer `unless` over `when` with an
empty `then` for readability.

---

### `verify`

Self-checking primitive: run `cmd` (any node that produces a string — typically a
`bash` call), then check that its output contains the `expect` pattern. If the check
fails the flow aborts with a structured error. Use this after edits or builds to
guard against silent failures.

```json
{"kind": "verify",
 "cmd": {"kind": "call", "op": "bash", "args": [{"kind": "lit", "value": "cargo build --workspace 2>&1"}]},
 "expect": {"kind": "lit", "value": "Compiling"},
 "message": "cargo build failed"}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `cmd` | Node | yes | any node producing a string; typically a `bash` call |
| `expect` | Node | yes | substring (or regex) the output must contain |
| `message` | string | no | human-readable error shown on failure |

The check is a substring/regex search in the command's output. If `expect` is not
found the flow aborts with `"verify failed: {message — expected '{pattern}', got '{output}'}"`.
Wrap `verify` in a `try` if you want to handle failure gracefully rather than aborting.

---

### `peek`

Read the current in-session value of a named symbol without any filesystem IO. Returns
the symbol's stored value if bound, or an empty string if not yet bound. Use with
`when`/`unless` to branch on whether prior work was already done in this session.

```json
{"kind": "peek", "name": "survey"}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `name` | string | yes | the symbol name to look up (no leading `$`) |

Pairs naturally with `unless` for "skip if already computed" patterns:

```json
{"kind": "unless",
 "cond": {"kind": "peek", "name": "survey"},
 "body": [
   {"kind": "bind", "name": "survey",
    "value": {"kind": "call", "op": "read", "args": [{"kind": "lit", "value": "big.log"}]}}
 ]}
```

Note: for caching expensive work **across turns**, prefer `memo` — it uses the session
store and survives turn boundaries. `peek` is for within-plan conditional checks.

---

## Cross-turn suspend node

### `await`

Pause the flow until an external event or input arrives, then resume with the received
value bound to `binding`. The runtime stores the flow state and resumes it on the next
appropriate turn.

```json
{"kind": "await", "source": "user_input", "binding": "reply"}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `source` | string | yes | the event source identifier |
| `binding` | string | no | symbol to bind the received value to |
| `as_type` | TypeRef | no | expected type of the received value |

> **Status:** `await` is defined in the AST and accepted by the analyzer, but
> cross-turn execution is not yet implemented. Emitting an `await` node in a plan
> currently returns a clear error. Full suspend/resume lands in a future slice.

---

## Pure expression nodes (no IO, no approval gate)

These three nodes are pure — they carry no `Effect`, bypass the `OpRegistry`, and
never pause for approval. Use them wherever you would otherwise shell out to
`bash` for arithmetic, string formatting, or JSON extraction.

### `expr`

Inline arithmetic over named variables. `formula` is a safe whitelist expression;
the runtime evaluates it with a tiny recursive-descent parser — no `eval`, no shell.

Supported operators and functions: `+`, `-`, `*`, `/`, `round(x, n)`, `abs(x)`,
`min(a, b)`, `max(a, b)`.

```json
{"kind": "expr",
 "formula": "price * 2",
 "vars": {"price": {"kind": "var", "name": "price"}}}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `formula` | string | yes | the arithmetic expression |
| `vars` | object | no | map of variable name → node (`Lit` or `Var`) |

Variables not listed in `vars` are resolved from the session symbol table.
Result is a `Value::Number` (or `Value::String` for string concatenation with `+`).

---

### `fmt`

Pure string interpolation. `template` uses `{name}` (or `{{name}}`) placeholders
substituted from bound session symbols — the same syntax as `Lit` string
interpolation, but as a first-class node so the intent is explicit.

```json
{"kind": "fmt", "template": "BTC: {price} | Double: {doubled}"}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `template` | string | yes | the template string with `{name}` placeholders |

Unbound placeholders are left verbatim (no silent data loss). Result is always
a `Value::String`.

---

### `jq`

Pure JSON path extraction. `path` is a dot-path string applied to the JSON
content of `input`. Supports dot-notation (`.bitcoin.usd`), array indexing
(`results[0].value`), and nested paths. No shell-out — parsed in-process.

```json
{"kind": "jq",
 "path": ".bitcoin.usd",
 "input": {"kind": "var", "name": "raw"}}
```

**Fields**

| field | type | required | description |
|---|---|---|---|
| `path` | string | yes | dot-path (e.g. `.bitcoin.usd`, `results[0].value`) |
| `input` | Node | yes | any node producing a JSON string or object (`Var` or `Lit`) |

The extracted value is returned as the natural JSON type (`Number`, `String`,
`Bool`, etc.). If the path does not exist the node errors.

**The full BTC-double pattern in 4 nodes, 0 bash calls:**

```json
{"body": [
  {"kind": "bind", "name": "raw",
   "value": {"kind": "call", "op": "web_fetch",
     "args": [{"kind": "lit", "value": "https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd"}]}},
  {"kind": "bind", "name": "price",
   "value": {"kind": "jq", "path": ".bitcoin.usd",
     "input": {"kind": "var", "name": "raw"}}},
  {"kind": "bind", "name": "doubled",
   "value": {"kind": "expr", "formula": "price * 2",
     "vars": {"price": {"kind": "var", "name": "price"}}}},
  {"kind": "fmt", "template": "BTC: {price} | Double: {doubled}"}
]}
```

---

## Registered ops quick reference

Ops are passed by name to `call`. Arguments are positional in the order shown;
optional arguments are in `[brackets]`.

| op | signature | risk | description |
|---|---|---|---|
| `read` | `path[, limit, offset]` | Low | Read one file (string path), a list of files (JSON array), or a glob pattern (string with `*`/`?`). Single-file: line-numbered view, paging via `offset`/`limit`. Multi-file/glob: sections headed `==> path <==`. Guidance returned for over-cap files. |
| `grep` | `pattern[, glob, literal, max_results, path]` | Low | Search by regex (supports `\b`, lookaheads); use `literal: true` for plain substring |
| `glob` | `pattern[, path]` | Low | List files matching a glob pattern (`*` crosses `/`) |
| `search` | `query[, limit]` | Low | Search the indexed datasource |
| `web_fetch` | `url` | Low | Fetch an HTTP(S) URL |
| `write` | `path, content` | Medium | Write (create/overwrite) a file |
| `edit` | `path, old_string, new_string[, replace_all]` | Medium | Replace a string in a file (must match exactly once unless `replace_all`); if the exact text isn't found, progressively looser matching is tried (trailing whitespace → indentation drift → first/last-line anchor) and the result reports which strategy matched |
| `patch` | `path, edits` | Medium | Apply several line-anchored edits in one call; each edit is `{op, line, end_line?, text?}` where op is `insert_before`, `insert_after`, `replace_range`, or `delete_range`; ALL line numbers refer to the original file |
| `append` | `path, content` | Low | Append to a file (creates it and parent dirs if absent); lower-risk than `write` |
| `read_many` | `paths` | Low | Read several files at once (each section headed `==> path <==`); prefer single `read` when you need to embed a file's text into a later string |
| `task` | `role, task` | Medium | Delegate to a sub-agent role |
| `bash` | `command[, timeout_secs]` | High | Run a shell command |
| `file_stat` | `path` | Low | File metadata: size, line count, mtime (replaces `wc -l`, `stat`, `ls -la`) |
| `path_exists` | `path` | Low | Returns `"true"`/`"false"` — use with `when`/`unless` to branch on file presence |
| `sqlite_query` | `db, sql[, params]` | Low | Read-only SQLite query (SELECT/PRAGMA only) |
| `web_search` | `query[, max_results]` | Low | Tavily web search — requires `TAVILY_API_KEY` env var |
| `cargo_check` | `[package, args]` | Medium | `cargo check` (type-check only, no codegen) |
| `cargo_build` | `[package, release, args]` | Medium | `cargo build` |
| `cargo_test` | `[package, filter, args]` | Medium | `cargo test` |
| `cargo_clippy` | `[package, args]` | Medium | `cargo clippy` |
| `cargo_fmt` | `[package, check]` | Medium | `cargo fmt` (pass `check: true` to only verify) |
| `git_stage` | `paths` | Medium | Stage files (`git add`) |
| `git_commit` | `message[, body]` | Medium | Create a commit |
| `git_status` | | Low | Working tree status |
| `git_diff` | `[path, staged]` | Low | Show unstaged (or staged) diff |
| `git_log` | `[limit]` | Low | Recent commits |
| `git_push` | `[branch, remote]` | Medium | Push to remote |
| `git_checkout` | `branch[, create]` | Medium | Switch/create branch |
| `git_unstage` | `paths` | Low | Unstage files |

`write`, `edit`, `patch`, `append`, `task`, `bash`, and the `cargo_*` ops may pause for user approval
(controlled by the safety envelope and the active permission rules).

---

## Type system quick reference

`TypeRef` is the set of types the analyzer checks op signatures against:

| tag | meaning |
|---|---|
| `any` | top type — matches anything (pre-inference) |
| `bool` | boolean |
| `number` | 64-bit float |
| `string` | UTF-8 string |
| `list` | homogeneous list — `List<String>`, etc. |
| `named(X)` | a named/registered type — `Ticket`, `Draft`, `Result`, ... |

`FlowEffect` is the semantic effect declared on a `bind` or `memo` node. It drives
risk scoring and approval decisions:

| tag | meaning |
|---|---|
| `pure` | side-effect free |
| `read` | reads external state |
| `model` | invokes an LLM (non-deterministic) |
| `network` | general network egress |
| `write_file` | writes to the filesystem |
| `write_db` | writes to a database |
| `send_external` | sends email / message / webhook |
| `delete` | irreversibly deletes |
| `money` | moves money |
| `calendar` | mutates a calendar |
| `human_visible` | produces output a human will see |

---

## Truthiness rules

All condition nodes (`when.cond`, `assert.cond`, `repeat.until`, `loop.until`) use the
same JSON truthiness:

| value | truthy? |
|---|---|
| `null` | false |
| `false` | false |
| `0` | false |
| `""` | false |
| `"false"` | false |
| `"0"` | false |
| empty array `[]` | false |
| empty object `{}` | false |
| anything else | true |

A tool that returns the string `"false"` is therefore treated as falsey — so `when`
gates on a shell exit-code wrapper or a boolean tool output work as expected.

---

## Key invariants

- **Every op goes through `Executor::dispatch`** — policy, approval, and redaction are
  non-bypassable regardless of which node kind triggers the call.
- **`return` inside `parallel` is rejected** by the analyzer. Use `bind` inside the
  branch and read the bound symbol after the `parallel` node.
- **`memo` is session-scoped** — the cache key is `(session_id, symbol_name)`. A new
  session always recomputes.
- **`retry` does not retry fatal errors** — policy denial, unknown op, and type errors
  propagate immediately.
- **`throttle` errors instead of blocking** — if the rate limit is exceeded the node
  returns an error; the plan remains responsive and the caller can wrap with `try` or
  `retry`.
- **`debounce` in a single sequential flow is a fixed delay**, not a true event-driven
  debounce. Combine with `loop` to get settling semantics.
- **`race` picks the first *succeeding* branch**, not the fastest one — it is a
  sequential fallback with a deadline, not true concurrent fan-out. Use `parallel` if
  you want all branches to run concurrently.
