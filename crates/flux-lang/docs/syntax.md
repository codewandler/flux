# Flux-Lang — Language Design

This document is the authoritative specification for the Flux-Lang text syntax: the
human-writable, editor-friendly form of a flow. It covers grammar, every node kind,
the relationship to the JSON wire format, and the toolchain.

---

## Implementation status (read first)

The text syntax **is built**: `flux_lang::parse::parse(&str) -> Result<DraftAst>` (one flow) and
`flux_lang::format::format(&DraftAst) -> String`, with `parse(format(ast)) == ast` for **every**
`DraftAst`. But `format`/`parse` give a **native** surface to a *subset* of node kinds and fall back to a
single-line `@json <compact-json>` escape for the rest — so the round-trip always holds while the
hand-written grammar stays small.

- **Native text** (markers `=` bind · `do <op> <args>` effectful call · `+=` `ctx_append`): `bind`,
  `call` (bare `do …` or inline `op(…)`), `var` (`$x`), `lit` (JSON), `return`, `when`/`else`, `unless`,
  `each`, `repeat`, `seq`, and the context-pack nodes **`ctx`** / **`ctx_append`**. Flow header carries
  optional `name`/`params`/`returns`; a leading `goal "…"` line is accepted and ignored (not part of the
  AST/round-trip).
- **`@json` escape** (everything else, today): `parallel`, `race`, `try`, `retry`, `confirm`, `loop`,
  `throttle`, `debounce`, `assert`, `verify`, `pipe`, `memo`, `await`, `peek`, `thing`, `expr`, `fmt`,
  `jq`, `parse`, and the Tier-1 control-flow nodes `match`, `route`, `fallback`, `timeout`, `budget`
  (built P6b; no native grammar yet — they round-trip through `@json`).
- **Aspirational** (described below as the *target* language, **not** yet parsed): multiple flows per
  file, file-scope `type`/union declarations, and the `block`/`watch` spellings (the implemented nodes
  are `seq` and `loop`). The AST type is **`DraftAst`** (this doc historically said `FlowAst`, which does
  not exist).

---

## Motivation and scope

Flux-Lang exists at two levels:

- **Wire / storage format** — JSON (`FlowAst` via serde). Used by `emit_plan`, stored
  in `.flux/flows/`, passed between agent turns. Not meant to be hand-written.
- **Text format** — `.flux` files. The human-writable, version-controllable surface.
  This document specifies the text format.

The two formats are semantically identical: every `.flux` file compiles to exactly the
same `FlowAst` that the JSON wire format expresses. The text format adds nothing that
the JSON format cannot represent; it only makes flows readable and writable by humans.

The `render.rs` terminal display (box-drawing tree) is a third, separate thing: it is
read-only output for inspection, not a format you write.

---

## File structure

A `.flux` file is a sequence of one or more named flow definitions. The `flow` header
is **always required** — even for single-flow files. This keeps the format unambiguous
for parsers and formatters, and means any `.flux` snippet is valid in a multi-flow file
without modification.

```flux
# single-flow file
flow check-readme
  $content = read("README.md")
  return $content
```

```flux
# multi-flow file
flow fetch-and-grep
  $content = read("README.md")
  $hits    = grep("TODO", glob: "*.rs")
  return $hits

flow summarise(text: String) -> String
  $summary = task("summariser", "Summarise:\n{text}")
  return $summary
```

Flows are separated by one or more blank lines. Comments between flows are allowed.

### Flow header

```
flow <name> [( <param>, ... )] [-> <type>]
```

- `<name>` — identifier, `snake-case` or `snake_case` by convention
- `(<param>, ...)` — optional comma-separated parameter list; each param is
  `name: Type` (no `$` prefix on parameters — they are declarations, not references)
- `-> <type>` — optional return type annotation

Examples:

```flux
flow check-ci
flow build-report(repo: String, branch: String) -> String
flow poll-until-done(url: String, timeout_ms: Number) -> Bool
```

---

## Indentation

Indentation is **2 spaces** per level. The formatter always writes 2 spaces; the parser
accepts 2 or 4 spaces consistently within a file. Tabs are rejected. There are no
braces and no semicolons.

A block ends when the next non-blank line returns to the parent indentation level.
This is the only block-termination rule — there are no `end` keywords.

### else / catch at same indent as their opener

`else` is always at the same indentation level as its matching `when`:

```flux
when $ok
  bash("echo yes")
else
  bash("echo no")
```

Nested `when`: the `else` belongs to whichever `when` is at the same indent level:

```flux
when $a
  when $b
    bash("both true")
  else
    bash("a true, b false")   # else of inner when
else
  bash("a false")             # else of outer when
```

The same rule applies to `catch` relative to `try`.

---

## Symbols

All runtime values live in named symbols. A symbol reference is written `$name`
(lowercase, underscores allowed). The `$` sigil is mandatory on every symbol
*reference* in the body — it is the unambiguous signal that something is a runtime
value, not a keyword or op name.

```flux
$result = read("some/file.txt")   # bind: $result now holds the file contents
return $result                    # reference: pass $result to return
```

Symbols are immutable once bound within a single execution path. Rebinding the same
name in a different branch is allowed (the branches are independent paths).

**Parameters** are declared without `$` in the flow header (`name: Type`) but
referenced with `$` in the body (`$name`). This mirrors every mainstream language:
you declare `fn f(x: i32)` but write `x + 1` in the body — Flux-Lang uses `$x` in the
body to keep symbols visually distinct from keywords.

---

## Comments

Line comments start with `#` and run to end of line. Block comments are not supported.

```flux
# this is a comment
$x = read("a.txt")   # inline comment
```

`#` has no special meaning inside string literals (single- or triple-quoted).
A `#` comment is not permitted on the opening `"""` line of a multi-line string.

---

## Literals

| Kind | Syntax | Example |
|---|---|---|
| String | double-quoted | `"hello"` |
| Multi-line string | triple-quoted | `"""..."""` |
| Number | bare numeric | `42`, `3.14` |
| Bool | bare keyword | `true`, `false` |
| Null | bare keyword | `null` |
| Array | `[val, val, ...]` | `["a", "b", "c"]` |
| Object | `{key: val, ...}` | `{adapter: "local", trials: 3}` |

Object literals in expression position (e.g. as a call argument) use `{key: val}`
syntax. Inside a call argument list, `{` always starts an object literal, never a
block. Blocks are only introduced by flow-control keywords on their own line.

### String interpolation

Any string literal may embed `{symbol}` placeholders. The runtime substitutes the
symbol's current value at execution time:

```flux
$msg    = "built {sha} in {elapsed}ms"
$prompt = "Summarise this:\n{content}"
```

To emit a literal brace in output, double it: `{{` produces `{`, `}}` produces `}`:

```flux
$example = "use {{key: value}} syntax"   # outputs: use {key: value} syntax
```

The JSON wire format uses the same `{sym}` / `{{` / `}}` convention inside string
values — there is no difference between the text and wire formats for interpolation.

### Multi-line strings

Triple-quoted strings strip the common leading indent from all non-empty lines:

```flux
$prompt = """
  Analyse this diff and suggest improvements.
  Focus on correctness, not style.

  Diff:
  {diff}
"""
```

The `"""` opening token must be the last non-whitespace on its line. `#` inside a
triple-quoted string is literal text, not a comment.

### Inline object literals

Multi-line object literals are allowed inside call arguments:

```flux
$result = eval_run({
  adapter:       "terminal-bench",
  tasks:         ["chess-best-move"],
  trials:        1,
  agent_timeout: 180
})
```

The closing `)` may appear on its own line. Contents are indented 2 spaces deeper
than the call.

---

## Calls and binds

### Bare call (result discarded)

```flux
git_stage(["."])
git_commit("chore: bump version")
```

### Bind (result stored)

```flux
$hits    = grep("TODO", glob: "*.rs")
$content = read("README.md")
```

### Named arguments

All named arguments — for op calls **and** for flow-control node parameters — use
`key: value` syntax. There is exactly one convention.

Positional arguments come first; named arguments follow:

```flux
$hits = grep("ERROR", glob: "*.log", max_results: 50)
$page = read("large.txt", limit: 100, offset: 200)
```

Flow-control nodes use the same `key: value` form:

```flux
retry 3, backoff: exponential, delay: 500
watch for: 10000, every: 1000
race timeout: 5000 -> $result
```

### Memo (cross-turn cache)

A `memo` bind is computed once per session. On subsequent turns the cached value is
reused without re-executing the op:

```flux
memo $schema = read("schema.sql")
```

The `memo` keyword is the only form — there is no `@memo` annotation alternative.

---

## Effect annotations

An optional `@effect(name)` annotation precedes the statement it annotates:

```flux
@effect(send_external)
$report = generate_pdf($data)

@effect(delete)
bash("rm -rf tmp/")
```

Valid effects: `pure`, `read`, `model`, `network`, `write_file`, `write_db`,
`send_external`, `delete`, `money`, `calendar`, `human_visible`.

The `@` prefix is unambiguous: it introduces an annotation or a thing-reference and
is never used as an operator.

---

## Control flow

### when / else

```flux
when $ok
  bash("echo yes")
else
  bash("echo no")
```

The condition is any expression — a symbol, a call, or a bool literal. Calling an op
directly as the condition (without binding first) is valid:

```flux
when fetch_status($url)
  bash("echo up")
```

The `else` branch is optional.

### unless

Sugar for `when !cond`. Use for guard clauses:

```flux
unless $already_built
  bash("cargo build")
```

`unless` does not support an `else` branch. Use `when` if you need one.

### repeat

Counter-driven bounded loop. The count is required.

```flux
repeat 5
  bash("poll.sh")
```

With an early-exit condition. `until` is written on its own line as the **first**
statement of the body; it is evaluated before each iteration:

```flux
repeat 10
  until $done
  $done = bash("poll.sh")
```

### each

List-driven loop. Prefer over `repeat` when iterating a known list.

```flux
each $f in $files
  $text = read($f)
```

Collecting results — the result of each iteration is the value of the last expression
in the body. If the source list is empty, `$collect` is bound to `[]`:

```flux
each $f in $files -> $results
  read($f)
```

Flattened collect — each iteration yields a list; they are concatenated:

```flux
each $dir in $dirs -> flat $all_files
  glob("*.rs", path: $dir)
```

### watch

Time-bounded iteration. `loop` is a keyword in Rust and many shells; `watch` is the
Flux-Lang equivalent. `for` (milliseconds) is required.

```flux
watch for: 10000, every: 1000
  $done = bash("check-ready.sh")
```

With early exit and result capture:

```flux
watch for: 30000, every: 2000, until: $done -> $last
  $done = bash("health-check.sh")
```

Argument order: `for`, `every`, `until` (all named), then `-> $name` (optional result
capture after the argument list). `for` is the only required argument.

---

## Sequencing and piping

### block

A sequential block that optionally binds its final result. (`seq` was the earlier
name; `block` reads more naturally and avoids confusion with the Unix `seq` command.)

```flux
block -> $result
  bash("echo one")
  $two = bash("echo two")
```

A `block` with no result binding is valid:

```flux
block
  git_stage(["."])
  git_commit("chore: update")
```

### pipe

Each step's output is passed as the first argument of the next step. The final
step's output is the pipe's result. Named result (`-> $name`) is optional.

```flux
pipe -> $hits
  read("log.txt")
  grep("ERROR")
```

A `pipe` with a single step is valid (equivalent to a bare call). A `pipe` with zero
steps is a parse error.

---

## Concurrency

### parallel

Run independent branches concurrently. Each branch is introduced by `$name:` at one
indent level, with the branch body indented one level further.

The **result of a branch** is the value of the last expression evaluated in its body.
After the `parallel` block, each branch name is a bound symbol.

```flux
parallel
  $readme:
    read("README.md")
  $todos:
    grep("TODO")
```

After this block, `$readme` holds the file contents and `$todos` holds the grep hits.

A symbol bound *inside* a branch body (other than its implicit result) is not
visible outside that branch. A `parallel` with one branch is valid (degenerates to a
sequential bind). A `parallel` with zero branches is a parse error.

### race

Run branches concurrently; return as soon as the first succeeds. `timeout`
(milliseconds) is required. If no branch completes within the timeout, the node errors.

```flux
race timeout: 5000 -> $result
  $fast:
    bash("fast-path.sh")
  $slow:
    bash("slow-path.sh")
```

Branch syntax is identical to `parallel`. `-> $result` binds the winning value.

---

## Error handling

### try / catch

```flux
try
  bash("might-fail.sh")
catch $err
  bash("echo fallback: {err}")
```

- `catch $name` binds the error message string to `$name`
- `catch` is optional; a bare `try` with no `catch` suppresses errors silently
- If the handler also errors, that error propagates

### retry

Retry the body on failure up to `max` times.

```flux
retry 3, backoff: exponential, delay: 500 -> $out
  bash("flaky.sh")
```

- `max` (positional, required) — maximum attempts including the first
- `backoff: none | linear | exponential` — default `none`
- `delay: <ms>` — base delay in milliseconds, default `0`
- `-> $name` — binds the last expression of the body on success
- Fatal errors (policy denial, unknown op) are never retried
- A denied `confirm` inside a `retry` body is **not** retried

Do not also bind the result inside the body — the header binding captures it:

```flux
# correct: header binding
retry 3 -> $out
  bash("flaky.sh")

# correct: side-effects only, no binding needed
retry 3
  bash("flaky.sh")
```

---

## Human-in-the-loop

### confirm

Explicit approval gate. The `--yes` flag and the TUI modal satisfy it automatically.

```flux
confirm "Delete all temp files?", risk: high
  bash("rm -rf tmp/")
```

- `message` (positional string, required)
- `risk: low | medium | high | critical` — default `medium`
- Body runs only on approval; denial causes the node to error
- A `confirm` with **no body** is valid — a pure gate with no conditional action:

```flux
confirm "Proceed with production deploy?", risk: critical
```

---

## Rate limiting and debouncing

### throttle

At most `max` executions per sliding `window` (milliseconds):

```flux
throttle max: 5, window: 60000
  web_fetch($url)
```

### debounce

The body fires only after `wait` milliseconds have elapsed with no new invocation:

```flux
debounce wait: 300
  bash("rebuild.sh")
```

---

## Guards and assertions

### assert

Abort the flow if the condition is falsey. The optional second argument is the error
message:

```flux
assert $hits, "grep returned no results"
assert $gate
```

### verify

Run a command and assert its output contains a pattern. Syntax:
`verify <pattern> in <cmd-expr>`:

```flux
verify "test result: ok" in bash("cargo test")
verify "healthy" in bash("./health.sh"), message: "health check failed"
```

The pattern is a substring match. `message: "..."` overrides the default error text.

---

## Pure (no-IO) expressions

### expr — arithmetic

```flux
$total  = expr("price * qty", price: $price, qty: $qty)
$scaled = expr("round(base * 1.2, 2)", base: $base)
```

Standard precedence: `*` and `/` before `+` and `-`; parentheses for grouping.
Nesting is allowed: `round(max($a, $b) * 1.1, 2)`. All variable names used in
the formula must be declared as `name: $sym` named arguments — undeclared identifiers
are a parse error.

| Token | Meaning |
|---|---|
| `+` `-` `*` `/` | arithmetic |
| `(expr)` | grouping |
| `round(x, n)` | round to n decimal places |
| `abs(x)` | absolute value |
| `min(a, b)` | minimum |
| `max(a, b)` | maximum |

### fmt — string interpolation

```flux
$label = fmt("BTC: {price} | 24h: {change}%")
```

Substitutes from already-bound session symbols. Identical semantics to `{sym}` in
string literals, but explicit about being pure.

### jq — JSON path extraction

```flux
$price = jq(".bitcoin.usd", $raw)
$first = jq(".results[0].value", $response)
```

Path syntax: a leading `.` followed by dot-separated field names with optional `[n]`
array-index suffixes. This is a strict subset of jq — no filters, pipes, or
conditionals. Allowed forms:

- `.field`
- `.field.nested`
- `.field[0]`
- `.field[0].nested`

---

## return

```flux
return $hits      # end the flow with a value
return "done"     # literal return value
return            # return null
```

`return` is an **unconditional early exit from the entire flow**. Execution after a
`return` is unreachable. This is consistent with every mainstream language.

To conditionally exit:

```flux
when $done
  return $result
# execution continues here only if $done was falsey
bash("continue working")
```

---

## peek

`peek` reads the current in-session value of a named symbol without IO. Returns the
stored value, or `null` if the symbol is not yet bound. Useful for resumable flows.

`peek` takes the symbol name as a **string** argument:

```flux
$prev = peek("last_result")   # correct: looks up the symbol named "last_result"
$prev = peek($last_result)    # wrong: reads the value stored in $last_result
```

---

## External references (things)

External objects use `@kind(key: value)` syntax:

```flux
$author = @person(name: "timo")
$ticket = @ticket(id: "FLUX-42")
$config = @file(path: "config.yaml")
$secret = @secret(key: "ANTHROPIC_API_KEY")
```

`@` introduces either a thing-reference or an effect annotation (`@effect(...)`). It
is never an operator, so there is no ambiguity.

Built-in kinds: `context`, `file`, `person`, `ticket`, `email`, `repo`, `dataset`,
`calendar_event`, `url`, `secret`. Custom kinds: `@jira_issue(id: "PROJ-1")`.
Selector keys: `id`, `name`, `path`, `query`, `key`.

---

## Async / cross-turn

### await

Suspend until an external event arrives:

```flux
$push   = await("github.push")
$result = await("human.reply")
```

The event source is a string label. The optional `as` type is coerced leniently onto the received value.

**Implemented (P6a):** a **top-level** `await` suspends the flow for cross-turn resume — the interpreter
records the suspend point (`FlowOutcome.suspension` + a `RunEvent::Awaiting` trace), and the engine
persists it (a `suspensions` table) and resumes via `resume_flow` when the awaited input arrives next
turn; the already-run prefix is **not** re-executed. `await` is **top-level only** in v1 (the analyzer
rejects it nested inside `when`/`repeat`/`each`/… ), and the optimized `execute_plan` path does not suspend.

---

## Schema declarations

Flux-Lang has a lightweight, structural type system. Types are declared at file
scope alongside `flow` definitions — same indentation level, separated by blank
lines. They are used to constrain op call arguments, flow parameters and return
values, and model calls (e.g. `intent_extract`).

The design deliberately avoids JSON Schema verbosity. It borrows the structural
shape of TypeScript and the union syntax of GraphQL.

### Record types

```flux
type Slot
  destination: String
  date:        String
  passengers:  Number
  cabin:       String?    # ? = optional field
```

Fields are `name: Type`, one per line, indented 2 spaces. `?` suffix marks an
optional field (may be absent or null). All built-in types are valid field types;
nested record types and `List<T>` are also valid:

```flux
type RouteResult
  intent:    Intent
  slots:     CallerSlots
  response:  String
  escalated: Bool
```

### Union types

```flux
type Intent
  | book_flight
  | change_booking
  | cancel_booking
  | baggage_enquiry
  | escalate_agent
```

Each variant is a `| name` line, indented 2 spaces. Variant names are
`snake_case`. A union value is matched against with `when $x == "variant_name"`
(the runtime represents variants as strings).

### Using types in flows

Types appear in:

- Flow parameters: `flow route-call(utterance: String, caller_id: String) -> RouteResult`
- Op named arguments: `intent_extract($utt, schema: CallerSlots, intents: Intent)`
- Assert messages give richer context: `assert $slots.destination, "no destination in utterance"`

### Model-backed ops and schema

The key use case for schema declarations is constraining model calls. When an op
is declared as model-backed (e.g. `intent_extract`), passing `schema:` and
`intents:` named arguments tells the runtime to request structured output from
the model constrained to those types. The flow author writes routing logic
against typed fields; all prompt engineering lives in the op's registered spec.

```flux
$extract = intent_extract($utterance,
  schema:  CallerSlots,
  intents: Intent
)
```

This is the single LLM-cost step in the flow. Everything else — `when`, `assert`,
`confirm`, `return` — is deterministic execution with no token cost.

---

## Types (built-in)

| Syntax | Meaning |
|---|---|
| `String` | UTF-8 text |
| `Number` | 64-bit float |
| `Bool` | boolean |
| `Any` | top type |
| `List<T>` | homogeneous list |
| `Ticket`, `PushEvent`, … | named / registered type |

Type annotations are optional everywhere. The runtime does not enforce them today;
they are documentation and are preserved in the AST.

---

## Edge cases

| Situation | Behaviour |
|---|---|
| `each` over an empty list | `$collect` bound to `[]`; body never runs |
| `each` with no `-> $collect` | results discarded; body still runs |
| `parallel` with one branch | valid; degenerates to sequential bind |
| `parallel` with zero branches | parse error |
| `pipe` with one step | valid; equivalent to a bare call |
| `pipe` with zero steps | parse error |
| `confirm` with no body | valid; pure approval gate |
| `retry` wrapping `confirm` | denial is fatal — not retried |
| Flow with empty body | parse error |
| `watch` `until` | evaluated at start of each iteration, before the body |
| `repeat` `until` | `until` must be the first line of the body |

---

## Complete examples

### eval-smoke.flux

```flux
flow eval-smoke
  $baseline   = eval_run("mock")
  $sessions   = eval_sessions($baseline)
  $mined      = painpoints_collect($sessions)
  $candidates = improvements_aggregate($mined, [])
  return $candidates
```

### improve.flux (abridged)

```flux
flow improve -> EvalReport
  $baseline = eval_run({
    adapter:  "local",
    dir:      "suites",
    flux_bin: "target/debug/flux",
    trials:   3
  })
  $sessions = eval_sessions($baseline)
  $digest   = sessions_digest($sessions)

  parallel
    $mined:
      painpoints_collect($sessions)
    $reviewed:
      task("reviewer", """
        Review these flux eval sessions for failure modes.
        Sessions:
        {digest}

        Return ONLY a JSON array of findings.
      """)

  $candidates = improvements_aggregate($mined, $reviewed)

  repeat 3
    until $done
    $tasks    = task("planner", "Turn these candidates into AT MOST 2 tasks:\n{candidates}")
    $snapshot = git_snapshot()
    change_implement($tasks, 2)
    $gate     = gate_check()

    when $gate
      $candidate = eval_run({adapter: "local", dir: "suites", trials: 3})
      when score_compare($baseline, $candidate)
        git_stage(["."])
        git_commit("improve: adopt candidate")
        $baseline = eval_adopt($candidate)
      else
        git_revert($snapshot)
    else
      git_revert($snapshot)

    $done       = candidates_empty($candidates)
    $candidates = candidates_advance($candidates)

  return $baseline
```

---

## Toolchain

- `parse.rs` — `parse(src: &str) -> Result<DraftAst>` (a single flow). Hand-written, indentation-sensitive
  recursive descent; malformed input returns `FlowError::Parse` (never panics). Accepts both the canonical
  `do <op> <args>` and inline `op(args)` call forms, and reads the `@json` escape back.
- `format.rs` — `format(ast: &DraftAst) -> String`. Canonical emitter, always 2-space indentation,
  brace-free indentation blocks; emits `@json` for nodes without a native form. Separate from `render.rs`
  (a lossy one-way terminal display tree).

Round-trip invariant (**holds today**): `parse(&format(&ast)) == ast` for every `DraftAst`.

`flux run <app.flux>` runs a multi-agent program through the `flux-app` host (see
[`../../../docs/designs/flux-lang-evolution.md`](../../../docs/designs/flux-lang-evolution.md) §6); a
`fluxlang compile` subcommand onto `parse` is the one remaining toolchain step.

---

## Relationship to the JSON wire format

| Property | Text (`.flux`) | JSON (wire) |
|---|---|---|
| Who writes it | humans, editors | the LLM / `emit_plan` |
| Where it lives | `examples/`, user repos | `.flux/flows/`, session storage |
| Round-trips | via `parse` + `format` | via `serde_json` |
| Comments | yes (`#`) | no |
| Multi-line strings | yes (`"""..."""`) | escaped `\n` in JSON string |
| Named args | yes (`key: val`) | positional array only |
| Type annotations | yes (params/returns) | yes (same `TypeRef` serde) |
| String interpolation | `{sym}`, escape `{{` `}}` | same `{sym}` inside JSON strings |

The JSON format remains the authoritative wire format. The text format is a
programmer-facing projection — nothing it can express is absent from the JSON format.
